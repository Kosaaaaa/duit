use std::collections::HashMap;
use std::io;
use std::mem;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use duit_model::{DirectoryPath, Sink, Special, Stat, ThreadDirectories};
use rustix::fs::Dir;

use crate::scan::{Entry, Frame, add_unreadable, classify, open_directory};
use crate::{DirectoryPatterns, ScanError, ScanOptions};

const LISTINGS_MAX: usize = 16;

const PART_ENTRIES: usize = 64;

/// The most directories that wait for a thread, `QUEUE_SIZE` in `scan.zig`. A thread that finds a
/// subdirectory while the queue is full reads it itself before the rest of its directory.
const QUEUE_MAX: usize = 16;

const IDLE_INTERVAL: Duration = Duration::from_millis(100);

/// A directory for a thread to read, opened by the thread that found it.
struct Job<'a> {
    entries: Dir,
    device: u64,
    patterns: DirectoryPatterns<'a>,
    index: usize,
    path: Arc<DirectoryPath>,
}

/// An entry as a thread sends it to the calling thread.
enum Item {
    Special(Special),
    Stat(Stat),
    Unreadable(Stat),
    /// A subdirectory, whose own parts follow under `index`.
    Directory {
        stat: Stat,
        index: usize,
    },
}

/// Entries of the directory `index`, in the order of `readdir`, with their names one after the
/// other, each followed by its NUL byte.
struct Listing {
    index: usize,
    items: Vec<Item>,
    names: Vec<u8>,
    last: bool,
    read_error: bool,
}

impl Listing {
    fn new(index: usize) -> Self {
        Self {
            index,
            items: Vec::new(),
            names: Vec::new(),
            last: false,
            read_error: false,
        }
    }
}

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

    fn lock_ignoring_poison(&self) -> MutexGuard<'_, Jobs<'a>> {
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait_ignoring_poison<'guard>(
        &self,
        jobs: MutexGuard<'guard, Jobs<'a>>,
    ) -> MutexGuard<'guard, Jobs<'a>> {
        self.ready
            .wait(jobs)
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Queue `job` for another thread, or give it back when the queue is full, `tryPush` in
    /// `scan.zig`.
    fn try_push(&self, job: Job<'a>) -> Option<Job<'a>> {
        let mut jobs = self.lock_ignoring_poison();
        if jobs.waiting.len() >= QUEUE_MAX {
            return Some(job);
        }
        jobs.waiting.push(job);
        drop(jobs);
        self.ready.notify_one();
        None
    }

    fn pop(&self) -> Option<Job<'a>> {
        let mut jobs = self.lock_ignoring_poison();
        loop {
            if jobs.closed {
                return None;
            }
            if let Some(job) = jobs.waiting.pop() {
                return Some(job);
            }
            jobs = self.wait_ignoring_poison(jobs);
        }
    }

    fn close(&self) {
        let mut jobs = self.lock_ignoring_poison();
        jobs.closed = true;
        jobs.waiting.clear();
        drop(jobs);
        self.ready.notify_all();
    }
}

struct Open<D> {
    sink: D,
    parent: Option<usize>,
    listing_pending: bool,
    unfinished_subdirectories: usize,
}

impl<D> Open<D> {
    fn new(sink: D, parent: Option<usize>) -> Self {
        Self {
            sink,
            parent,
            listing_pending: true,
            unfinished_subdirectories: 0,
        }
    }

    fn is_finished(&self) -> bool {
        !self.listing_pending && self.unfinished_subdirectories == 0
    }
}

/// The directories that the calling thread reported and did not finish yet, by the index that
/// the thread that found each one gave it.
struct Directories<D> {
    open: HashMap<usize, Open<D>>,
    /// Parts of directories that are not reported yet: a thread queues a subdirectory before it
    /// sends the part with it, and another thread can send parts of the subdirectory first.
    early: HashMap<usize, Vec<Listing>>,
}

impl<D> Directories<D> {
    fn finish<S: Sink<Directory = D>>(
        &mut self,
        sink: &mut S,
        mut index: usize,
    ) -> io::Result<bool> {
        loop {
            let directory = self.open.get(&index).ok_or_else(missing)?;
            if !directory.is_finished() {
                return Ok(false);
            }
            let directory = self.open.remove(&index).ok_or_else(missing)?;
            let Some(parent_index) = directory.parent else {
                sink.finish_directory(directory.sink, None)?;
                return Ok(true);
            };
            let parent = self.open.get_mut(&parent_index).ok_or_else(missing)?;
            sink.finish_directory(directory.sink, Some(&mut parent.sink))?;
            parent.unfinished_subdirectories = parent.unfinished_subdirectories.saturating_sub(1);
            index = parent_index;
        }
    }
}

fn missing() -> io::Error {
    io::Error::other("a directory of the scan is missing")
}

pub(crate) fn walk<S: Sink>(
    sink: &mut S,
    frame: Frame<'_, S::Directory>,
    path: Arc<DirectoryPath>,
    threads: &ThreadDirectories,
    options: ScanOptions,
) -> Result<(), ScanError> {
    let queue = Queue::new();
    let indexes = AtomicUsize::new(1);
    let _ = queue.try_push(Job {
        entries: frame.entries,
        device: frame.device,
        patterns: frame.patterns,
        index: 0,
        path,
    });
    let mut directories = Directories {
        open: HashMap::from([(0, Open::new(frame.sink, None))]),
        early: HashMap::new(),
    };
    thread::scope(|scope| {
        let (sender, receiver) = mpsc::sync_channel(LISTINGS_MAX);
        for number in 0..options.threads {
            let sender = sender.clone();
            let queue = &queue;
            let indexes = &indexes;
            let spawned = thread::Builder::new().spawn_scoped(scope, move || {
                let reader = Reader {
                    thread: number,
                    queue,
                    indexes,
                    listings: &sender,
                    threads,
                    options,
                };
                reader.run();
            });
            if let Err(error) = spawned {
                shut_down(receiver, queue);
                return Err(ScanError::Spawn(error));
            }
        }
        drop(sender);
        let received = receive(sink, &receiver, &mut directories);
        shut_down(receiver, &queue);
        received.map_err(ScanError::Sink)
    })
}

fn shut_down(listings: Receiver<io::Result<Listing>>, queue: &Queue<'_>) {
    drop(listings);
    queue.close();
}

fn receive<S: Sink>(
    sink: &mut S,
    listings: &Receiver<io::Result<Listing>>,
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
        if report(sink, listing, directories)? {
            return Ok(());
        }
    }
}

fn report<S: Sink>(
    sink: &mut S,
    listing: Listing,
    directories: &mut Directories<S::Directory>,
) -> io::Result<bool> {
    let mut listings = vec![listing];
    while let Some(listing) = listings.pop() {
        let Listing {
            index,
            items,
            names,
            last,
            read_error,
        } = listing;
        let Some(mut directory) = directories.open.remove(&index) else {
            directories.early.entry(index).or_default().push(Listing {
                index,
                items,
                names,
                last,
                read_error,
            });
            continue;
        };
        for (item, name) in items
            .into_iter()
            .zip(names.split_inclusive(|&byte| byte == 0))
        {
            let name = name.strip_suffix(b"\0").unwrap_or(name);
            match item {
                Item::Special(special) => sink.add_special(&mut directory.sink, name, special)?,
                Item::Stat(stat) => sink.add_stat(&mut directory.sink, name, &stat)?,
                Item::Unreadable(stat) => add_unreadable(sink, &mut directory.sink, name, &stat)?,
                Item::Directory { stat, index: child } => {
                    let child_sink = sink.add_directory(&mut directory.sink, name, &stat)?;
                    directories
                        .open
                        .insert(child, Open::new(child_sink, Some(index)));
                    directory.unfinished_subdirectories += 1;
                    if let Some(early) = directories.early.remove(&child) {
                        // In the order they came, after the rest of this part.
                        listings.splice(0..0, early.into_iter().rev());
                    }
                }
            }
        }
        if read_error {
            sink.set_read_error(&mut directory.sink)?;
        }
        if last {
            directory.listing_pending = false;
        }
        directories.open.insert(index, directory);
        if directories.finish(sink, index)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A thread that reads directories: one from the queue, then depth-first each subdirectory that
/// does not fit in the queue, as each ncdu thread keeps a stack of its own (`Thread.run` in
/// `scan.zig`).
struct Reader<'scope, 'a> {
    thread: usize,
    queue: &'scope Queue<'a>,
    indexes: &'scope AtomicUsize,
    listings: &'scope SyncSender<io::Result<Listing>>,
    threads: &'scope ThreadDirectories,
    options: ScanOptions,
}

/// A directory that a [`Reader`] reads, with its entries that it did not send yet.
struct Reading<'a> {
    job: Job<'a>,
    part: Listing,
}

impl<'a> Reading<'a> {
    fn new(job: Job<'a>) -> Self {
        let part = Listing::new(job.index);
        Self { job, part }
    }
}

impl<'a> Reader<'_, 'a> {
    fn run(&self) {
        while let Some(job) = self.queue.pop() {
            let read = self.read(job);
            self.threads.set(self.thread, None);
            if !read {
                return;
            }
        }
    }

    fn send(&self, listing: io::Result<Listing>) -> bool {
        self.listings.send(listing).is_ok()
    }

    /// Read `job` and the subdirectories that do not fit in the queue, depth-first. `false` once
    /// the calling thread stopped.
    fn read(&self, job: Job<'a>) -> bool {
        self.threads.set(self.thread, Some(Arc::clone(&job.path)));
        let mut stack = vec![Reading::new(job)];
        while let Some(reading) = stack.last_mut() {
            let next = match reading.job.entries.read() {
                Some(Ok(entry)) => Some(entry),
                Some(Err(_)) => {
                    reading.part.read_error = true;
                    None
                }
                None => None,
            };
            let Some(entry) = next else {
                if let Some(Reading { mut part, .. }) = stack.pop() {
                    part.last = true;
                    if !self.send(Ok(part)) {
                        return false;
                    }
                }
                let parent = stack.last().map(|parent| Arc::clone(&parent.job.path));
                self.threads.set(self.thread, parent);
                continue;
            };
            let name = entry.file_name();
            if name == c"." || name == c".." {
                continue;
            }
            let job = &reading.job;
            let parent = match job.entries.fd() {
                Ok(parent) => parent,
                Err(error) => {
                    self.send(Err(error.into()));
                    return false;
                }
            };
            let classified = match classify(parent, job.device, &job.patterns, name, self.options) {
                Ok(classified) => classified,
                Err(error) => {
                    self.send(Err(error));
                    return false;
                }
            };
            let mut kept = None;
            let item = match classified {
                Entry::Special(special) => Item::Special(special),
                Entry::Stat(stat) => Item::Stat(stat),
                Entry::Unreadable(stat) => Item::Unreadable(stat),
                Entry::Directory {
                    stat,
                    patterns,
                    entries,
                } => match entries.map_or_else(|| open_directory(parent, name), Ok) {
                    Ok(entries) => {
                        let index = self.indexes.fetch_add(1, Ordering::Relaxed);
                        let subdirectory = Job {
                            entries,
                            device: stat.device,
                            patterns,
                            index,
                            path: DirectoryPath::child(&job.path, name.to_bytes()),
                        };
                        kept = self.queue.try_push(subdirectory);
                        Item::Directory { stat, index }
                    }
                    Err(_) => Item::Unreadable(stat),
                },
            };
            reading.part.items.push(item);
            reading
                .part
                .names
                .extend_from_slice(name.to_bytes_with_nul());
            if kept.is_none() && reading.part.items.len() < PART_ENTRIES {
                continue;
            }
            let part = mem::replace(&mut reading.part, Listing::new(reading.job.index));
            if !self.send(Ok(part)) {
                return false;
            }
            // A subdirectory that no thread can take now is read next, as in ncdu, once the
            // calling thread has it.
            if let Some(subdirectory) = kept {
                self.threads
                    .set(self.thread, Some(Arc::clone(&subdirectory.path)));
                stack.push(Reading::new(subdirectory));
            }
        }
        true
    }
}
