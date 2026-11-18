use std::io;

use crate::tree::{DirectoryId, EntryExtended, FileKind, Tree, add_blocks};
use crate::{EntryType, Extended, Sink, Special, Stat};

/// A [`Sink`] that builds a [`Tree`] in memory, with the totals of each directory.
///
/// The sizes of hard links are added to their directories when the tree is finished, by
/// [`Tree::add_all_stats`], so that each inode counts once in each directory.
#[derive(Debug)]
pub struct MemorySink {
    tree: Tree,
    /// Whether to keep the extended information, as with `-e`.
    extended: bool,
}

/// The handle of a directory of a [`MemorySink`] whose entries are being added.
#[derive(Debug)]
pub struct MemoryDirectory {
    directory: DirectoryId,
    /// The sizes of the directory itself, which its parent added when it added the directory.
    own_blocks: u64,
    own_size: u64,
    /// The totals that the subdirectories add when they are finished.
    blocks: u64,
    size: u64,
    items: u32,
    mtime: u64,
    sub_error: bool,
}

impl MemoryDirectory {
    fn new(directory: DirectoryId, stat: &Stat) -> Self {
        Self {
            directory,
            own_blocks: stat.blocks,
            own_size: stat.size,
            blocks: 0,
            size: 0,
            items: 0,
            mtime: 0,
            sub_error: false,
        }
    }
}

impl MemorySink {
    /// A sink that keeps the extended information of the entries when `extended` is set.
    pub fn new(extended: bool) -> Self {
        Self {
            tree: Tree::default(),
            extended,
        }
    }

    /// Finish the tree after the root is finished, with the sizes of the hard links, and return
    /// it.
    pub fn finish(self) -> Tree {
        let mut tree = self.tree;
        if let Some(root) = tree.root() {
            tree.update_sub_error(root);
        }
        tree.add_all_stats();
        tree
    }

    /// The extended information to keep for an entry with `stat`.
    fn extended_of(&self, stat: &Stat) -> Option<EntryExtended> {
        (self.extended && stat.extended != Extended::default()).then(|| EntryExtended {
            extended: stat.extended,
            latest_mtime: stat.extended.mtime.unwrap_or(0),
        })
    }

    /// Count an entry with `stat` in the totals of `directory`. The sizes of a hard link are left
    /// to [`Tree::add_all_stats`].
    fn count(&mut self, directory: &MemoryDirectory, stat: &Stat) {
        let data = self.tree.directory_mut(directory.directory);
        data.items = data.items.saturating_add(1);
        let id = data.entry;
        if stat.entry_type != EntryType::HardLink {
            let entry = self.tree.entry_mut(id);
            entry.blocks = add_blocks(entry.blocks, stat.blocks);
            entry.size = entry.size.saturating_add(stat.size);
        }
        if let Some(entry_extended) = self.tree.extended_mut(id) {
            entry_extended.latest_mtime = entry_extended
                .latest_mtime
                .max(stat.extended.mtime.unwrap_or(0));
        }
    }
}

impl Sink for MemorySink {
    type Directory = MemoryDirectory;

    fn create_root(&mut self, path: &[u8], stat: &Stat) -> io::Result<MemoryDirectory> {
        let device = self.tree.device_id(stat.device)?;
        let entry_extended = self.extended_of(stat);
        let root = self
            .tree
            .create_directory(None, device, stat, entry_extended, path);
        self.tree.set_root(root);
        Ok(MemoryDirectory::new(root, stat))
    }

    fn add_special(
        &mut self,
        directory: &mut MemoryDirectory,
        name: &[u8],
        special: Special,
    ) -> io::Result<()> {
        let data = self.tree.directory_mut(directory.directory);
        data.items = data.items.saturating_add(1);
        if special == Special::ReadError {
            data.sub_error = true;
        }
        self.tree.create(
            directory.directory,
            FileKind::Special(special),
            &Stat::default(),
            None,
            name,
        );
        Ok(())
    }

    fn add_stat(
        &mut self,
        directory: &mut MemoryDirectory,
        name: &[u8],
        stat: &Stat,
    ) -> io::Result<()> {
        let kind = match stat.entry_type {
            // A producer adds a directory with `add_directory`, so this one gets no entries.
            EntryType::Directory => return self.add_directory(directory, name, stat).map(drop),
            EntryType::Regular => FileKind::Regular,
            EntryType::NonRegular => FileKind::NonRegular,
            EntryType::HardLink => FileKind::HardLink,
        };
        self.count(directory, stat);
        let entry_extended = self.extended_of(stat);
        self.tree
            .create(directory.directory, kind, stat, entry_extended, name);
        Ok(())
    }

    fn add_directory(
        &mut self,
        parent: &mut MemoryDirectory,
        name: &[u8],
        stat: &Stat,
    ) -> io::Result<MemoryDirectory> {
        let device = self.tree.device_id(stat.device)?;
        self.count(parent, stat);
        let entry_extended = self.extended_of(stat);
        let directory =
            self.tree
                .create_directory(Some(parent.directory), device, stat, entry_extended, name);
        Ok(MemoryDirectory::new(directory, stat))
    }

    fn set_read_error(&mut self, directory: &mut MemoryDirectory) -> io::Result<()> {
        self.tree.directory_mut(directory.directory).read_error = true;
        Ok(())
    }

    fn finish_directory(
        &mut self,
        directory: MemoryDirectory,
        parent: Option<&mut MemoryDirectory>,
    ) -> io::Result<()> {
        let id = self.tree.directory(directory.directory).entry;
        let data = self.tree.directory_mut(directory.directory);
        data.items = data.items.saturating_add(directory.items);
        if directory.sub_error {
            data.sub_error = true;
        }
        let (items, read_error, sub_error) = (data.items, data.read_error, data.sub_error);
        let entry = self.tree.entry_mut(id);
        entry.blocks = add_blocks(entry.blocks, directory.blocks);
        entry.size = entry.size.saturating_add(directory.size);
        let (blocks, size) = (entry.blocks, entry.size);
        let mtime = self.tree.extended_mut(id).map(|entry_extended| {
            entry_extended.latest_mtime = entry_extended.latest_mtime.max(directory.mtime);
            entry_extended.latest_mtime
        });
        if let Some(parent) = parent {
            parent.blocks = add_blocks(parent.blocks, blocks.saturating_sub(directory.own_blocks));
            parent.size = parent
                .size
                .saturating_add(size.saturating_sub(directory.own_size));
            parent.items = parent.items.saturating_add(items);
            if let Some(mtime) = mtime {
                parent.mtime = parent.mtime.max(mtime);
            }
            if directory.sub_error || sub_error || read_error {
                parent.sub_error = true;
            }
        }
        Ok(())
    }
}
