use std::io;
use std::rc::Rc;

use crate::{Sink, Special, Stat};

/// The producer that reports to a [`ProgressSink`], which decides where it handles an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Producer {
    /// A scan, which handles an event at each step of its walk, as `Thread.run` in `scan.zig`
    /// calls `main.handleEvent` before it reads the next entry.
    Scan,
    /// An import, which handles an event after each item, and after the entries of a directory,
    /// when the count of files has bits 0 and 6 clear, as `json_import.zig` and `bin_reader.zig`
    /// call `main.handleEvent`.
    Import,
    /// A replay of the tree in memory, which handles an event before each item when the count of
    /// files has bits 0 and 6 clear, as `rec` in `mem_src.zig` calls `main.handleEvent`. The root
    /// shows no directory until a subdirectory is finished, as `run` in `mem_src.zig` sets none.
    Replay,
}

/// What a scan or an import has reported so far, the counters of a `Thread` in `sink.zig`.
#[derive(Debug, Default)]
pub struct Progress {
    files: u64,
    bytes: u64,
    /// The path of the directory that entries were added to last.
    path: Vec<u8>,
    /// The directories from the root to the one of `path`, each with the length of `path` before
    /// its name, or none when no directory is being read.
    open: Vec<(Rc<Node>, usize)>,
    /// The path of the last entry that could not be read, `last_error` in `sink.zig`.
    last_error: Option<Vec<u8>>,
    /// The sink clears the counts of the directory that a refresh scans again, `zeroing` in
    /// `sink.zig`.
    zeroing: bool,
}

impl Progress {
    /// The number of entries reported, the root left out.
    pub fn files(&self) -> u64 {
        self.files
    }

    /// The disk usage of the entries reported, with the blocks of a hard link divided between its
    /// links.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// The path of the directory being read, as `Dir.path` in `sink.zig` joins it, or `None` once
    /// the root is finished.
    pub fn directory(&self) -> Option<&[u8]> {
        (!self.open.is_empty()).then_some(self.path.as_slice())
    }

    /// The path of the last entry that could not be read: a directory that failed to read, or an
    /// entry that failed to `stat`.
    pub fn last_error(&self) -> Option<&[u8]> {
        self.last_error.as_deref()
    }

    /// Whether the sink is about to clear the counts of the directory that a refresh scans again,
    /// which ncdu shows on a screen of its own.
    pub fn zeroing(&self) -> bool {
        self.zeroing
    }

    /// Show `node` as the directory being read. Only the names below the directories that
    /// `node` shares with the one shown are joined, so a walk depth-first does constant work.
    fn show(&mut self, node: &Rc<Node>) {
        let mut missing = Vec::new();
        let mut next = Some(node);
        while let Some(current) = next {
            if self
                .open
                .get(current.depth)
                .is_some_and(|(open, _)| Rc::ptr_eq(open, current))
            {
                break;
            }
            missing.push(current);
            next = current.parent.as_ref();
        }
        let kept = next.map_or(0, |shared| shared.depth + 1);
        if let Some(&(_, length)) = self.open.get(kept) {
            self.path.truncate(length);
        }
        self.open.truncate(kept);
        for current in missing.into_iter().rev() {
            let length = self.path.len();
            push_name(&mut self.path, current.depth == 0, &current.name);
            self.open.push((Rc::clone(current), length));
        }
    }

    /// The path of `node`, joined with `name` if it is given.
    fn path_of(&self, node: &Rc<Node>, name: Option<&[u8]>) -> Vec<u8> {
        let mut path = if self
            .open
            .last()
            .is_some_and(|(open, _)| Rc::ptr_eq(open, node))
        {
            self.path.clone()
        } else {
            let mut names = Vec::new();
            let mut next = Some(node);
            while let Some(current) = next {
                names.push(&current.name);
                next = current.parent.as_ref();
            }
            let mut path = Vec::new();
            for (depth, name) in names.into_iter().rev().enumerate() {
                push_name(&mut path, depth == 0, name);
            }
            path
        };
        if let Some(name) = name {
            push_name(&mut path, false, name);
        }
        path
    }
}

/// Join `name` to `path`, as `Dir.path` in `sink.zig` joins the names of a directory and its
/// parents: with a `/` unless `name` is the root or `path` ends with one.
fn push_name(path: &mut Vec<u8>, is_root: bool, name: &[u8]) {
    if !is_root && !path.ends_with(b"/") {
        path.push(b'/');
    }
    path.extend_from_slice(name);
}

/// A directory that is not finished, with its name and its parent, as `Dir` in `sink.zig` keeps
/// them to join its path. A scan with several threads adds entries to any directory that is not
/// finished, so each directory can give its own path.
#[derive(Debug)]
struct Node {
    name: Box<[u8]>,
    parent: Option<Rc<Self>>,
    /// The number of directories above this one.
    depth: usize,
}

impl Drop for Node {
    /// Drop the parents that only this node holds one at a time, so that a deep tree does not
    /// overflow the stack.
    fn drop(&mut self) {
        let mut parent = self.parent.take();
        while let Some(node) = parent {
            parent = Rc::into_inner(node).and_then(|mut node| node.parent.take());
        }
    }
}

/// A directory of the sink behind a [`ProgressSink`], which the progress shows while entries are
/// added to it.
pub struct ProgressDirectory<D> {
    directory: D,
    node: Rc<Node>,
}

/// A [`Sink`] in front of another that keeps the [`Progress`] of the producer, and passes it to
/// `handle_event` where ncdu calls `main.handleEvent`, as `Dir` and `Thread` in `sink.zig` count
/// what passes through them. An error of `handle_event` stops the producer, which the sink returns
/// it to, as a key that aborts the scan in `keyInput` does.
pub struct ProgressSink<'a, S> {
    sink: S,
    producer: Producer,
    progress: Progress,
    handle_event: &'a mut dyn FnMut(&Progress) -> io::Result<()>,
}

impl<'a, S: Sink> ProgressSink<'a, S> {
    /// A sink in front of `sink` that passes the progress of `producer` to `handle_event`.
    pub fn new(
        sink: S,
        producer: Producer,
        handle_event: &'a mut dyn FnMut(&Progress) -> io::Result<()>,
    ) -> Self {
        Self {
            sink,
            producer,
            progress: Progress::default(),
            handle_event,
        }
    }

    /// The sink behind this one.
    pub fn into_inner(self) -> S {
        self.sink
    }

    /// Get ready to add an entry to `directory`: show it, or handle an event first if the
    /// producer handles one there.
    fn item_start(&mut self, directory: &ProgressDirectory<S::Directory>) -> io::Result<()> {
        match self.producer {
            Producer::Scan | Producer::Import => self.progress.show(&directory.node),
            Producer::Replay if self.progress.files & 0b100_0001 == 0 => {
                (self.handle_event)(&self.progress)?;
            }
            Producer::Replay => {}
        }
        Ok(())
    }

    /// Handle an event after an item, if the producer handles one there.
    fn item_done(&mut self) -> io::Result<()> {
        let due = match self.producer {
            Producer::Scan => true,
            Producer::Import => self.progress.files & 0b100_0001 == 0,
            Producer::Replay => false,
        };
        if due {
            (self.handle_event)(&self.progress)?;
        }
        Ok(())
    }

    fn add(&mut self, bytes: u64) {
        self.progress.files = self.progress.files.saturating_add(1);
        self.progress.bytes = self.progress.bytes.saturating_add(bytes);
    }
}

impl<S: Sink> Sink for ProgressSink<'_, S> {
    type Directory = ProgressDirectory<S::Directory>;

    fn create_root(&mut self, path: &[u8], stat: &Stat) -> io::Result<Self::Directory> {
        if self.sink.zeroing() {
            self.progress.zeroing = true;
            let handled = (self.handle_event)(&self.progress);
            self.progress.zeroing = false;
            handled?;
        }
        let root = self.sink.create_root(path, stat)?;
        let root = ProgressDirectory {
            directory: root,
            node: Rc::new(Node {
                name: path.into(),
                parent: None,
                depth: 0,
            }),
        };
        match self.producer {
            Producer::Scan => {
                self.progress.show(&root.node);
                (self.handle_event)(&self.progress)?;
            }
            Producer::Import => self.progress.show(&root.node),
            Producer::Replay => {}
        }
        Ok(root)
    }

    fn add_special(
        &mut self,
        directory: &mut Self::Directory,
        name: &[u8],
        special: Special,
    ) -> io::Result<()> {
        self.item_start(directory)?;
        self.sink
            .add_special(&mut directory.directory, name, special)?;
        match special {
            Special::ReadError => {
                self.progress.last_error = Some(self.progress.path_of(&directory.node, Some(name)));
            }
            Special::Pattern | Special::OtherFilesystem | Special::KernelFilesystem => {}
        }
        self.add(0);
        self.item_done()
    }

    fn add_stat(
        &mut self,
        directory: &mut Self::Directory,
        name: &[u8],
        stat: &Stat,
    ) -> io::Result<()> {
        self.item_start(directory)?;
        self.sink.add_stat(&mut directory.directory, name, stat)?;
        self.add(stat.blocks.saturating_mul(512) / u64::from(stat.link_count.max(1)));
        self.item_done()
    }

    /// A directory is done after its entries, so no event is handled here. A scan reads the first
    /// entry next, and handles the event after it.
    fn add_directory(
        &mut self,
        parent: &mut Self::Directory,
        name: &[u8],
        stat: &Stat,
    ) -> io::Result<Self::Directory> {
        self.item_start(parent)?;
        let directory = self.sink.add_directory(&mut parent.directory, name, stat)?;
        let directory = ProgressDirectory {
            directory,
            node: Rc::new(Node {
                name: name.into(),
                parent: Some(Rc::clone(&parent.node)),
                depth: parent.node.depth + 1,
            }),
        };
        self.add(stat.blocks.saturating_mul(512));
        self.progress.show(&directory.node);
        Ok(directory)
    }

    /// The path of the directory is the last error then.
    fn set_read_error(&mut self, directory: &mut Self::Directory) -> io::Result<()> {
        self.sink.set_read_error(&mut directory.directory)?;
        self.progress.last_error = Some(self.progress.path_of(&directory.node, None));
        Ok(())
    }

    /// The parent is shown next. A scan handles no event after the root, whose walk ends there.
    fn finish_directory(
        &mut self,
        directory: Self::Directory,
        parent: Option<&mut Self::Directory>,
    ) -> io::Result<()> {
        if let Some(parent) = parent {
            self.sink
                .finish_directory(directory.directory, Some(&mut parent.directory))?;
            self.progress.show(&parent.node);
        } else {
            self.sink.finish_directory(directory.directory, None)?;
            self.progress.open.clear();
            self.progress.path.clear();
            if self.producer == Producer::Scan {
                return Ok(());
            }
        }
        self.item_done()
    }

    /// Handle an event while the producer waits.
    fn idle(&mut self) -> io::Result<()> {
        (self.handle_event)(&self.progress)
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::{Producer, Progress, ProgressSink};
    use crate::{EntryType, MemorySink, Sink, Special, Stat};

    /// The counters and the directory at each event of the entries that `build` adds, as a
    /// `producer` reports them.
    fn events(
        producer: Producer,
        build: impl FnOnce(&mut ProgressSink<'_, MemorySink>) -> io::Result<()>,
    ) -> Vec<(u64, u64, Option<String>)> {
        let mut events = Vec::new();
        let mut record = |progress: &Progress| {
            let directory = progress
                .directory()
                .map(|directory| String::from_utf8_lossy(directory).into_owned());
            events.push((progress.files(), progress.bytes(), directory));
            Ok(())
        };
        let mut sink = ProgressSink::new(MemorySink::new(false), producer, &mut record);
        build(&mut sink).expect("entries are added");
        drop(sink);
        events
    }

    fn directory() -> Stat {
        Stat {
            entry_type: EntryType::Directory,
            blocks: 1,
            ..Stat::default()
        }
    }

    /// Every kind of entry, in a root whose name ends with a `/` and a subdirectory.
    fn tree(sink: &mut impl Sink) -> io::Result<()> {
        let mut root = sink.create_root(b"/", &directory())?;
        let file = Stat {
            blocks: 8,
            link_count: 1,
            ..Stat::default()
        };
        sink.add_stat(&mut root, b"file", &file)?;
        let link = Stat {
            entry_type: EntryType::HardLink,
            inode: 1,
            link_count: 2,
            ..file
        };
        sink.add_stat(&mut root, b"link", &link)?;
        sink.add_special(&mut root, b"excluded", Special::Pattern)?;
        let mut child = sink.add_directory(&mut root, b"child", &directory())?;
        sink.add_stat(&mut child, b"empty", &Stat::default())?;
        sink.finish_directory(child, Some(&mut root))?;
        sink.finish_directory(root, None)
    }

    /// As in `sink.zig`, every entry but the root counts as a file, and adds its blocks, which a
    /// hard link shares with its other links. A scan handles an event at each step of its walk,
    /// as `Thread.run` in `scan.zig` calls `main.handleEvent` before it reads the next entry of
    /// the directory on top of its stack, but not after the root.
    #[test]
    fn scan() {
        assert_eq!(
            events(Producer::Scan, |sink| tree(sink)),
            [
                (0, 0, Some("/".to_owned())),
                (1, 4096, Some("/".to_owned())),
                (2, 6144, Some("/".to_owned())),
                (3, 6144, Some("/".to_owned())),
                (5, 6656, Some("/child".to_owned())),
                (5, 6656, Some("/".to_owned())),
            ]
        );
    }

    /// An import handles an event after each item, a directory after its entries, when the count
    /// of files has bits 0 and 6 clear, as `json_import.zig` and `bin_reader.zig` call
    /// `main.handleEvent`. After the root, no directory is being read.
    #[test]
    fn import() {
        let events = events(Producer::Import, |sink| {
            let mut root = sink.create_root(b"root", &directory())?;
            for _ in 0..130 {
                sink.add_special(&mut root, b"excluded", Special::Pattern)?;
            }
            sink.finish_directory(root, None)
        });
        let files: Vec<u64> = events.iter().map(|(files, _, _)| *files).collect();
        let expected: Vec<u64> = (2..=62).step_by(2).chain([128, 130, 130]).collect();
        assert_eq!(files, expected);
        assert_eq!(events.first(), Some(&(2, 0, Some("root".to_owned()))));
        assert_eq!(events.last(), Some(&(130, 0, None)));
    }

    /// The last read error names the directory that failed to read, unlike ncdu, which names the
    /// root, or the entry that failed in its directory, as `setReadError` and `addSpecial` in
    /// `sink.zig` store `last_error`. Other special entries leave it as it is.
    #[test]
    fn last_error() {
        let mut errors = Vec::new();
        let mut record = |progress: &Progress| {
            errors.push(
                progress
                    .last_error()
                    .map(|path| String::from_utf8_lossy(path).into_owned()),
            );
            Ok(())
        };
        let mut sink = ProgressSink::new(MemorySink::new(false), Producer::Scan, &mut record);
        let built = (|| -> io::Result<()> {
            let mut root = sink.create_root(b"/", &directory())?;
            sink.add_special(&mut root, b"unreadable", Special::ReadError)?;
            let mut child = sink.add_directory(&mut root, b"child", &directory())?;
            sink.set_read_error(&mut child)?;
            sink.add_special(&mut child, b"excluded", Special::Pattern)?;
            sink.finish_directory(child, Some(&mut root))?;
            sink.add_special(&mut root, b"other", Special::OtherFilesystem)?;
            sink.finish_directory(root, None)
        })();
        built.expect("entries are added");
        drop(sink);
        assert_eq!(
            errors,
            [
                None,
                Some("/unreadable".to_owned()),
                Some("/child".to_owned()),
                Some("/child".to_owned()),
                Some("/child".to_owned()),
            ]
        );
    }

    /// A scan with several threads adds entries to a directory after other directories were
    /// added. The directory of an event is the one that an entry was added to last, or the parent
    /// of the directory finished last, and a read error names the directory it marks.
    #[test]
    fn interleaved() {
        let mut events = Vec::new();
        let mut record = |progress: &Progress| {
            let text =
                |path: Option<&[u8]>| path.map(|path| String::from_utf8_lossy(path).into_owned());
            events.push((text(progress.directory()), text(progress.last_error())));
            Ok(())
        };
        let mut sink = ProgressSink::new(MemorySink::new(false), Producer::Scan, &mut record);
        let built = (|| -> io::Result<()> {
            let mut root = sink.create_root(b"/", &directory())?;
            let mut first = sink.add_directory(&mut root, b"first", &directory())?;
            let mut second = sink.add_directory(&mut root, b"second", &directory())?;
            sink.add_special(&mut first, b"excluded", Special::Pattern)?;
            sink.add_special(&mut second, b"excluded", Special::Pattern)?;
            sink.set_read_error(&mut first)?;
            sink.finish_directory(first, Some(&mut root))?;
            sink.add_special(&mut second, b"unreadable", Special::ReadError)?;
            sink.finish_directory(second, Some(&mut root))?;
            sink.finish_directory(root, None)
        })();
        built.expect("entries are added");
        drop(sink);
        let text = |path: &str| Some(path.to_owned());
        assert_eq!(
            events,
            [
                (text("/"), None),
                (text("/first"), None),
                (text("/second"), None),
                (text("/"), text("/first")),
                (text("/second"), text("/second/unreadable")),
                (text("/"), text("/second/unreadable")),
            ]
        );
    }

    /// A replay of the tree handles an event before each item when the count of files has bits 0
    /// and 6 clear, and none after the entries of a directory, as `rec` in `mem_src.zig` calls
    /// `main.handleEvent`. As `run` in `mem_src.zig` sets no directory for the root, the entries
    /// of the root show none until a subdirectory is finished.
    #[test]
    fn replay() {
        let events = events(Producer::Replay, |sink| {
            let mut root = sink.create_root(b"root", &directory())?;
            sink.add_special(&mut root, b"excluded", Special::Pattern)?;
            let mut child = sink.add_directory(&mut root, b"child", &directory())?;
            for _ in 0..70 {
                sink.add_special(&mut child, b"excluded", Special::Pattern)?;
            }
            sink.finish_directory(child, Some(&mut root))?;
            for _ in 0..60 {
                sink.add_special(&mut root, b"excluded", Special::Pattern)?;
            }
            sink.finish_directory(root, None)
        });
        let files: Vec<u64> = events.iter().map(|(files, _, _)| *files).collect();
        let expected: Vec<u64> = (0..=62).step_by(2).chain([128, 130]).collect();
        assert_eq!(files, expected);
        assert_eq!(events.first(), Some(&(0, 0, None)));
        assert_eq!(events[1], (2, 512, Some("root/child".to_owned())));
        assert_eq!(events.last(), Some(&(130, 512, Some("root".to_owned()))));
    }

    /// An error of the event handler is returned to the producer.
    #[test]
    fn handle_event_error() {
        let mut fail = |progress: &Progress| {
            if progress.files() == 0 {
                Ok(())
            } else {
                Err(io::Error::other("aborted"))
            }
        };
        let mut sink = ProgressSink::new(MemorySink::new(false), Producer::Scan, &mut fail);
        let mut root = sink
            .create_root(b"root", &directory())
            .expect("root is added");
        let error = sink
            .add_special(&mut root, b"excluded", Special::Pattern)
            .expect_err("event handler fails");
        assert_eq!(error.to_string(), "aborted");
    }

    /// A refresh of a directory with more than 10 000 items handles an event before it clears
    /// their counts, once the directory could be opened, as `createRoot` in `mem_sink.zig` draws
    /// `zeroing`. The next event is that of the root.
    #[test]
    fn zeroing() {
        for (items, expected) in [(10_000, &[false][..]), (10_001, &[true, false])] {
            let mut memory = MemorySink::new(false);
            let built = (|| -> io::Result<()> {
                let mut root = memory.create_root(b"/", &directory())?;
                for _ in 0..items {
                    memory.add_special(&mut root, b"excluded", Special::Pattern)?;
                }
                memory.finish_directory(root, None)
            })();
            built.expect("entries are added");
            let tree = memory.finish(&mut |_, _| {});
            let root = tree.root().expect("tree has a root");
            let mut zeroing = Vec::new();
            let mut record = |progress: &Progress| {
                zeroing.push(progress.zeroing());
                Ok(())
            };
            let memory = MemorySink::refresh(tree, root, false);
            let mut sink = ProgressSink::new(memory, Producer::Scan, &mut record);
            sink.create_root(b"/", &directory()).expect("root is added");
            drop(sink);
            assert_eq!(zeroing, expected, "{items}");
        }
    }

    /// The bytes saturate, as `+|` and `*|` do in `sink.zig`.
    #[test]
    fn saturating_bytes() {
        let events = events(Producer::Scan, |sink| {
            let mut root = sink.create_root(b"root", &directory())?;
            let huge = Stat {
                blocks: u64::MAX,
                ..Stat::default()
            };
            sink.add_stat(&mut root, b"huge", &huge)?;
            sink.add_directory(&mut root, b"child", &directory())
                .and_then(|child| sink.finish_directory(child, Some(&mut root)))?;
            sink.finish_directory(root, None)
        });
        assert_eq!(
            events,
            [
                (0, 0, Some("root".to_owned())),
                (1, u64::MAX, Some("root".to_owned())),
                (2, u64::MAX, Some("root".to_owned())),
            ]
        );
    }
}
