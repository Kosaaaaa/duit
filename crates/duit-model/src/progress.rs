use std::io;

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
}

/// What a scan or an import has reported so far, the counters of a `Thread` in `sink.zig`.
#[derive(Debug, Default)]
pub struct Progress {
    files: u64,
    bytes: u64,
    /// The path of the innermost directory that is open.
    path: Vec<u8>,
    /// The length of `path` before each open directory was added to it, from the root.
    lengths: Vec<usize>,
    /// The path of the last entry that could not be read, `last_error` in `sink.zig`.
    last_error: Option<Vec<u8>>,
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
        (!self.lengths.is_empty()).then_some(self.path.as_slice())
    }

    /// The path of the last entry that could not be read: a directory that failed to read, or an
    /// entry that failed to `stat`.
    pub fn last_error(&self) -> Option<&[u8]> {
        self.last_error.as_deref()
    }

    fn enter(&mut self, name: &[u8]) {
        self.lengths.push(self.path.len());
        if self.lengths.len() > 1 && !self.path.ends_with(b"/") {
            self.path.push(b'/');
        }
        self.path.extend_from_slice(name);
    }

    /// Store the path of the open directory, joined with `name` if it is given, as the last error.
    fn set_error(&mut self, name: Option<&[u8]>) {
        let mut path = self.path.clone();
        if let Some(name) = name {
            if !path.ends_with(b"/") {
                path.push(b'/');
            }
            path.extend_from_slice(name);
        }
        self.last_error = Some(path);
    }

    fn leave(&mut self) {
        if let Some(length) = self.lengths.pop() {
            self.path.truncate(length);
        }
    }
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

    /// Handle an event after an item, if the producer handles one there.
    fn item_done(&mut self) -> io::Result<()> {
        let due = match self.producer {
            Producer::Scan => true,
            Producer::Import => self.progress.files & 0b100_0001 == 0,
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
    type Directory = S::Directory;

    fn create_root(&mut self, path: &[u8], stat: &Stat) -> io::Result<S::Directory> {
        let root = self.sink.create_root(path, stat)?;
        self.progress.enter(path);
        match self.producer {
            Producer::Scan => (self.handle_event)(&self.progress)?,
            Producer::Import => {}
        }
        Ok(root)
    }

    fn add_special(
        &mut self,
        directory: &mut S::Directory,
        name: &[u8],
        special: Special,
    ) -> io::Result<()> {
        self.sink.add_special(directory, name, special)?;
        match special {
            Special::ReadError => self.progress.set_error(Some(name)),
            Special::Pattern | Special::OtherFilesystem | Special::KernelFilesystem => {}
        }
        self.add(0);
        self.item_done()
    }

    fn add_stat(
        &mut self,
        directory: &mut S::Directory,
        name: &[u8],
        stat: &Stat,
    ) -> io::Result<()> {
        self.sink.add_stat(directory, name, stat)?;
        self.add(stat.blocks.saturating_mul(512) / u64::from(stat.link_count.max(1)));
        self.item_done()
    }

    /// A directory is done after its entries, so no event is handled here. A scan reads the first
    /// entry next, and handles the event after it.
    fn add_directory(
        &mut self,
        parent: &mut S::Directory,
        name: &[u8],
        stat: &Stat,
    ) -> io::Result<S::Directory> {
        let directory = self.sink.add_directory(parent, name, stat)?;
        self.add(stat.blocks.saturating_mul(512));
        self.progress.enter(name);
        Ok(directory)
    }

    /// Every producer marks the innermost open directory, whose path is the last error then.
    fn set_read_error(&mut self, directory: &mut S::Directory) -> io::Result<()> {
        self.sink.set_read_error(directory)?;
        self.progress.set_error(None);
        Ok(())
    }

    /// A scan handles no event after the root, whose walk ends there.
    fn finish_directory(
        &mut self,
        directory: S::Directory,
        parent: Option<&mut S::Directory>,
    ) -> io::Result<()> {
        let is_root = parent.is_none();
        self.sink.finish_directory(directory, parent)?;
        self.progress.leave();
        match self.producer {
            Producer::Scan if is_root => Ok(()),
            Producer::Scan | Producer::Import => self.item_done(),
        }
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
