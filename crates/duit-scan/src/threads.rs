//! A scan with several threads, `-t` in ncdu. Unlike ncdu, whose threads each report to the sink,
//! the threads read and `stat` directories and send their entries in parts to the calling thread,
//! which alone reports them to the sink. A [`Sink`] then needs no locks, and the progress is
//! counted in one place.

use std::ffi::CString;
use std::io;
use std::mem;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use duit_model::{DirectoryPath, Sink, ThreadDirectories};
use rustix::fs::Dir;
use rustix::io::fcntl_dupfd_cloexec;

use crate::scan::{Entry, Frame, add_unreadable, classify, open_directory};
use crate::{DirectoryPatterns, ScanError, ScanOptions};

/// How many parts of directories that were read wait for the calling thread before the threads
/// that read more wait too, as the size of the queue in `State` in `scan.zig` bounds the
/// directories that wait there.
const LISTINGS_MAX: usize = 16;

/// How many entries a part of a directory holds at most. The threads send a directory in parts,
/// so that the channel holds at most [`LISTINGS_MAX`] times as many entries, however large a
/// directory is, where ncdu reports each entry as it reads it.
const PART_ENTRIES: usize = 64;

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
    /// The path that the progress shows while a thread reads the directory.
    path: Arc<DirectoryPath>,
}

/// Where the directory of a [`Job`] is.
enum Location {
    /// The root, which is opened before the threads start.
    Open(Dir),
    /// A subdirectory, by its name in its parent.
    At(Arc<Parent>, CString),
}

/// A directory that its subdirectories are opened in.
enum Parent {
    /// A copy of the descriptor of a directory, for the subdirectories in the parts that are sent
    /// while the thread reads on.
    Copy(OwnedFd),
    /// A directory that was read to the end.
    Read(Dir),
}

impl Parent {
    fn fd(&self) -> io::Result<BorrowedFd<'_>> {
        match self {
            Self::Copy(fd) => Ok(fd.as_fd()),
            Self::Read(directory) => Ok(directory.fd()?),
        }
    }
}

/// A part of the entries of a directory, as a thread read them. The parts of a directory follow
/// each other in the order of `readdir`, and the last one ends the directory.
struct Listing<'a> {
    /// The index of the directory in [`Directories`].
    index: usize,
    /// At most [`PART_ENTRIES`] entries, in the order of `readdir`. A directory among them is not
    /// open.
    entries: Vec<(CString, Entry<'a>)>,
    /// Whether this is the last part of the directory.
    last: bool,
    /// Whether the directory could not be opened, or `readdir` failed before the end, in the last
    /// part.
    read_error: bool,
    /// The directory, which the subdirectories among the entries are opened in, or `None` if they
    /// cannot be opened.
    directory: Option<Arc<Parent>>,
    /// The path of the directory, which the paths of its subdirectories are joined to.
    path: Arc<DirectoryPath>,
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
    /// The listing of the directory, until its last part is reported, and its subdirectories
    /// that are not finished.
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

/// Read the tree below the root of `frame`, at `path`, with `options.threads` threads, and report
/// it to `sink`. Each thread sets the directory it reads in `threads`.
pub(crate) fn walk<S: Sink>(
    sink: &mut S,
    frame: Frame<'_, S::Directory>,
    path: Arc<DirectoryPath>,
    threads: &ThreadDirectories,
    options: ScanOptions,
) -> Result<(), ScanError> {
    let queue = Queue::new();
    queue.push(Job {
        location: Location::Open(frame.entries),
        device: frame.device,
        patterns: frame.patterns,
        index: 0,
        path,
    });
    let mut directories = Directories {
        open: vec![Some(Open {
            sink: frame.sink,
            parent: None,
            waiting: 1,
        })],
        free: Vec::new(),
    };
    thread::scope(|scope| {
        let (sender, receiver) = mpsc::sync_channel(LISTINGS_MAX);
        for number in 0..options.threads {
            let sender = sender.clone();
            let queue = &queue;
            let spawned = thread::Builder::new().spawn_scoped(scope, move || {
                read_directories(number, queue, &sender, threads, options);
            });
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
/// that are done then, once it is the last part of its directory. Return whether the root is
/// finished.
fn report<'a, S: Sink>(
    sink: &mut S,
    listing: Listing<'a>,
    queue: &Queue<'a>,
    directories: &mut Directories<S::Directory>,
) -> io::Result<bool> {
    let Listing {
        index,
        entries,
        last,
        read_error,
        directory: opened,
        path,
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
                // A part whose directory has no descriptor for its subdirectories reports them as
                // directories that cannot be opened.
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
                    path: DirectoryPath::child(&path, bytes),
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
    if last {
        directory.waiting = directory.waiting.saturating_sub(1);
    }
    let slot = directories.open.get_mut(index).ok_or_else(missing)?;
    *slot = Some(directory);
    directories.finish(sink, index)
}

/// Read the directories of `queue` and send them to `listings` until the scan is over, `run` of
/// `Thread` in `scan.zig`. The progress shows the directory from when `thread` takes it until its
/// last part is sent, as ncdu shows it until its entries are reported, and none while `thread`
/// waits.
fn read_directories<'a>(
    thread: usize,
    queue: &Queue<'a>,
    listings: &SyncSender<io::Result<Listing<'a>>>,
    threads: &ThreadDirectories,
    options: ScanOptions,
) {
    while let Some(job) = queue.pop() {
        threads.set(thread, Some(Arc::clone(&job.path)));
        let sent = read(job, options, &mut |listing| listings.send(listing).is_ok());
        threads.set(thread, None);
        if !sent {
            return;
        }
    }
}

/// Read and classify the entries of the directory of `job`, and pass them to `send` in parts.
/// Return `false` once `send` does, as the calling thread takes no more.
fn read<'a>(
    job: Job<'a>,
    options: ScanOptions,
    send: &mut dyn FnMut(io::Result<Listing<'a>>) -> bool,
) -> bool {
    let mut listing = Listing {
        index: job.index,
        entries: Vec::new(),
        last: true,
        read_error: false,
        directory: None,
        path: job.path,
    };
    let opened = match job.location {
        Location::Open(entries) => Ok(entries),
        Location::At(parent, name) => match parent.fd() {
            Ok(parent) => open_directory(parent, &name),
            Err(error) => return send(Err(error)),
        },
    };
    let Ok(mut entries) = opened else {
        listing.read_error = true;
        return send(Ok(listing));
    };
    // Only a directory with subdirectories in a part before the last needs a copy of its
    // descriptor, so that most directories take no more than one.
    let mut copy = None;
    let mut part_directories = false;
    loop {
        match entries.read() {
            Some(Ok(entry)) => {
                let name = entry.file_name();
                if name == c"." || name == c".." {
                    continue;
                }
                let classified = entries
                    .fd()
                    .map_err(io::Error::from)
                    .and_then(|parent| classify(parent, job.device, &job.patterns, name, options));
                let mut entry_kind = match classified {
                    Ok(entry_kind) => entry_kind,
                    Err(error) => return send(Err(error)),
                };
                // The thread that reads a subdirectory opens it again, so that the directories
                // that wait do not hold a descriptor each.
                if let Entry::Directory { entries, .. } = &mut entry_kind {
                    *entries = None;
                    part_directories = true;
                }
                listing.entries.push((name.to_owned(), entry_kind));
                if listing.entries.len() == PART_ENTRIES {
                    // Without a copy, the subdirectories of the part cannot be opened, as when
                    // `openat` fails for lack of descriptors.
                    if part_directories && copy.is_none() {
                        copy = entries
                            .fd()
                            .and_then(|fd| fcntl_dupfd_cloexec(fd, 0))
                            .ok()
                            .map(|fd| Arc::new(Parent::Copy(fd)));
                    }
                    part_directories = false;
                    let part = Listing {
                        index: listing.index,
                        entries: mem::take(&mut listing.entries),
                        last: false,
                        read_error: false,
                        directory: copy.clone(),
                        path: Arc::clone(&listing.path),
                    };
                    if !send(Ok(part)) {
                        return false;
                    }
                }
            }
            // As in a scan with one thread, the directory that failed to read is marked.
            Some(Err(_)) => {
                listing.read_error = true;
                break;
            }
            None => break,
        }
    }
    listing.directory = Some(copy.unwrap_or_else(|| Arc::new(Parent::Read(entries))));
    send(Ok(listing))
}
