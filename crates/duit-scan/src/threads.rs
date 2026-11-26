//! A scan with several threads, `-t` in ncdu. Unlike ncdu, whose threads each report to the sink,
//! the threads read and `stat` whole directories and send them to the calling thread, which alone
//! reports them to the sink. A [`Sink`] then needs no locks, and the progress is counted in one
//! place.

use std::ffi::CString;
use std::io;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use duit_model::Sink;
use rustix::fs::Dir;

use crate::scan::{Entry, add_unreadable, classify, open_directory};
use crate::{DirectoryPatterns, ScanError, ScanOptions};

/// How many directories that were read wait for the calling thread before the threads that read
/// more wait too, as the size of the queue in `State` in `scan.zig` bounds the directories that
/// wait there.
const LISTINGS_MAX: usize = 16;

/// How long the calling thread waits for a directory before the sink can handle an event, so that
/// the progress is drawn and its keys are read while a large directory is read.
const IDLE_INTERVAL: Duration = Duration::from_millis(100);

/// A directory for a thread to read.
struct Job<'a> {
    location: Location,
    device: u64,
    patterns: DirectoryPatterns<'a>,
    /// The index of the directory in [`Directories`].
    index: usize,
}

/// Where the directory of a [`Job`] is.
enum Location {
    /// The root, which is opened before the threads start.
    Open(Dir),
    /// A subdirectory, by its name in its parent.
    At(Arc<Dir>, CString),
}

/// The entries of a directory, as a thread read them.
struct Listing<'a> {
    /// The index of the directory in [`Directories`].
    index: usize,
    /// The entries, in the order of `readdir`. A directory among them is not open.
    entries: Vec<(CString, Entry<'a>)>,
    /// Whether the directory could not be opened, or `readdir` failed before the end.
    read_error: bool,
    /// The directory, which its subdirectories are opened in, or `None` if it could not be opened.
    directory: Option<Arc<Dir>>,
}

/// The directories that wait for a thread, taken last in first out as from `State` in `scan.zig`,
/// and whether the scan is over.
struct Queue<'a> {
    jobs: Mutex<Jobs<'a>>,
    ready: Condvar,
}

struct Jobs<'a> {
    waiting: Vec<Job<'a>>,
    closed: bool,
}

impl<'a> Queue<'a> {
    fn new() -> Self {
        Self {
            jobs: Mutex::new(Jobs {
                waiting: Vec::new(),
                closed: false,
            }),
            ready: Condvar::new(),
        }
    }

    // A thread that panics while it holds the lock leaves the jobs as they were, so a poisoned
    // lock is used as it is.
    fn lock(&self) -> MutexGuard<'_, Jobs<'a>> {
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn push(&self, job: Job<'a>) {
        self.lock().waiting.push(job);
        self.ready.notify_one();
    }

    /// The next directory to read, or `None` once the scan is over.
    fn pop(&self) -> Option<Job<'a>> {
        let mut jobs = self.lock();
        loop {
            if jobs.closed {
                return None;
            }
            if let Some(job) = jobs.waiting.pop() {
                return Some(job);
            }
            jobs = self
                .ready
                .wait(jobs)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// End the scan, so that every thread stops once it has sent the directory it reads.
    fn close(&self) {
        let mut jobs = self.lock();
        jobs.closed = true;
        jobs.waiting.clear();
        drop(jobs);
        self.ready.notify_all();
    }
}

/// A directory that was reported to the sink and is not finished.
struct Open<D> {
    sink: D,
    /// The index of the parent, or `None` for the root.
    parent: Option<usize>,
    /// The listing of the directory, until it is reported, and its subdirectories that are not
    /// finished.
    waiting: usize,
}

/// The directories that are not finished, by index. The index of a finished directory is used
/// again, so that the table is as large as the directories that are open at once.
struct Directories<D> {
    open: Vec<Option<Open<D>>>,
    free: Vec<usize>,
}

impl<D> Directories<D> {
    fn insert(&mut self, directory: Open<D>) -> usize {
        if let Some(index) = self.free.pop()
            && let Some(slot) = self.open.get_mut(index)
        {
            *slot = Some(directory);
            return index;
        }
        self.open.push(Some(directory));
        self.open.len() - 1
    }

    /// Finish the directory at `index` if it is done, and the directories above it that are done
    /// then. Return whether the root is finished.
    fn finish<S: Sink<Directory = D>>(
        &mut self,
        sink: &mut S,
        mut index: usize,
    ) -> io::Result<bool> {
        loop {
            let slot = self.open.get_mut(index).ok_or_else(missing)?;
            if slot.as_ref().is_some_and(|directory| directory.waiting > 0) {
                return Ok(false);
            }
            let directory = slot.take().ok_or_else(missing)?;
            self.free.push(index);
            let Some(parent_index) = directory.parent else {
                sink.finish_directory(directory.sink, None)?;
                return Ok(true);
            };
            let parent = self
                .open
                .get_mut(parent_index)
                .and_then(Option::as_mut)
                .ok_or_else(missing)?;
            sink.finish_directory(directory.sink, Some(&mut parent.sink))?;
            parent.waiting = parent.waiting.saturating_sub(1);
            index = parent_index;
        }
    }
}

/// The error for a directory that the threads report but that is not in [`Directories`], which
/// does not happen.
fn missing() -> io::Error {
    io::Error::other("a directory of the scan is missing")
}

/// Read the tree below `root`, whose `entries` are open and on `device`, with `options.threads`
/// threads, and report it to `sink`, whose handle of the root is `root`.
pub(crate) fn walk<S: Sink>(
    sink: &mut S,
    root: S::Directory,
    entries: Dir,
    device: u64,
    patterns: DirectoryPatterns<'_>,
    options: ScanOptions,
) -> Result<(), ScanError> {
    let queue = Queue::new();
    queue.push(Job {
        location: Location::Open(entries),
        device,
        patterns,
        index: 0,
    });
    let mut directories = Directories {
        open: vec![Some(Open {
            sink: root,
            parent: None,
            waiting: 1,
        })],
        free: Vec::new(),
    };
    thread::scope(|scope| {
        let (sender, receiver) = mpsc::sync_channel(LISTINGS_MAX);
        for _ in 0..options.threads {
            let sender = sender.clone();
            let queue = &queue;
            let spawned = thread::Builder::new()
                .spawn_scoped(scope, move || read_directories(queue, &sender, options));
            if let Err(error) = spawned {
                queue.close();
                return Err(ScanError::Spawn(error));
            }
        }
        drop(sender);
        let received = receive(sink, &receiver, &queue, &mut directories);
        // A thread that waits to send stops once the receiver is dropped.
        drop(receiver);
        queue.close();
        received.map_err(ScanError::Sink)
    })
}

/// Report the directories that the threads send to `sink` until the root is finished.
fn receive<'a, S: Sink>(
    sink: &mut S,
    listings: &Receiver<io::Result<Listing<'a>>>,
    queue: &Queue<'a>,
    directories: &mut Directories<S::Directory>,
) -> io::Result<()> {
    loop {
        let listing = match listings.recv_timeout(IDLE_INTERVAL) {
            Ok(listing) => listing?,
            Err(RecvTimeoutError::Timeout) => {
                sink.idle()?;
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(io::Error::other("the threads of the scan stopped"));
            }
        };
        if report(sink, listing, queue, directories)? {
            return Ok(());
        }
    }
}

/// Report the entries of `listing` to `sink`, queue its subdirectories, and finish the directories
/// that are done then. Return whether the root is finished.
fn report<'a, S: Sink>(
    sink: &mut S,
    listing: Listing<'a>,
    queue: &Queue<'a>,
    directories: &mut Directories<S::Directory>,
) -> io::Result<bool> {
    let Listing {
        index,
        entries,
        read_error,
        directory: opened,
    } = listing;
    let mut directory = directories
        .open
        .get_mut(index)
        .and_then(Option::take)
        .ok_or_else(missing)?;
    for (name, entry) in entries {
        let bytes = name.to_bytes();
        match entry {
            Entry::Special(special) => sink.add_special(&mut directory.sink, bytes, special)?,
            Entry::Stat(stat) => sink.add_stat(&mut directory.sink, bytes, &stat)?,
            Entry::Unreadable(stat) => add_unreadable(sink, &mut directory.sink, bytes, &stat)?,
            Entry::Directory { stat, patterns, .. } => {
                // A listing of a directory that could not be opened has no entries.
                let Some(parent) = &opened else {
                    add_unreadable(sink, &mut directory.sink, bytes, &stat)?;
                    continue;
                };
                let child = sink.add_directory(&mut directory.sink, bytes, &stat)?;
                let child_index = directories.insert(Open {
                    sink: child,
                    parent: Some(index),
                    waiting: 1,
                });
                directory.waiting += 1;
                queue.push(Job {
                    location: Location::At(Arc::clone(parent), name),
                    device: stat.device,
                    patterns,
                    index: child_index,
                });
            }
        }
    }
    if read_error {
        sink.set_read_error(&mut directory.sink)?;
    }
    directory.waiting = directory.waiting.saturating_sub(1);
    let slot = directories.open.get_mut(index).ok_or_else(missing)?;
    *slot = Some(directory);
    directories.finish(sink, index)
}

/// Read the directories of `queue` and send them to `listings` until the scan is over, `run` of
/// `Thread` in `scan.zig`.
fn read_directories<'a>(
    queue: &Queue<'a>,
    listings: &SyncSender<io::Result<Listing<'a>>>,
    options: ScanOptions,
) {
    while let Some(job) = queue.pop() {
        if listings.send(read(job, options)).is_err() {
            return;
        }
    }
}

/// Read and classify the entries of the directory of `job`.
fn read(job: Job<'_>, options: ScanOptions) -> io::Result<Listing<'_>> {
    let mut listing = Listing {
        index: job.index,
        entries: Vec::new(),
        read_error: false,
        directory: None,
    };
    let mut entries = match job.location {
        Location::Open(entries) => entries,
        Location::At(parent, name) => {
            let Ok(entries) = open_directory(parent.fd()?, &name) else {
                listing.read_error = true;
                return Ok(listing);
            };
            entries
        }
    };
    loop {
        match entries.read() {
            Some(Ok(entry)) => {
                let name = entry.file_name();
                if name == c"." || name == c".." {
                    continue;
                }
                let mut entry_kind =
                    classify(entries.fd()?, job.device, &job.patterns, name, options)?;
                // The thread that reads a subdirectory opens it again, so that the directories
                // that wait do not hold a descriptor each.
                if let Entry::Directory { entries, .. } = &mut entry_kind {
                    *entries = None;
                }
                listing.entries.push((name.to_owned(), entry_kind));
            }
            // As in a scan with one thread, the directory that failed to read is marked.
            Some(Err(_)) => {
                listing.read_error = true;
                break;
            }
            None => break,
        }
    }
    listing.directory = Some(Arc::new(entries));
    Ok(listing)
}
