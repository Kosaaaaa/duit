use std::collections::HashMap;
use std::collections::hash_map::Entry as MapEntry;
use std::error::Error;
use std::fmt;
use std::io;
use std::iter;

use crate::{Extended, Special, Stat};

/// The largest block count that ncdu stores, in 60 bits. Sums of block counts saturate here.
pub const BLOCKS_MAX: u64 = (1 << 60) - 1;

/// The number of device identifiers that ncdu can store, in 30 bits.
const DEVICES_MAX: usize = (1 << 30) - 1;

/// Why a [`Tree`] could not take an entry. It reaches the caller of a [`crate::Sink`] method as
/// the inner error of an [`io::Error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeError {
    /// The tree holds as many distinct devices as ncdu can number.
    TooManyDevices,
}

impl fmt::Display for TreeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooManyDevices => "maximum number of device identifiers exceeded",
        })
    }
}

impl Error for TreeError {}

impl From<TreeError> for io::Error {
    fn from(error: TreeError) -> Self {
        Self::other(error)
    }
}

/// The identifier of an [`Entry`] in its [`Tree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntryId(usize);

/// The identifier of a [`Directory`] in its [`Tree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DirectoryId(usize);

/// The identifier of a [`Link`] in its [`Tree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LinkId(usize);

/// The identifier of a device in its [`Tree`], which [`Tree::device`] turns back into the
/// device number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceId(usize);

/// What an [`Entry`] is, with the identifier of the data that a directory or a hard link has on
/// top of an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Directory(DirectoryId),
    HardLink(LinkId),
    Regular,
    NonRegular,
    /// An entry without stat data. An excluded directory is one too, so it has no entries.
    Special(Special),
}

/// An entry of a [`Tree`].
#[derive(Debug, Clone)]
pub struct Entry {
    pub kind: EntryKind,
    /// The disk usage in 512-byte blocks. For a directory, it includes the entries below it.
    pub blocks: u64,
    /// The apparent size in bytes. For a directory, it includes the entries below it.
    pub size: u64,
    /// The next entry of the same directory.
    next: Option<EntryId>,
    /// The range of the name in the names of the tree.
    name_start: usize,
    name_end: usize,
    /// The index of the extended information in the tree.
    extended: Option<usize>,
}

/// The data that a directory has on top of its [`Entry`].
#[derive(Debug, Clone)]
pub struct Directory {
    pub entry: EntryId,
    /// The directory that holds this one, or `None` for the root.
    pub parent: Option<DirectoryId>,
    /// The number of entries below this directory, at any depth.
    pub items: u32,
    pub device: DeviceId,
    /// The directory could not be read completely.
    pub read_error: bool,
    /// An entry below this directory, at any depth, could not be read.
    pub sub_error: bool,
    /// The first entry of this directory, which is the last one that was added.
    sub: Option<EntryId>,
}

/// The data that a hard link has on top of its [`Entry`]. Its device is that of its parent.
#[derive(Debug, Clone)]
pub struct Link {
    pub parent: DirectoryId,
    pub inode: u64,
    /// The number of links to the inode, or 0 when it is unknown, as in old JSON exports.
    pub link_count: u32,
}

/// The extended information of an [`Entry`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EntryExtended {
    pub extended: Extended,
    /// The mtime of the entry, or 0 when it is unknown. A directory gets the latest mtime of
    /// its entries, and of the directories with extended information below it, even when its own
    /// is unknown. ncdu sorts by this value, and shows it only when `extended.mtime` is known.
    pub latest_mtime: u64,
}

/// A directory tree in memory. Entries refer to each other by identifier, and each identifier is
/// only valid for the tree that returned it.
#[derive(Debug, Default)]
pub struct Tree {
    entries: Vec<Entry>,
    directories: Vec<Directory>,
    links: Vec<Link>,
    extended: Vec<EntryExtended>,
    names: Vec<u8>,
    /// The device number of each [`DeviceId`].
    devices: Vec<u64>,
    device_ids: HashMap<u64, DeviceId>,
    root: Option<DirectoryId>,
}

impl Tree {
    /// The root directory, once a [`crate::MemorySink`] created it.
    pub fn root(&self) -> Option<DirectoryId> {
        self.root
    }

    pub fn entry(&self, id: EntryId) -> &Entry {
        &self.entries[id.0]
    }

    pub fn name(&self, id: EntryId) -> &[u8] {
        let entry = &self.entries[id.0];
        &self.names[entry.name_start..entry.name_end]
    }

    /// The extended information of `id`, which an entry only has with `-e`, and only when it
    /// has at least one of the extended fields.
    pub fn extended(&self, id: EntryId) -> Option<&EntryExtended> {
        self.entries[id.0]
            .extended
            .map(|index| &self.extended[index])
    }

    pub fn directory(&self, id: DirectoryId) -> &Directory {
        &self.directories[id.0]
    }

    pub fn link(&self, id: LinkId) -> &Link {
        &self.links[id.0]
    }

    /// The device number of `id`.
    pub fn device(&self, id: DeviceId) -> u64 {
        self.devices[id.0]
    }

    /// The entries of `directory`, last added first.
    pub fn entries(&self, directory: DirectoryId) -> impl Iterator<Item = EntryId> + '_ {
        iter::successors(self.directories[directory.0].sub, |&id| {
            self.entries[id.0].next
        })
    }

    pub(crate) fn set_root(&mut self, root: DirectoryId) {
        self.root = Some(root);
    }

    /// The identifier of `device`, which is new for a device that the tree does not have yet.
    pub(crate) fn device_id(&mut self, device: u64) -> Result<DeviceId, TreeError> {
        match self.device_ids.entry(device) {
            MapEntry::Occupied(occupied) => Ok(*occupied.get()),
            MapEntry::Vacant(vacant) => {
                if self.devices.len() >= DEVICES_MAX {
                    return Err(TreeError::TooManyDevices);
                }
                let id = DeviceId(self.devices.len());
                self.devices.push(device);
                Ok(*vacant.insert(id))
            }
        }
    }

    /// Add the directory `name` with the sizes of `stat` to `parent`, or as a directory without
    /// a parent when `parent` is `None`.
    pub(crate) fn create_directory(
        &mut self,
        parent: Option<DirectoryId>,
        device: DeviceId,
        stat: &Stat,
        extended: Option<EntryExtended>,
        name: &[u8],
    ) -> DirectoryId {
        let directory = DirectoryId(self.directories.len());
        let entry = self.push(EntryKind::Directory(directory), stat, extended, name);
        self.directories.push(Directory {
            entry,
            parent,
            items: 0,
            device,
            read_error: false,
            sub_error: false,
            sub: None,
        });
        if let Some(parent) = parent {
            self.prepend(parent, entry);
        }
        directory
    }

    /// Add the entry `name` with the sizes of `stat` to `parent`. A hard link gets the inode and
    /// the link count of `stat`.
    pub(crate) fn create(
        &mut self,
        parent: DirectoryId,
        kind: FileKind,
        stat: &Stat,
        extended: Option<EntryExtended>,
        name: &[u8],
    ) -> EntryId {
        let kind = match kind {
            FileKind::HardLink => {
                self.links.push(Link {
                    parent,
                    inode: stat.inode,
                    link_count: stat.link_count,
                });
                EntryKind::HardLink(LinkId(self.links.len() - 1))
            }
            FileKind::Regular => EntryKind::Regular,
            FileKind::NonRegular => EntryKind::NonRegular,
            FileKind::Special(special) => EntryKind::Special(special),
        };
        let entry = self.push(kind, stat, extended, name);
        self.prepend(parent, entry);
        entry
    }

    fn push(
        &mut self,
        kind: EntryKind,
        stat: &Stat,
        extended: Option<EntryExtended>,
        name: &[u8],
    ) -> EntryId {
        let extended = extended.map(|entry_extended| {
            self.extended.push(entry_extended);
            self.extended.len() - 1
        });
        let name_start = self.names.len();
        self.names.extend_from_slice(name);
        self.entries.push(Entry {
            kind,
            blocks: stat.blocks,
            size: stat.size,
            next: None,
            name_start,
            name_end: self.names.len(),
            extended,
        });
        EntryId(self.entries.len() - 1)
    }

    /// Make `entry` the first entry of `directory`.
    fn prepend(&mut self, directory: DirectoryId, entry: EntryId) {
        let directory = &mut self.directories[directory.0];
        self.entries[entry.0].next = directory.sub;
        directory.sub = Some(entry);
    }

    pub(crate) fn entry_mut(&mut self, id: EntryId) -> &mut Entry {
        &mut self.entries[id.0]
    }

    pub(crate) fn directory_mut(&mut self, id: DirectoryId) -> &mut Directory {
        &mut self.directories[id.0]
    }

    pub(crate) fn extended_mut(&mut self, id: EntryId) -> Option<&mut EntryExtended> {
        self.entries[id.0]
            .extended
            .map(|index| &mut self.extended[index])
    }

    /// Whether `id` is an entry that could not be read, or a directory with such an entry at any
    /// depth.
    fn has_error(&self, id: EntryId) -> bool {
        match self.entries[id.0].kind {
            EntryKind::Directory(directory) => {
                let directory = &self.directories[directory.0];
                directory.read_error || directory.sub_error
            }
            EntryKind::Special(Special::ReadError) => true,
            EntryKind::HardLink(_)
            | EntryKind::Regular
            | EntryKind::NonRegular
            | EntryKind::Special(
                Special::Pattern | Special::OtherFilesystem | Special::KernelFilesystem,
            ) => false,
        }
    }

    /// Set the sizes, item counts, and errors of `id` and of every entry below it to zero, and take
    /// its totals from `parent` and the directories above it, as if `id` were not in the tree.
    ///
    /// The `sub_error` of the directories above is left as it is: call
    /// [`Tree::update_sub_error`] on each of them afterwards.
    pub fn zero_stats(&mut self, id: EntryId, parent: Option<DirectoryId>) {
        let entry = &self.entries[id.0];
        let (blocks, size) = (entry.blocks, entry.size);
        let items = match entry.kind {
            EntryKind::Directory(directory) => self.directories[directory.0].items,
            EntryKind::HardLink(_)
            | EntryKind::Regular
            | EntryKind::NonRegular
            | EntryKind::Special(_) => 0,
        }
        .saturating_add(1);
        let mut next = parent;
        while let Some(directory) = next {
            let directory = &mut self.directories[directory.0];
            directory.items = directory.items.saturating_sub(items);
            next = directory.parent;
            let entry = &mut self.entries[directory.entry.0];
            entry.blocks = entry.blocks.saturating_sub(blocks);
            entry.size = entry.size.saturating_sub(size);
        }
        let mut stack = vec![id];
        while let Some(id) = stack.pop() {
            let entry = &mut self.entries[id.0];
            entry.blocks = 0;
            entry.size = 0;
            if let EntryKind::Directory(directory) = entry.kind {
                let data = &mut self.directories[directory.0];
                data.items = 0;
                data.read_error = false;
                data.sub_error = false;
                stack.extend(self.entries(directory));
            }
        }
    }

    /// Set the `sub_error` of `directory` from its entries alone. The directories below it must be
    /// up to date, and the directories above it are not changed.
    pub fn update_sub_error(&mut self, directory: DirectoryId) {
        let sub_error = self.entries(directory).any(|id| self.has_error(id));
        self.directories[directory.0].sub_error = sub_error;
    }
}

/// What [`Tree::create`] adds: any entry that is not a directory.
#[derive(Debug, Clone, Copy)]
pub(crate) enum FileKind {
    HardLink,
    Regular,
    NonRegular,
    Special(Special),
}

#[cfg(test)]
mod tests {
    use super::{EntryKind, FileKind, Tree};
    use crate::Stat;

    /// Port of the upstream test "entry".
    #[test]
    fn entry() {
        let mut tree = Tree::default();
        let device = tree.device_id(0).expect("device fits");
        let directory = tree.create_directory(None, device, &Stat::default(), None, b"");
        let id = tree.create(
            directory,
            FileKind::Regular,
            &Stat::default(),
            None,
            b"hello",
        );
        assert_eq!(tree.entry(id).kind, EntryKind::Regular);
        assert!(tree.extended(id).is_none());
        assert_eq!(tree.name(id), b"hello");
    }
}
