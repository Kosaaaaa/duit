use std::collections::hash_map::Entry as MapEntry;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::io;
use std::iter;
use std::mem;
use std::num::NonZeroU32;

use crate::{EntryType, Extended, Special, Stat, is_event_count};

/// The largest block count that ncdu stores, in 60 bits. Sums of block counts saturate here.
pub const BLOCKS_MAX: u64 = (1 << 60) - 1;

pub(crate) fn add_blocks(blocks: u64, more: u64) -> u64 {
    blocks.saturating_add(more).min(BLOCKS_MAX)
}

const DEVICES_MAX: usize = (1 << 30) - 1;

/// Why a [`Tree`] could not take an entry. It reaches the caller of a [`crate::Sink`] method as
/// the inner error of an [`io::Error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeError {
    /// The tree holds as many distinct devices as ncdu can number.
    TooManyDevices,
    /// The tree holds as many entries as it can number, 2^32 - 1, unlike ncdu, which has no
    /// such limit.
    TooManyEntries,
}

impl fmt::Display for TreeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooManyDevices => "maximum number of device identifiers exceeded",
            Self::TooManyEntries => "maximum number of entries exceeded",
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
pub struct EntryId(NonZeroU32);

/// The identifier of a [`Directory`] in its [`Tree`], in the order the tree creates directories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DirectoryId(NonZeroU32);

/// The identifier of a [`Link`] in its [`Tree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LinkId(NonZeroU32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExtendedId(NonZeroU32);

/// The identifier of a device in its [`Tree`], which [`Tree::device`] turns back into the
/// device number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceId(NonZeroU32);

fn identifier(index: usize) -> Result<NonZeroU32, TreeError> {
    u32::try_from(index)
        .ok()
        .and_then(|index| NonZeroU32::MIN.checked_add(index))
        .ok_or(TreeError::TooManyEntries)
}

fn index(identifier: NonZeroU32) -> usize {
    (identifier.get() - 1) as usize
}

impl EntryId {
    fn index(self) -> usize {
        index(self.0)
    }
}

impl DirectoryId {
    fn index(self) -> usize {
        index(self.0)
    }
}

impl LinkId {
    fn index(self) -> usize {
        index(self.0)
    }
}

impl ExtendedId {
    fn index(self) -> usize {
        index(self.0)
    }
}

impl DeviceId {
    fn index(self) -> usize {
        index(self.0)
    }
}

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
    next: Option<EntryId>,
    name_start: NameStart,
    extended: Option<ExtendedId>,
}

#[derive(Debug, Clone, Copy)]
struct NameStart(usize);

#[derive(Debug, Default, Clone)]
struct Names {
    bytes: Vec<u8>,
}

impl Names {
    fn push(&mut self, name: &[u8]) -> NameStart {
        let start = NameStart(self.bytes.len());
        self.bytes.extend_from_slice(name);
        start
    }

    fn until_next(&self, start: NameStart, next: Option<NameStart>) -> &[u8] {
        let end = next.map_or(self.bytes.len(), |next| next.0);
        &self.bytes[start.0..end]
    }
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
    /// The disk usage in 512-byte blocks of the hard links below this directory that have links
    /// outside of it, which deleting the directory would not free.
    pub shared_blocks: u64,
    /// The apparent size in bytes of the same hard links as `shared_blocks`.
    pub shared_size: u64,
    /// The directory could not be read completely.
    pub read_error: bool,
    /// An entry below this directory, at any depth, could not be read.
    pub sub_error: bool,
    sub: Option<EntryId>,
}

/// The data that a hard link has on top of its [`Entry`]. Its device is that of its parent.
#[derive(Debug, Clone)]
pub struct Link {
    pub parent: DirectoryId,
    pub inode: u64,
    /// The number of links to the inode, or 0 when it is unknown, as in old JSON exports.
    pub link_count: u32,
    pub entry: EntryId,
    next: LinkId,
    previous: LinkId,
    counted: bool,
}

type Inode = (DeviceId, u64);

#[derive(Debug, Default, Clone)]
enum UncountedInodes {
    #[default]
    All,
    Listed(HashSet<Inode>),
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

impl EntryExtended {
    /// The extended information to keep for an entry with `stat`, with `-e` (`extended`), and
    /// when `stat` has any.
    pub fn of(stat: &Stat, extended: bool) -> Option<Self> {
        (extended && stat.extended != Extended::default()).then(|| Self {
            extended: stat.extended,
            latest_mtime: stat.extended.mtime.unwrap_or(0),
        })
    }
}

/// A directory tree in memory. Entries refer to each other by identifier, and each identifier is
/// only valid for the tree that returned it.
///
/// A [`crate::MemorySink`] builds a tree with the totals of each directory. The browser reads a
/// binary export into a tree of one directory and its entries at a time, with the totals that the
/// export stores, through [`Tree::prepend_directory`] and [`Tree::prepend_file`].
#[derive(Debug, Default, Clone)]
pub struct Tree {
    entries: Vec<Entry>,
    directories: Vec<Directory>,
    links: Vec<Link>,
    inodes: HashMap<Inode, LinkId>,
    uncounted: UncountedInodes,
    counted_inodes: usize,
    extended: Vec<EntryExtended>,
    names: Names,
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
        &self.entries[id.index()]
    }

    pub fn name(&self, id: EntryId) -> &[u8] {
        let index = id.index();
        let next = self.entries.get(index + 1).map(|next| next.name_start);
        self.names.until_next(self.entries[index].name_start, next)
    }

    /// The extended information of `id`, which an entry only has with `-e`, and only when it
    /// has at least one of the extended fields.
    pub fn extended(&self, id: EntryId) -> Option<&EntryExtended> {
        self.entries[id.index()]
            .extended
            .map(|extended| &self.extended[extended.index()])
    }

    pub fn directory(&self, id: DirectoryId) -> &Directory {
        &self.directories[id.index()]
    }

    pub fn link(&self, id: LinkId) -> &Link {
        &self.links[id.index()]
    }

    /// The links to the same inode as `link`, starting with it.
    pub fn links(&self, link: LinkId) -> impl Iterator<Item = LinkId> + '_ {
        iter::successors(Some(link), move |&current| {
            Some(self.links[current.index()].next).filter(|&next| next != link)
        })
    }

    /// Append the path of `directory` to `out`: the names of the directories from the root down
    /// to it, joined with a `/` that is left out after a name that ends with one. The root is
    /// left out unless `with_root` is set, so without it the path of the root is empty.
    /// `Dir.fmtPath` in `model.zig`.
    pub fn format_path(&self, directory: DirectoryId, with_root: bool, out: &mut Vec<u8>) {
        let mut names = Vec::new();
        let mut current = Some(directory);
        while let Some(id) = current {
            let data = &self.directories[id.index()];
            if with_root || data.parent.is_some() {
                names.push(self.name(data.entry));
            }
            current = data.parent;
        }
        for (index, name) in names.into_iter().rev().enumerate() {
            if index != 0 && out.last() != Some(&b'/') {
                out.push(b'/');
            }
            out.extend_from_slice(name);
        }
    }

    /// The path of `link`: the path of its directory, as [`Tree::format_path`] gives it, a `/`,
    /// and its name. `Link.path` in `model.zig`.
    pub fn link_path(&self, link: LinkId, with_root: bool) -> Vec<u8> {
        let link = &self.links[link.index()];
        let mut path = Vec::new();
        self.format_path(link.parent, with_root, &mut path);
        path.push(b'/');
        path.extend_from_slice(self.name(link.entry));
        path
    }

    /// The device number of `id`.
    pub fn device(&self, id: DeviceId) -> u64 {
        self.devices[id.index()]
    }

    /// The entries of `directory`, last added first.
    pub fn entries(&self, directory: DirectoryId) -> impl Iterator<Item = EntryId> + '_ {
        iter::successors(self.directories[directory.index()].sub, |&id| {
            self.entries[id.index()].next
        })
    }

    pub(crate) fn set_root(&mut self, root: DirectoryId) {
        self.root = Some(root);
    }

    /// The identifier of `device`, which is new for a device that the tree does not have yet.
    pub fn device_id(&mut self, device: u64) -> Result<DeviceId, TreeError> {
        match self.device_ids.entry(device) {
            MapEntry::Occupied(occupied) => Ok(*occupied.get()),
            MapEntry::Vacant(vacant) => {
                if self.devices.len() >= DEVICES_MAX {
                    return Err(TreeError::TooManyDevices);
                }
                let id = DeviceId(
                    identifier(self.devices.len()).map_err(|_| TreeError::TooManyDevices)?,
                );
                self.devices.push(device);
                Ok(*vacant.insert(id))
            }
        }
    }

    /// Add the directory `name` with the sizes of `stat` to `parent`, or as a directory without
    /// a parent when `parent` is `None`. Returns an error, and changes nothing, when the tree
    /// holds as many entries as it can number.
    pub fn prepend_directory(
        &mut self,
        parent: Option<DirectoryId>,
        device: DeviceId,
        stat: &Stat,
        extended: Option<EntryExtended>,
        name: &[u8],
    ) -> Result<DirectoryId, TreeError> {
        let directory = DirectoryId(identifier(self.directories.len())?);
        let entry = self.push(EntryKind::Directory(directory), stat, extended, name)?;
        self.directories.push(Directory {
            entry,
            parent,
            items: 0,
            device,
            shared_blocks: 0,
            shared_size: 0,
            read_error: false,
            sub_error: false,
            sub: None,
        });
        if let Some(parent) = parent {
            self.link_first(parent, entry);
        }
        Ok(directory)
    }

    /// Add the entry `name` with the sizes of `stat` to `parent`. A hard link gets the inode and
    /// the link count of `stat`, and joins the links to the same inode, whose sizes are then
    /// left out of the totals until [`Tree::add_all_stats`]. Returns an error, and changes nothing,
    /// when the tree holds as many entries as it can number.
    pub fn prepend_file(
        &mut self,
        parent: DirectoryId,
        kind: FileKind,
        stat: &Stat,
        extended: Option<EntryExtended>,
        name: &[u8],
    ) -> Result<EntryId, TreeError> {
        let kind = match kind {
            FileKind::HardLink => EntryKind::HardLink(LinkId(identifier(self.links.len())?)),
            FileKind::Regular => EntryKind::Regular,
            FileKind::NonRegular => EntryKind::NonRegular,
            FileKind::Special(special) => EntryKind::Special(special),
        };
        let entry = self.push(kind, stat, extended, name)?;
        self.link_first(parent, entry);
        if let EntryKind::HardLink(link) = kind {
            self.links.push(Link {
                parent,
                inode: stat.inode,
                link_count: stat.link_count,
                entry,
                next: link,
                previous: link,
                counted: false,
            });
            self.add_link(link);
        }
        Ok(entry)
    }

    fn push(
        &mut self,
        kind: EntryKind,
        stat: &Stat,
        extended: Option<EntryExtended>,
        name: &[u8],
    ) -> Result<EntryId, TreeError> {
        let id = EntryId(identifier(self.entries.len())?);
        let extended = match extended {
            Some(entry_extended) => {
                let extended_id = ExtendedId(identifier(self.extended.len())?);
                self.extended.push(entry_extended);
                Some(extended_id)
            }
            None => None,
        };
        let name_start = self.names.push(name);
        self.entries.push(Entry {
            kind,
            blocks: stat.blocks,
            size: stat.size,
            next: None,
            name_start,
            extended,
        });
        Ok(id)
    }

    fn check_entry_capacity(&self) -> Result<(), TreeError> {
        identifier(self.entries.len()).map(drop)
    }

    fn link_first(&mut self, directory: DirectoryId, entry: EntryId) {
        let directory = &mut self.directories[directory.index()];
        self.entries[entry.index()].next = directory.sub;
        directory.sub = Some(entry);
    }

    pub(crate) fn entry_mut(&mut self, id: EntryId) -> &mut Entry {
        &mut self.entries[id.index()]
    }

    pub(crate) fn directory_mut(&mut self, id: DirectoryId) -> &mut Directory {
        &mut self.directories[id.index()]
    }

    /// Set the totals of `id` that a binary export stores, for a directory that is read without
    /// the entries below it. The `parent` of a directory stays out of reach, as every walk up the
    /// tree needs it to be acyclic.
    pub fn set_totals(
        &mut self,
        id: DirectoryId,
        items: u32,
        shared_blocks: u64,
        shared_size: u64,
        read_error: bool,
        sub_error: bool,
    ) {
        let directory = &mut self.directories[id.index()];
        directory.items = items;
        directory.shared_blocks = shared_blocks;
        directory.shared_size = shared_size;
        directory.read_error = read_error;
        directory.sub_error = sub_error;
    }

    pub(crate) fn extended_mut(&mut self, id: EntryId) -> Option<&mut EntryExtended> {
        self.entries[id.index()]
            .extended
            .map(|extended| &mut self.extended[extended.index()])
    }

    fn has_error(&self, id: EntryId) -> bool {
        match self.entries[id.index()].kind {
            EntryKind::Directory(directory) => {
                let directory = &self.directories[directory.index()];
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
    /// [`Tree::update_sub_error`] on each of them afterwards. The hard links below `id` leave the
    /// accounting, and the other links to their inodes are left out of the totals: call
    /// [`Tree::add_all_stats`] afterwards.
    pub fn zero_stats(&mut self, id: EntryId, parent: Option<DirectoryId>) {
        self.remove_links(id);
        let entry = &self.entries[id.index()];
        let (blocks, size) = (entry.blocks, entry.size);
        let items = match entry.kind {
            EntryKind::Directory(directory) => self.directories[directory.index()].items,
            EntryKind::HardLink(_)
            | EntryKind::Regular
            | EntryKind::NonRegular
            | EntryKind::Special(_) => 0,
        }
        .saturating_add(1);
        let mut next = parent;
        while let Some(directory) = next {
            let directory = &mut self.directories[directory.index()];
            directory.items = directory.items.saturating_sub(items);
            next = directory.parent;
            let entry = &mut self.entries[directory.entry.index()];
            entry.blocks = entry.blocks.saturating_sub(blocks);
            entry.size = entry.size.saturating_sub(size);
        }
        let mut stack = vec![id];
        while let Some(id) = stack.pop() {
            let entry = &mut self.entries[id.index()];
            entry.blocks = 0;
            entry.size = 0;
            if let EntryKind::Directory(directory) = entry.kind {
                let data = &mut self.directories[directory.index()];
                data.items = 0;
                data.read_error = false;
                data.sub_error = false;
                stack.extend(self.entries(directory));
            }
        }
    }

    /// Zero `id` as [`Tree::zero_stats`] does, with `directory` as its parent, and take it out of
    /// the entries of `directory`, where it follows `previous`, or is the first entry when
    /// `previous` is `None`.
    pub fn remove(&mut self, directory: DirectoryId, previous: Option<EntryId>, id: EntryId) {
        self.zero_stats(id, Some(directory));
        let next = self.entries[id.index()].next;
        match previous {
            Some(previous) => self.entries[previous.index()].next = next,
            None => self.directories[directory.index()].sub = next,
        }
    }

    /// Replace `id` of `directory`, where it follows `previous`, or is the first entry when
    /// `previous` is `None`, with a new entry of the same name, the type and the sizes of `stat`,
    /// and `extended`, at the same place, as `deleteCmd` in `delete.zig` does with what a command
    /// left at the path of an entry. `id` is zeroed as [`Tree::zero_stats`] does, and the new entry
    /// is added to the item counts of `directory` and of the directories above it, and to their
    /// sizes unless it is a hard link: call [`Tree::add_all_stats`] afterwards. The latest mtime of
    /// the directories stays, as in ncdu. Returns the new entry, or an error, and changes nothing,
    /// when a new directory is on a device that the tree cannot number, or when the tree holds as
    /// many entries as it can number.
    pub fn replace(
        &mut self,
        directory: DirectoryId,
        previous: Option<EntryId>,
        id: EntryId,
        stat: &Stat,
        extended: Option<EntryExtended>,
    ) -> Result<EntryId, TreeError> {
        self.check_entry_capacity()?;
        let name = self.name(id).to_vec();
        let kind = match stat.entry_type {
            EntryType::Directory => None,
            EntryType::Regular => Some(FileKind::Regular),
            EntryType::NonRegular => Some(FileKind::NonRegular),
            EntryType::HardLink => Some(FileKind::HardLink),
        };
        let entry = if let Some(kind) = kind {
            self.remove(directory, previous, id);
            self.prepend_file(directory, kind, stat, extended, &name)?
        } else {
            let device = self.device_id(stat.device)?;
            self.remove(directory, previous, id);
            let created = self.prepend_directory(Some(directory), device, stat, extended, &name)?;
            self.directories[created.index()].entry
        };
        if let Some(previous) = previous {
            self.move_first_after(directory, previous, entry);
        }
        let (blocks, size) = match stat.entry_type {
            EntryType::HardLink => (0, 0),
            EntryType::Directory | EntryType::Regular | EntryType::NonRegular => {
                (stat.blocks, stat.size)
            }
        };
        let mut next = Some(directory);
        while let Some(current) = next {
            let data = &mut self.directories[current.index()];
            data.items = data.items.saturating_add(1);
            next = data.parent;
            let entry = &mut self.entries[data.entry.index()];
            entry.blocks = add_blocks(entry.blocks, blocks);
            entry.size = entry.size.saturating_add(size);
        }
        Ok(entry)
    }

    fn move_first_after(&mut self, directory: DirectoryId, previous: EntryId, first: EntryId) {
        self.directories[directory.index()].sub = self.entries[first.index()].next;
        self.entries[first.index()].next = self.entries[previous.index()].next;
        self.entries[previous.index()].next = Some(first);
    }

    pub(crate) fn retain(&mut self, directory: DirectoryId, mut keep: impl FnMut(EntryId) -> bool) {
        let mut previous = None;
        let mut next = self.directories[directory.index()].sub;
        while let Some(id) = next {
            next = self.entries[id.index()].next;
            if keep(id) {
                previous = Some(id);
            } else {
                match previous {
                    Some(previous) => self.entries[previous.index()].next = next,
                    None => self.directories[directory.index()].sub = next,
                }
            }
        }
    }

    pub(crate) fn reuse_directory(
        &mut self,
        id: EntryId,
        device: DeviceId,
        stat: &Stat,
        extended: Option<EntryExtended>,
    ) -> Option<DirectoryId> {
        let directory = match self.entries[id.index()].kind {
            EntryKind::Directory(directory) => directory,
            EntryKind::HardLink(_)
            | EntryKind::Regular
            | EntryKind::NonRegular
            | EntryKind::Special(_) => return None,
        };
        if !self.refill(id, stat, extended) {
            return None;
        }
        self.directories[directory.index()].device = device;
        Some(directory)
    }

    pub(crate) fn reuse(
        &mut self,
        id: EntryId,
        parent: DirectoryId,
        kind: FileKind,
        stat: &Stat,
        extended: Option<EntryExtended>,
    ) -> bool {
        let kind = match (self.entries[id.index()].kind, kind) {
            (EntryKind::HardLink(link), FileKind::HardLink) => EntryKind::HardLink(link),
            (
                EntryKind::Regular | EntryKind::NonRegular | EntryKind::Special(_),
                FileKind::Regular,
            ) => EntryKind::Regular,
            (
                EntryKind::Regular | EntryKind::NonRegular | EntryKind::Special(_),
                FileKind::NonRegular,
            ) => EntryKind::NonRegular,
            (
                EntryKind::Regular | EntryKind::NonRegular | EntryKind::Special(_),
                FileKind::Special(special),
            ) => EntryKind::Special(special),
            (
                EntryKind::Directory(_),
                FileKind::HardLink
                | FileKind::Regular
                | FileKind::NonRegular
                | FileKind::Special(_),
            )
            | (
                EntryKind::HardLink(_),
                FileKind::Regular | FileKind::NonRegular | FileKind::Special(_),
            )
            | (
                EntryKind::Regular | EntryKind::NonRegular | EntryKind::Special(_),
                FileKind::HardLink,
            ) => return false,
        };
        if !self.refill(id, stat, extended) {
            return false;
        }
        self.entries[id.index()].kind = kind;
        if let EntryKind::HardLink(link) = kind {
            let data = &mut self.links[link.index()];
            data.parent = parent;
            data.inode = stat.inode;
            data.link_count = stat.link_count;
            data.next = link;
            data.previous = link;
            data.counted = false;
            self.add_link(link);
        }
        true
    }

    fn refill(&mut self, id: EntryId, stat: &Stat, extended: Option<EntryExtended>) -> bool {
        let entry = &mut self.entries[id.index()];
        match (entry.extended, extended) {
            (None, Some(_)) => return false,
            (Some(id), Some(extended)) => self.extended[id.index()] = extended,
            (Some(_), None) => entry.extended = None,
            (None, None) => {}
        }
        entry.blocks = stat.blocks;
        entry.size = stat.size;
        true
    }

    fn remove_links(&mut self, id: EntryId) {
        let mut stack = vec![id];
        while let Some(id) = stack.pop() {
            match self.entries[id.index()].kind {
                EntryKind::Directory(directory) => {
                    let start = stack.len();
                    stack.extend(self.entries(directory));
                    stack[start..].reverse();
                }
                EntryKind::HardLink(link) => self.remove_link(link),
                EntryKind::Regular | EntryKind::NonRegular | EntryKind::Special(_) => {}
            }
        }
    }

    fn inode(&self, link: LinkId) -> Inode {
        let link = &self.links[link.index()];
        (self.directories[link.parent.index()].device, link.inode)
    }

    fn add_link(&mut self, link: LinkId) {
        let inode = self.inode(link);
        match self.inodes.entry(inode) {
            MapEntry::Vacant(vacant) => {
                vacant.insert(link);
            }
            MapEntry::Occupied(occupied) => {
                let first = *occupied.get();
                self.remove_stats(first);
                let previous = self.links[first.index()].previous;
                self.links[link.index()].next = first;
                self.links[link.index()].previous = previous;
                self.links[first.index()].previous = link;
                self.links[previous.index()].next = link;
            }
        }
        self.add_uncounted(inode);
    }

    fn remove_link(&mut self, link: LinkId) {
        self.remove_stats(link);
        let inode = self.inode(link);
        let Some(&first) = self.inodes.get(&inode) else {
            return;
        };
        let Link { next, previous, .. } = self.links[link.index()];
        if next == link {
            self.inodes.remove(&inode);
            if let UncountedInodes::Listed(uncounted) = &mut self.uncounted {
                uncounted.remove(&inode);
            }
        } else {
            if first == link {
                self.inodes.insert(inode, next);
            }
            self.add_uncounted(inode);
            self.links[next.index()].previous = previous;
            self.links[previous.index()].next = next;
        }
    }

    fn add_uncounted(&mut self, inode: Inode) {
        if let UncountedInodes::Listed(uncounted) = &mut self.uncounted {
            if uncounted.len() > self.inodes.len() / 8 {
                self.uncounted = UncountedInodes::All;
            } else {
                uncounted.insert(inode);
            }
        }
    }

    fn add_stats(&mut self, link: LinkId) {
        let Some(totals) = self.inode_totals(link, true) else {
            return;
        };
        for (directory, count) in totals.directories {
            let data = &mut self.directories[directory.index()];
            if count < totals.link_count {
                data.shared_blocks = data.shared_blocks.saturating_add(totals.blocks);
                data.shared_size = data.shared_size.saturating_add(totals.size);
            }
            let entry = &mut self.entries[data.entry.index()];
            entry.blocks = add_blocks(entry.blocks, totals.blocks);
            entry.size = entry.size.saturating_add(totals.size);
        }
    }

    fn remove_stats(&mut self, link: LinkId) {
        let Some(totals) = self.inode_totals(link, false) else {
            return;
        };
        for (directory, count) in totals.directories {
            let data = &mut self.directories[directory.index()];
            if count < totals.link_count {
                data.shared_blocks = data.shared_blocks.saturating_sub(totals.blocks);
                data.shared_size = data.shared_size.saturating_sub(totals.size);
            }
            let entry = &mut self.entries[data.entry.index()];
            entry.blocks = entry.blocks.saturating_sub(totals.blocks);
            entry.size = entry.size.saturating_sub(totals.size);
        }
    }

    fn inode_totals(&mut self, link: LinkId, counted: bool) -> Option<InodeTotals> {
        if self.links[link.index()].counted == counted {
            return None;
        }
        let link_count = self.links[link.index()].link_count;
        let mut found: u32 = 0;
        let mut inconsistent = false;
        let mut parents: HashMap<DirectoryId, u32> = HashMap::new();
        let mut current = link;
        loop {
            let data = &mut self.links[current.index()];
            data.counted = counted;
            found = found.saturating_add(1);
            if data.link_count != link_count {
                inconsistent = true;
            }
            current = data.next;
            let count = parents.entry(data.parent).or_insert(0);
            *count = count.saturating_add(1);
            if current == link {
                break;
            }
        }
        let (mut directories, bottom_up) = self.ancestors_bottom_up(&parents);
        for directory in bottom_up {
            if let Some(parent) = self.directories[directory.index()].parent
                && let Some(&count) = directories.get(&directory)
                && let Some(total) = directories.get_mut(&parent)
            {
                *total = total.saturating_add(count);
            }
        }
        let link_count = if !inconsistent && link_count >= found {
            link_count
        } else {
            found
        };
        let Entry { blocks, size, .. } = self.entries[self.links[link.index()].entry.index()];
        Some(InodeTotals {
            directories,
            link_count,
            blocks,
            size,
        })
    }

    fn ancestors_bottom_up(
        &self,
        parents: &HashMap<DirectoryId, u32>,
    ) -> (HashMap<DirectoryId, u32>, Vec<DirectoryId>) {
        let mut directories: HashMap<DirectoryId, u32> = HashMap::new();
        let mut order: Vec<DirectoryId> = Vec::new();
        for &start in parents.keys() {
            let walk_start = order.len();
            let mut parent = Some(start);
            while let Some(directory) = parent
                && !directories.contains_key(&directory)
            {
                directories.insert(directory, parents.get(&directory).copied().unwrap_or(0));
                order.push(directory);
                parent = self.directories[directory.index()].parent;
            }
            order[walk_start..].reverse();
        }
        order.reverse();
        (directories, order)
    }

    /// Add the sizes of the inodes that are left out of the totals, as after adding or removing
    /// hard links, `addAllStats` in `model.zig`. `progress` gets how many of how many inodes are
    /// done whenever bits 0 and 6 of the count are clear, where ncdu calls `handleEvent`.
    pub fn add_all_stats(&mut self, progress: &mut dyn FnMut(usize, usize)) {
        let inodes = mem::take(&mut self.inodes);
        let uncounted = mem::take(&mut self.uncounted);
        let (total, links): (usize, Box<dyn Iterator<Item = Option<LinkId>>>) = match &uncounted {
            UncountedInodes::All => (inodes.len(), Box::new(inodes.values().copied().map(Some))),
            UncountedInodes::Listed(uncounted) => (
                uncounted.len(),
                Box::new(uncounted.iter().map(|inode| inodes.get(inode).copied())),
            ),
        };
        for (done, link) in (1..).zip(links) {
            if let Some(link) = link {
                self.add_stats(link);
            }
            if is_event_count(done) {
                progress(done, total);
            }
        }
        self.counted_inodes = total;
        self.inodes = inodes;
        self.uncounted = UncountedInodes::Listed(HashSet::new());
    }

    /// How many inodes the last [`Tree::add_all_stats`] went through, which the first frame of the
    /// next count shows before it starts, as ncdu keeps its counters until then.
    pub fn counted_inodes(&self) -> usize {
        self.counted_inodes
    }

    /// Set the `sub_error` of `directory` from its entries alone. The directories below it must be
    /// up to date, and the directories above it are not changed.
    pub fn update_sub_error(&mut self, directory: DirectoryId) {
        let sub_error = self.entries(directory).any(|id| self.has_error(id));
        self.directories[directory.index()].sub_error = sub_error;
    }
}

struct InodeTotals {
    directories: HashMap<DirectoryId, u32>,
    link_count: u32,
    blocks: u64,
    size: u64,
}

/// What [`Tree::prepend_file`] adds: any entry that is not a directory.
#[derive(Debug, Clone, Copy)]
pub enum FileKind {
    HardLink,
    Regular,
    NonRegular,
    Special(Special),
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::{
        Directory, DirectoryId, Entry, EntryId, EntryKind, FileKind, Link, Tree, TreeError,
        identifier,
    };
    use crate::Stat;

    #[test]
    fn entry() {
        let mut tree = Tree::default();
        let device = tree.device_id(0).expect("device fits");
        let directory = tree
            .prepend_directory(None, device, &Stat::default(), None, b"")
            .expect("entry fits");
        let id = tree
            .prepend_file(
                directory,
                FileKind::Regular,
                &Stat::default(),
                None,
                b"hello",
            )
            .expect("entry fits");
        assert_eq!(tree.entry(id).kind, EntryKind::Regular);
        assert!(tree.extended(id).is_none());
        assert_eq!(tree.name(id), b"hello");
    }

    #[test]
    fn paths() {
        let mut tree = Tree::default();
        let device = tree.device_id(0).expect("device fits");
        let stat = Stat::default();
        let mut directory = |parent, name: &[u8]| {
            tree.prepend_directory(parent, device, &stat, None, name)
                .expect("entry fits")
        };
        let root = directory(None, b"/");
        let first = directory(Some(root), b"a");
        let second = directory(Some(first), b"b");
        let path = |directory, with_root| {
            let mut path = Vec::new();
            tree.format_path(directory, with_root, &mut path);
            String::from_utf8(path).expect("path is UTF-8")
        };
        assert_eq!(path(root, false), "");
        assert_eq!(path(root, true), "/");
        assert_eq!(path(second, false), "a/b");
        assert_eq!(path(second, true), "/a/b");

        let link = |tree: &mut Tree, parent, name: &[u8]| {
            let entry = tree
                .prepend_file(parent, FileKind::HardLink, &stat, None, name)
                .expect("entry fits");
            match tree.entry(entry).kind {
                EntryKind::HardLink(link) => Some(link),
                EntryKind::Directory(_)
                | EntryKind::Regular
                | EntryKind::NonRegular
                | EntryKind::Special(_) => None,
            }
            .expect("entry is a hard link")
        };
        let top = link(&mut tree, root, b"x");
        let deep = link(&mut tree, second, b"y");
        let link_path = |link, with_root| {
            String::from_utf8(tree.link_path(link, with_root)).expect("path is UTF-8")
        };
        assert_eq!(link_path(top, false), "/x");
        assert_eq!(link_path(top, true), "//x");
        assert_eq!(link_path(deep, false), "a/b/y");
        assert_eq!(link_path(deep, true), "/a/b/y");
        assert_eq!(tree.links(top).collect::<Vec<_>>(), [top, deep]);
        assert_eq!(tree.links(deep).collect::<Vec<_>>(), [deep, top]);
    }

    #[test]
    fn identifiers() {
        let last = usize::try_from(u32::MAX - 1).expect("u32 fits in usize");
        assert_eq!(identifier(0).map(NonZeroU32::get), Ok(1));
        assert_eq!(identifier(last).map(NonZeroU32::get), Ok(u32::MAX));
        assert_eq!(identifier(last + 1), Err(TreeError::TooManyEntries));
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn sizes() {
        assert_eq!(size_of::<Option<EntryId>>(), 4);
        assert_eq!(size_of::<Option<DirectoryId>>(), 4);
        assert_eq!(size_of::<EntryKind>(), 8);
        assert_eq!(size_of::<Entry>(), 40);
        assert_eq!(size_of::<Directory>(), 40);
        assert_eq!(size_of::<Link>(), 32);
    }
}
