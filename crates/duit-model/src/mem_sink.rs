use std::collections::{HashMap, HashSet};
use std::io;

use crate::tree::{DirectoryId, EntryExtended, EntryId, FileKind, Tree, add_blocks};
use crate::{EntryType, Sink, Special, Stat};

/// A [`Sink`] that builds a [`Tree`] in memory, with the totals of each directory.
///
/// The sizes of hard links are added to their directories when the tree is finished, by
/// [`Tree::add_all_stats`], so that each inode counts once in each directory.
#[derive(Debug)]
pub struct MemorySink {
    tree: Tree,
    /// Whether to keep the extended information, as with `-e`.
    extended: bool,
    /// The directory of the tree that a refresh scans again, `global.root` in `mem_sink.zig`, or
    /// `None` for a new tree.
    refresh: Option<DirectoryId>,
}

/// A refresh clears the counts of a directory with more items than this on a screen of its own,
/// as `createRoot` in `mem_sink.zig` does.
const ZEROING_ITEMS: u32 = 10_000;

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
    /// The entries that the directory had before a refresh, by name, which the refresh has not
    /// found again yet. The first entry of a name is the one that a refresh finds.
    previous: HashMap<Vec<u8>, EntryId>,
}

impl MemoryDirectory {
    /// The handle of `directory` of `tree`, which has the sizes of `stat`, `Dir.init` in
    /// `mem_sink.zig`.
    fn new(tree: &Tree, directory: DirectoryId, stat: &Stat) -> Self {
        let mut previous = HashMap::new();
        for id in tree.entries(directory) {
            previous.entry(tree.name(id).to_vec()).or_insert(id);
        }
        Self {
            directory,
            own_blocks: stat.blocks,
            own_size: stat.size,
            blocks: 0,
            size: 0,
            items: 0,
            mtime: 0,
            sub_error: false,
            previous,
        }
    }
}

impl MemorySink {
    /// A sink that keeps the extended information of the entries when `extended` is set.
    pub fn new(extended: bool) -> Self {
        Self {
            tree: Tree::default(),
            extended,
            refresh: None,
        }
    }

    /// A sink that scans `directory` of `tree` again into it, as a refresh does. The root of the
    /// scan is the directory, whose entries are kept when the scan finds them again by name, and
    /// dropped when it does not.
    pub fn refresh(tree: Tree, directory: DirectoryId, extended: bool) -> Self {
        Self {
            tree,
            extended,
            refresh: Some(directory),
        }
    }

    /// Finish the tree after the root is finished, with the sizes of the hard links, and return
    /// it, `done` in `mem_sink.zig`. The directories above a refreshed directory get its totals.
    ///
    /// A refresh whose directory could not be opened finishes the tree too, as `scan` in
    /// `scan.zig` defers `done`. The directory was not cleared then, so the directories above it
    /// count its totals twice, as in ncdu.
    pub fn finish(self) -> Tree {
        let mut tree = self.tree;
        if let Some(directory) = self.refresh.or(tree.root()) {
            let data = tree.directory(directory);
            let entry = tree.entry(data.entry);
            let (blocks, size, items) = (entry.blocks, entry.size, data.items);
            let mut next = Some(directory);
            while let Some(current) = next {
                tree.update_sub_error(current);
                let data = tree.directory_mut(current);
                next = data.parent;
                if current != directory {
                    data.items = data.items.saturating_add(items).saturating_add(1);
                    let id = data.entry;
                    let entry = tree.entry_mut(id);
                    entry.blocks = add_blocks(entry.blocks, blocks);
                    entry.size = entry.size.saturating_add(size);
                }
            }
        }
        tree.add_all_stats();
        tree
    }

    /// The extended information to keep for an entry with `stat`.
    fn extended_of(&self, stat: &Stat) -> Option<EntryExtended> {
        EntryExtended::of(stat, self.extended)
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

    /// Add the entry `name` of `kind` to `directory`, or give the entry of that name that a
    /// refresh finds again the new data, `getEntry` and `statToEntry` in `mem_sink.zig`.
    fn add_file(
        &mut self,
        directory: &mut MemoryDirectory,
        kind: FileKind,
        stat: &Stat,
        extended: Option<EntryExtended>,
        name: &[u8],
    ) {
        if let Some(id) = directory.previous.get(name).copied()
            && self
                .tree
                .reuse(id, directory.directory, kind, stat, extended)
        {
            directory.previous.remove(name);
        } else {
            self.tree
                .create(directory.directory, kind, stat, extended, name);
        }
    }
}

impl Sink for MemorySink {
    type Directory = MemoryDirectory;

    fn create_root(&mut self, path: &[u8], stat: &Stat) -> io::Result<MemoryDirectory> {
        let device = self.tree.device_id(stat.device)?;
        let Some(root) = self.refresh else {
            let entry_extended = self.extended_of(stat);
            let root = self
                .tree
                .create_directory(None, device, stat, entry_extended, path);
            self.tree.set_root(root);
            return Ok(MemoryDirectory::new(&self.tree, root, stat));
        };
        // The counts are cleared once the directory could be opened, so that a refresh that
        // fails leaves them.
        let data = self.tree.directory(root);
        let (id, parent) = (data.entry, data.parent);
        self.tree.zero_stats(id, parent);
        self.tree.directory_mut(root).device = device;
        let entry = self.tree.entry_mut(id);
        entry.blocks = stat.blocks;
        entry.size = stat.size;
        // The directory keeps its extended information, or its lack of it.
        if let Some(entry_extended) = self.tree.extended_mut(id) {
            *entry_extended = EntryExtended {
                extended: stat.extended,
                latest_mtime: stat.extended.mtime.unwrap_or(0),
            };
        }
        Ok(MemoryDirectory::new(&self.tree, root, stat))
    }

    fn zeroing(&self) -> bool {
        self.refresh
            .is_some_and(|directory| self.tree.directory(directory).items > ZEROING_ITEMS)
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
        self.add_file(
            directory,
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
        self.add_file(directory, kind, stat, entry_extended, name);
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
        let reused = parent.previous.get(name).copied().and_then(|id| {
            let directory = self
                .tree
                .reuse_directory(id, device, stat, entry_extended)?;
            parent.previous.remove(name);
            Some(directory)
        });
        let directory = reused.unwrap_or_else(|| {
            self.tree
                .create_directory(Some(parent.directory), device, stat, entry_extended, name)
        });
        Ok(MemoryDirectory::new(&self.tree, directory, stat))
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
        // The entries that a refresh did not find again are cleared already, with their hard links
        // out of the accounting.
        if !directory.previous.is_empty() {
            let gone: HashSet<EntryId> = directory.previous.values().copied().collect();
            self.tree
                .retain(directory.directory, |id| !gone.contains(&id));
        }
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
