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

const LISTINGS_MAX: usize = 16;

const PART_ENTRIES: usize = 64;

const IDLE_INTERVAL: Duration = Duration::from_millis(100);

struct Job<'a> {
    location: Location,
    device: u64,
    patterns: DirectoryPatterns<'a>,
    index: usize,
    path: Arc<DirectoryPath>,
}

enum Location {
    Open(Dir),
    At(Arc<Parent>, CString),
}

enum Parent {
    Copy(OwnedFd),
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

enum Subdirectories {
    Unreadable,
    At(Arc<Parent>),
}

struct Listing<'a> {
    index: usize,
    entries: Vec<(CString, Entry<'a>)>,
    last: bool,
    read_error: bool,
    subdirectories: Subdirectories,
    path: Arc<DirectoryPath>,
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

    fn push(&self, job: Job<'a>) {
        self.lock_ignoring_poison().waiting.push(job);
        self.ready.notify_one();
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

    fn finish<S: Sink<Directory = D>>(
        &mut self,
        sink: &mut S,
        mut index: usize,
    ) -> io::Result<bool> {
        loop {
            let slot = self.open.get_mut(index).ok_or_else(missing)?;
            if slot
                .as_ref()
                .is_some_and(|directory| !directory.is_finished())
            {
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
    queue.push(Job {
        location: Location::Open(frame.entries),
        device: frame.device,
        patterns: frame.patterns,
        index: 0,
        path,
    });
    let mut directories = Directories {
        open: vec![Some(Open::new(frame.sink, None))],
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
                shut_down(receiver, queue);
                return Err(ScanError::Spawn(error));
            }
        }
        drop(sender);
        let received = receive(sink, &receiver, &queue, &mut directories);
        shut_down(receiver, &queue);
        received.map_err(ScanError::Sink)
    })
}

fn shut_down<'a>(listings: Receiver<io::Result<Listing<'a>>>, queue: &Queue<'a>) {
    drop(listings);
    queue.close();
}

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
        subdirectories,
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
                let parent = match &subdirectories {
                    Subdirectories::At(parent) => parent,
                    Subdirectories::Unreadable => {
                        add_unreadable(sink, &mut directory.sink, bytes, &stat)?;
                        continue;
                    }
                };
                let child = sink.add_directory(&mut directory.sink, bytes, &stat)?;
                let child_index = directories.insert(Open::new(child, Some(index)));
                directory.unfinished_subdirectories += 1;
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
        directory.listing_pending = false;
    }
    let slot = directories.open.get_mut(index).ok_or_else(missing)?;
    *slot = Some(directory);
    directories.finish(sink, index)
}

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

struct ReadDirectory {
    entries: Dir,
    copy: Option<Arc<Parent>>,
}

impl ReadDirectory {
    fn subdirectories_of_part(&mut self, has_directories: bool) -> Subdirectories {
        if has_directories && self.copy.is_none() {
            self.copy = self
                .entries
                .fd()
                .and_then(|fd| fcntl_dupfd_cloexec(fd, 0))
                .ok()
                .map(|fd| Arc::new(Parent::Copy(fd)));
        }
        match &self.copy {
            Some(copy) => Subdirectories::At(Arc::clone(copy)),
            None => Subdirectories::Unreadable,
        }
    }

    fn into_parent(self) -> Arc<Parent> {
        self.copy
            .unwrap_or_else(|| Arc::new(Parent::Read(self.entries)))
    }
}

fn close_if_directory(entry: &mut Entry<'_>) -> bool {
    match entry {
        Entry::Directory { entries, .. } => {
            *entries = None;
            true
        }
        Entry::Special(_) | Entry::Stat(_) | Entry::Unreadable(_) => false,
    }
}

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
        subdirectories: Subdirectories::Unreadable,
        path: job.path,
    };
    let opened = match job.location {
        Location::Open(entries) => Ok(entries),
        Location::At(parent, name) => match parent.fd() {
            Ok(parent) => open_directory(parent, &name),
            Err(error) => return send(Err(error)),
        },
    };
    let Ok(entries) = opened else {
        listing.read_error = true;
        return send(Ok(listing));
    };
    let mut directory = ReadDirectory {
        entries,
        copy: None,
    };
    let mut part_has_directories = false;
    loop {
        match directory.entries.read() {
            Some(Ok(entry)) => {
                let name = entry.file_name();
                if name == c"." || name == c".." {
                    continue;
                }
                let classified = directory
                    .entries
                    .fd()
                    .map_err(io::Error::from)
                    .and_then(|parent| classify(parent, job.device, &job.patterns, name, options));
                let mut entry_kind = match classified {
                    Ok(entry_kind) => entry_kind,
                    Err(error) => return send(Err(error)),
                };
                part_has_directories |= close_if_directory(&mut entry_kind);
                listing.entries.push((name.to_owned(), entry_kind));
                if listing.entries.len() == PART_ENTRIES {
                    let part = Listing {
                        index: listing.index,
                        entries: mem::take(&mut listing.entries),
                        last: false,
                        read_error: false,
                        subdirectories: directory.subdirectories_of_part(part_has_directories),
                        path: Arc::clone(&listing.path),
                    };
                    part_has_directories = false;
                    if !send(Ok(part)) {
                        return false;
                    }
                }
            }
            Some(Err(_)) => {
                listing.read_error = true;
                break;
            }
            None => break,
        }
    }
    listing.subdirectories = Subdirectories::At(directory.into_parent());
    send(Ok(listing))
}
