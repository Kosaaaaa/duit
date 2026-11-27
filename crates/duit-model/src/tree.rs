use std::collections::hash_map::Entry as MapEntry;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::hash::{BuildHasher, BuildHasherDefault, Hasher, RandomState};
use std::io;
use std::iter;
use std::mem;
use std::num::NonZeroU32;

use hashbrown::HashTable;

use crate::{EntryType, Extended, Special, Stat, is_event_count};

/// The largest block count that ncdu stores, in 60 bits. Sums of block counts saturate here.
pub const BLOCKS_MAX: u64 = (1 << 60) - 1;

pub(crate) fn add_blocks(blocks: u64, more: u64) -> u64 {
    blocks.saturating_add(more).min(BLOCKS_MAX)
}

/// The number of device identifiers that ncdu can store, in 30 bits, `DevId` in `model.zig`.
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

/// The identifier of an [`Entry`] in its [`Tree`], in the order the tree creates entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntryId(NonZeroU32);

/// The identifier of a [`Directory`] in its [`Tree`], in the order the tree creates directories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DirectoryId(NonZeroU32);

/// The identifier of a [`Link`] in its [`Tree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LinkId(NonZeroU32);

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

/// The identifier at `index` of a list of the tree, which holds fewer items than identifiers.
fn index_identifier(index: usize) -> NonZeroU32 {
    identifier(index).unwrap_or(NonZeroU32::MAX)
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

/// An entry of a [`Tree`], as [`Tree::entry`] reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub kind: EntryKind,
    /// The disk usage in 512-byte blocks. For a directory, it includes the entries below it.
    pub blocks: u64,
    /// The apparent size in bytes. For a directory, it includes the entries below it.
    pub size: u64,
}

/// What a [`Node`] is: an [`EntryKind`] without the identifier of the data of a directory or a
/// hard link, which the tree finds by the identifier of the entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tag {
    Directory,
    HardLink,
    Regular,
    NonRegular,
    Special(Special),
}

impl Tag {
    fn of(kind: EntryKind) -> Self {
        match kind {
            EntryKind::Directory(_) => Self::Directory,
            EntryKind::HardLink(_) => Self::HardLink,
            EntryKind::Regular => Self::Regular,
            EntryKind::NonRegular => Self::NonRegular,
            EntryKind::Special(special) => Self::Special(special),
        }
    }

    fn bits(self) -> u64 {
        match self {
            Self::Directory => 0,
            Self::HardLink => 1,
            Self::Regular => 2,
            Self::NonRegular => 3,
            Self::Special(Special::ReadError) => 4,
            Self::Special(Special::Pattern) => 5,
            Self::Special(Special::OtherFilesystem) => 6,
            Self::Special(Special::KernelFilesystem) => 7,
        }
    }

    fn from_bits(bits: u64) -> Self {
        match bits & 7 {
            0 => Self::Directory,
            1 => Self::HardLink,
            2 => Self::Regular,
            3 => Self::NonRegular,
            4 => Self::Special(Special::ReadError),
            5 => Self::Special(Special::Pattern),
            6 => Self::Special(Special::OtherFilesystem),
            _ => Self::Special(Special::KernelFilesystem),
        }
    }
}

const TAG_SHIFT: u32 = 60;
const EXTENDED_BIT: u64 = 1 << 63;

/// An entry as the tree stores it, in 24 bytes, as ncdu stores a file without its name.
#[derive(Debug, Clone)]
pub(crate) struct Node {
    /// The block count in the low 60 bits, then the [`Tag`] in 3 bits, then whether the entry
    /// has extended information, as `Entry.Packed` in `model.zig`.
    packed: u64,
    pub(crate) size: u64,
    next: Option<EntryId>,
    /// The low 32 bits of the offset of the name, whose high bits [`Names`] keeps.
    name_start: u32,
}

const _: () = assert!(size_of::<Node>() == 24);

impl Node {
    fn new(tag: Tag, blocks: u64, size: u64, extended: bool, name_start: u32) -> Self {
        let mut node = Self {
            packed: 0,
            size,
            next: None,
            name_start,
        };
        node.set_tag(tag);
        node.set_blocks(blocks);
        node.set_extended(extended);
        node
    }

    pub(crate) fn blocks(&self) -> u64 {
        self.packed & BLOCKS_MAX
    }

    pub(crate) fn set_blocks(&mut self, blocks: u64) {
        self.packed = (self.packed & !BLOCKS_MAX) | blocks.min(BLOCKS_MAX);
    }

    fn tag(&self) -> Tag {
        Tag::from_bits(self.packed >> TAG_SHIFT)
    }

    fn set_tag(&mut self, tag: Tag) {
        self.packed = (self.packed & !(7 << TAG_SHIFT)) | (tag.bits() << TAG_SHIFT);
    }

    fn has_extended(&self) -> bool {
        self.packed & EXTENDED_BIT != 0
    }

    fn set_extended(&mut self, extended: bool) {
        if extended {
            self.packed |= EXTENDED_BIT;
        } else {
            self.packed &= !EXTENDED_BIT;
        }
    }
}

/// The names of the entries, one after the other, so that a name ends where the name of the next
/// entry starts. An entry keeps the low 32 bits of the offset of its name, and `high` the first
/// entry of each 4 GiB after the first, as the names grow in the order of the entries.
#[derive(Debug, Default, Clone)]
struct Names {
    bytes: Vec<u8>,
    high: Vec<EntryId>,
}

impl Names {
    /// Add the name of the new entry `id`, and return the low bits of its offset.
    fn push(&mut self, id: EntryId, name: &[u8]) -> u32 {
        let start = u64::try_from(self.bytes.len()).unwrap_or(u64::MAX);
        while u64::try_from(self.high.len()).unwrap_or(u64::MAX) < start >> u32::BITS {
            self.high.push(id);
        }
        self.bytes.extend_from_slice(name);
        u32::try_from(start & u64::from(u32::MAX)).unwrap_or(u32::MAX)
    }

    /// The offset of the name of `id`, whose low bits are `low`.
    fn start(&self, id: EntryId, low: u32) -> usize {
        let high = self.high.partition_point(|&first| first <= id);
        let high = u64::try_from(high).unwrap_or(u64::MAX);
        usize::try_from(high << u32::BITS | u64::from(low)).unwrap_or(usize::MAX)
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
    pub entry: EntryId,
    /// The inode in two halves, so that a link takes 28 bytes rather than 32.
    inode: [u32; 2],
    /// The link count in the low 31 bits, as `Link.Pack` in `model.zig`, then whether the inode is
    /// counted in the totals of the directories above its links.
    pack: u32,
    next: LinkId,
    previous: LinkId,
}

const COUNTED_BIT: u32 = 1 << 31;

impl Link {
    fn new(parent: DirectoryId, entry: EntryId, stat: &Stat, link: LinkId) -> Self {
        let mut data = Self {
            parent,
            entry,
            inode: [0; 2],
            pack: 0,
            next: link,
            previous: link,
        };
        data.set_stat(stat);
        data
    }

    fn set_stat(&mut self, stat: &Stat) {
        let half = |bits: u64| u32::try_from(bits & u64::from(u32::MAX)).unwrap_or(u32::MAX);
        self.inode = [half(stat.inode >> u32::BITS), half(stat.inode)];
        self.pack = stat.link_count.min(COUNTED_BIT - 1);
    }

    pub fn inode(&self) -> u64 {
        u64::from(self.inode[0]) << u32::BITS | u64::from(self.inode[1])
    }

    /// The number of links to the inode, or 0 when it is unknown, as in old JSON exports.
    pub fn link_count(&self) -> u32 {
        self.pack & !COUNTED_BIT
    }

    fn counted(&self) -> bool {
        self.pack & COUNTED_BIT != 0
    }

    fn set_counted(&mut self, counted: bool) {
        if counted {
            self.pack |= COUNTED_BIT;
        } else {
            self.pack &= !COUNTED_BIT;
        }
    }
}

type Inode = (DeviceId, u64);

fn link_inode(links: &[Link], directories: &[Directory], link: LinkId) -> Inode {
    let link = &links[link.index()];
    (directories[link.parent.index()].device, link.inode())
}

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
    entries: Vec<Node>,
    directories: Vec<Directory>,
    links: Vec<Link>,
    /// The first link to each inode, found by the inode of the link.
    inodes: HashTable<LinkId>,
    inode_hasher: RandomState,
    uncounted: UncountedInodes,
    counted_inodes: usize,
    scratch: InodeScratch,
    /// The extended information of the entries that have it, sorted by entry.
    extended: Vec<(EntryId, EntryExtended)>,
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

    pub fn entry(&self, id: EntryId) -> Entry {
        let node = &self.entries[id.index()];
        Entry {
            kind: self.kind(id),
            blocks: node.blocks(),
            size: node.size,
        }
    }

    /// The kind of `id`. A directory or a hard link is found among the others by its entry, as
    /// the tree adds each of them with an entry after those of the others.
    fn kind(&self, id: EntryId) -> EntryKind {
        match self.entries[id.index()].tag() {
            Tag::Directory => {
                let index = self
                    .directories
                    .partition_point(|directory| directory.entry < id);
                EntryKind::Directory(DirectoryId(index_identifier(index)))
            }
            Tag::HardLink => {
                let index = self.links.partition_point(|link| link.entry < id);
                EntryKind::HardLink(LinkId(index_identifier(index)))
            }
            Tag::Regular => EntryKind::Regular,
            Tag::NonRegular => EntryKind::NonRegular,
            Tag::Special(special) => EntryKind::Special(special),
        }
    }

    pub fn name(&self, id: EntryId) -> &[u8] {
        let index = id.index();
        let start = self.names.start(id, self.entries[index].name_start);
        let end = match self.entries.get(index + 1) {
            Some(next) => self
                .names
                .start(EntryId(index_identifier(index + 1)), next.name_start),
            None => self.names.bytes.len(),
        };
        &self.names.bytes[start..end]
    }

    /// The extended information of `id`, which an entry only has with `-e`, and only when it
    /// has at least one of the extended fields.
    pub fn extended(&self, id: EntryId) -> Option<&EntryExtended> {
        if !self.entries[id.index()].has_extended() {
            return None;
        }
        let index = self.extended.partition_point(|(entry, _)| *entry < id);
        self.extended.get(index).map(|(_, extended)| extended)
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
            self.links.push(Link::new(parent, entry, stat, link));
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
        let has_extended = extended.is_some();
        if let Some(entry_extended) = extended {
            self.extended.push((id, entry_extended));
        }
        let name_start = self.names.push(id, name);
        self.entries.push(Node::new(
            Tag::of(kind),
            stat.blocks,
            stat.size,
            has_extended,
            name_start,
        ));
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

    pub(crate) fn entry_mut(&mut self, id: EntryId) -> &mut Node {
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
        if !self.entries[id.index()].has_extended() {
            return None;
        }
        let index = self.extended.partition_point(|(entry, _)| *entry < id);
        self.extended.get_mut(index).map(|(_, extended)| extended)
    }

    fn has_error(&self, id: EntryId) -> bool {
        match self.kind(id) {
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
        let (blocks, size) = (entry.blocks(), entry.size);
        let items = match self.kind(id) {
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
            entry.set_blocks(entry.blocks().saturating_sub(blocks));
            entry.size = entry.size.saturating_sub(size);
        }
        let mut stack = vec![id];
        while let Some(id) = stack.pop() {
            let entry = &mut self.entries[id.index()];
            entry.set_blocks(0);
            entry.size = 0;
            if let EntryKind::Directory(directory) = self.kind(id) {
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
            entry.set_blocks(add_blocks(entry.blocks(), blocks));
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
        let directory = match self.kind(id) {
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
        let kind = match (self.kind(id), kind) {
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
        self.entries[id.index()].set_tag(Tag::of(kind));
        if let EntryKind::HardLink(link) = kind {
            let data = &mut self.links[link.index()];
            *data = Link::new(parent, data.entry, stat, link);
            self.add_link(link);
        }
        true
    }

    fn refill(&mut self, id: EntryId, stat: &Stat, extended: Option<EntryExtended>) -> bool {
        match (self.extended_mut(id), extended) {
            (None, Some(_)) => return false,
            (Some(entry_extended), Some(extended)) => *entry_extended = extended,
            (Some(_), None) => self.entries[id.index()].set_extended(false),
            (None, None) => {}
        }
        let entry = &mut self.entries[id.index()];
        entry.set_blocks(stat.blocks);
        entry.size = stat.size;
        true
    }

    fn remove_links(&mut self, id: EntryId) {
        let mut stack = vec![id];
        while let Some(id) = stack.pop() {
            match self.kind(id) {
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
        link_inode(&self.links, &self.directories, link)
    }

    fn add_link(&mut self, link: LinkId) {
        let inode = self.inode(link);
        let hash = self.inode_hasher.hash_one(inode);
        let Self {
            inodes,
            inode_hasher,
            links,
            directories,
            ..
        } = self;
        let first = inodes
            .find(hash, |&candidate| {
                link_inode(links, directories, candidate) == inode
            })
            .copied();
        match first {
            None => {
                inodes.insert_unique(hash, link, |&stored| {
                    inode_hasher.hash_one(link_inode(links, directories, stored))
                });
            }
            Some(first) => {
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
        let hash = self.inode_hasher.hash_one(inode);
        let Self {
            inodes,
            links,
            directories,
            ..
        } = self;
        let Ok(stored) = inodes.find_entry(hash, |&candidate| {
            link_inode(links, directories, candidate) == inode
        }) else {
            return;
        };
        let Link { next, previous, .. } = links[link.index()];
        if next == link {
            stored.remove();
            if let UncountedInodes::Listed(uncounted) = &mut self.uncounted {
                uncounted.remove(&inode);
            }
        } else {
            let mut stored = stored;
            let first = stored.get_mut();
            if *first == link {
                *first = next;
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
        let scratch = mem::take(&mut self.scratch);
        for (&directory, &count) in &scratch.directories {
            let data = &mut self.directories[directory.index()];
            if count < totals.link_count {
                data.shared_blocks = data.shared_blocks.saturating_add(totals.blocks);
                data.shared_size = data.shared_size.saturating_add(totals.size);
            }
            let entry = &mut self.entries[data.entry.index()];
            entry.set_blocks(add_blocks(entry.blocks(), totals.blocks));
            entry.size = entry.size.saturating_add(totals.size);
        }
        self.scratch = scratch;
    }

    fn remove_stats(&mut self, link: LinkId) {
        let Some(totals) = self.inode_totals(link, false) else {
            return;
        };
        let scratch = mem::take(&mut self.scratch);
        for (&directory, &count) in &scratch.directories {
            let data = &mut self.directories[directory.index()];
            if count < totals.link_count {
                data.shared_blocks = data.shared_blocks.saturating_sub(totals.blocks);
                data.shared_size = data.shared_size.saturating_sub(totals.size);
            }
            let entry = &mut self.entries[data.entry.index()];
            entry.set_blocks(entry.blocks().saturating_sub(totals.blocks));
            entry.size = entry.size.saturating_sub(totals.size);
        }
        self.scratch = scratch;
    }

    /// Mark the links to the inode of `link` as `counted`, and fill `self.scratch.directories`
    /// with the directories above them and the number of links below each. `None`, and nothing
    /// changes, if the links are marked so already.
    fn inode_totals(&mut self, link: LinkId, counted: bool) -> Option<InodeTotals> {
        if self.links[link.index()].counted() == counted {
            return None;
        }
        let link_count = self.links[link.index()].link_count();
        let mut found: u32 = 0;
        let mut inconsistent = false;
        let InodeScratch {
            parents,
            directories,
            order,
        } = &mut self.scratch;
        parents.clear();
        directories.clear();
        order.clear();
        let mut current = link;
        loop {
            let data = &mut self.links[current.index()];
            data.set_counted(counted);
            found = found.saturating_add(1);
            if data.link_count() != link_count {
                inconsistent = true;
            }
            current = data.next;
            let count = parents.entry(data.parent).or_insert(0);
            *count = count.saturating_add(1);
            if current == link {
                break;
            }
        }
        // Each directory above a link, and the number of links right in it, with every directory
        // after the directories below it.
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
        for &directory in order.iter().rev() {
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
        let entry = &self.entries[self.links[link.index()].entry.index()];
        let (blocks, size) = (entry.blocks(), entry.size);
        Some(InodeTotals {
            link_count,
            blocks,
            size,
        })
    }

    /// Add the sizes of the inodes that are left out of the totals, as after adding or removing
    /// hard links, `addAllStats` in `model.zig`. `progress` gets how many of how many inodes are
    /// done whenever bits 0 and 6 of the count are clear, where ncdu calls `handleEvent`.
    pub fn add_all_stats(&mut self, progress: &mut dyn FnMut(usize, usize)) {
        let inodes = mem::take(&mut self.inodes);
        let uncounted = mem::take(&mut self.uncounted);
        let links: Vec<Option<LinkId>> = match &uncounted {
            UncountedInodes::All => inodes.iter().copied().map(Some).collect(),
            UncountedInodes::Listed(uncounted) => uncounted
                .iter()
                .map(|&inode| {
                    let hash = self.inode_hasher.hash_one(inode);
                    inodes
                        .find(hash, |&link| self.inode(link) == inode)
                        .copied()
                })
                .collect(),
        };
        let total = links.len();
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
    link_count: u32,
    blocks: u64,
    size: u64,
}

/// The memory of [`Tree::inode_totals`], kept from one inode to the next.
#[derive(Debug, Default, Clone)]
struct InodeScratch {
    parents: DirectoryMap,
    directories: DirectoryMap,
    order: Vec<DirectoryId>,
}

/// A count for each of a set of directories. The tree numbers its directories itself, so a
/// multiplication spreads their identifiers well enough, where the default hasher would cost
/// more than the rest of the hard link accounting.
type DirectoryMap = HashMap<DirectoryId, u32, BuildHasherDefault<IdentifierHasher>>;

#[derive(Default)]
struct IdentifierHasher(u64);

/// 2^64 divided by the golden ratio, an odd number whose multiples spread over all bits.
const MULTIPLIER: u64 = 0x9e37_79b9_7f4a_7c15;

impl Hasher for IdentifierHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = (self.0.rotate_left(8) ^ u64::from(byte)).wrapping_mul(MULTIPLIER);
        }
    }

    fn write_u32(&mut self, value: u32) {
        self.write_u64(u64::from(value));
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = value.wrapping_mul(MULTIPLIER);
    }
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
        Directory, DirectoryId, EntryId, EntryKind, FileKind, Link, Node, Tree, TreeError,
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
        assert_eq!(size_of::<Node>(), 24);
        assert_eq!(size_of::<Directory>(), 40);
        assert_eq!(size_of::<Link>(), 28);
    }
}
