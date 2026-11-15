use std::io;

use crate::{Special, Stat};

/// A consumer of entries, such as an export or the in-memory tree.
///
/// A producer, such as a scan or an import, reports a tree depth-first:
///
/// ```text
/// root = sink.create_root(path, stat)
/// sink.add_stat(&mut root, name, stat)
/// sink.add_special(&mut root, name, special)
/// child = sink.add_directory(&mut root, name, stat)
///     sink.add_stat(&mut child, name, stat)
///     sink.finish_directory(child, Some(&mut root))
/// sink.finish_directory(root, None)
/// ```
///
/// Each sink has its own method to finish the output once the root is finished.
pub trait Sink {
    /// The handle of a directory whose entries are being added.
    type Directory;

    /// Start the tree at the directory `path`.
    fn create_root(&mut self, path: &[u8], stat: &Stat) -> io::Result<Self::Directory>;

    /// Add an entry without [`Stat`] data to `directory`.
    fn add_special(
        &mut self,
        directory: &mut Self::Directory,
        name: &[u8],
        special: Special,
    ) -> io::Result<()>;

    /// Add an entry that is not a directory to `directory`.
    fn add_stat(
        &mut self,
        directory: &mut Self::Directory,
        name: &[u8],
        stat: &Stat,
    ) -> io::Result<()>;

    /// Add a subdirectory to `parent` and return its handle. Its entries follow, and then
    /// [`Sink::finish_directory`].
    fn add_directory(
        &mut self,
        parent: &mut Self::Directory,
        name: &[u8],
        stat: &Stat,
    ) -> io::Result<Self::Directory>;

    /// Mark `directory` as not completely read.
    fn set_read_error(&mut self, directory: &mut Self::Directory) -> io::Result<()>;

    /// End `directory` after its last entry. `parent` is `None` for the root.
    fn finish_directory(
        &mut self,
        directory: Self::Directory,
        parent: Option<&mut Self::Directory>,
    ) -> io::Result<()>;
}
