use std::collections::hash_map::Entry as MapEntry;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::io;
use std::iter;
use std::mem;

use crate::{Extended, Special, Stat};

/// The largest block count that ncdu stores, in 60 bits. Sums of block counts saturate here.
pub const BLOCKS_MAX: u64 = (1 << 60) - 1;

/// The sum of two block counts, saturated at [`BLOCKS_MAX`].
pub(crate) fn add_blocks(blocks: u64, more: u64) -> u64 {
    blocks.saturating_add(more).min(BLOCKS_MAX)
}

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
    /// The disk usage in 512-byte blocks of the hard links below this directory that have links
    /// outside of it, which deleting the directory would not free.
    pub shared_blocks: u64,
    /// The apparent size in bytes of the same hard links as `shared_blocks`.
    pub shared_size: u64,
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
    pub entry: EntryId,
    /// The links to the same inode form a ring, through which any one of them reaches the others.
    next: LinkId,
    previous: LinkId,
    /// Whether the size of the inode is in the totals of the directories above its links. All
    /// links of the ring have the same value.
    counted: bool,
}

/// The inode of a [`Link`]: the device of its parent and its inode number.
type Inode = (DeviceId, u64);

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
/// export stores, through [`Tree::create_directory`] and [`Tree::create`].
#[derive(Debug, Default, Clone)]
pub struct Tree {
    entries: Vec<Entry>,
    directories: Vec<Directory>,
    links: Vec<Link>,
    /// One link of the ring of each inode.
    inodes: HashMap<Inode, LinkId>,
    /// The inodes whose sizes may be missing from the totals, which [`Tree::add_all_stats`] adds.
    /// `None` stands for every inode, which is cheaper than a set once many are uncounted.
    uncounted: Option<HashSet<Inode>>,
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

    /// The links to the same inode as `link`, starting with it.
    pub fn links(&self, link: LinkId) -> impl Iterator<Item = LinkId> + '_ {
        iter::successors(Some(link), move |&current| {
            Some(self.links[current.0].next).filter(|&next| next != link)
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
            let data = &self.directories[id.0];
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
        let link = &self.links[link.0];
        let mut path = Vec::new();
        self.format_path(link.parent, with_root, &mut path);
        path.push(b'/');
        path.extend_from_slice(self.name(link.entry));
        path
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
    pub fn device_id(&mut self, device: u64) -> Result<DeviceId, TreeError> {
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
    pub fn create_directory(
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
            shared_blocks: 0,
            shared_size: 0,
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
    /// the link count of `stat`, and joins the links to the same inode, whose sizes are then
    /// left out of the totals until [`Tree::add_all_stats`].
    pub fn create(
        &mut self,
        parent: DirectoryId,
        kind: FileKind,
        stat: &Stat,
        extended: Option<EntryExtended>,
        name: &[u8],
    ) -> EntryId {
        let kind = match kind {
            FileKind::HardLink => EntryKind::HardLink(LinkId(self.links.len())),
            FileKind::Regular => EntryKind::Regular,
            FileKind::NonRegular => EntryKind::NonRegular,
            FileKind::Special(special) => EntryKind::Special(special),
        };
        let entry = self.push(kind, stat, extended, name);
        self.prepend(parent, entry);
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
        let directory = &mut self.directories[id.0];
        directory.items = items;
        directory.shared_blocks = shared_blocks;
        directory.shared_size = shared_size;
        directory.read_error = read_error;
        directory.sub_error = sub_error;
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
    /// [`Tree::update_sub_error`] on each of them afterwards. The hard links below `id` leave the
    /// accounting, and the other links to their inodes are left out of the totals: call
    /// [`Tree::add_all_stats`] afterwards.
    pub fn zero_stats(&mut self, id: EntryId, parent: Option<DirectoryId>) {
        self.remove_links(id);
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

    /// Take every hard link at or below `id` out of the accounting, in the order of the tree.
    fn remove_links(&mut self, id: EntryId) {
        let mut stack = vec![id];
        while let Some(id) = stack.pop() {
            match self.entries[id.0].kind {
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
        let link = &self.links[link.0];
        (self.directories[link.parent.0].device, link.inode)
    }

    /// Add `link` to the ring of its inode, and leave the inode out of the totals until
    /// [`Tree::add_all_stats`].
    fn add_link(&mut self, link: LinkId) {
        let inode = self.inode(link);
        match self.inodes.entry(inode) {
            MapEntry::Vacant(vacant) => {
                vacant.insert(link);
            }
            MapEntry::Occupied(occupied) => {
                let first = *occupied.get();
                self.set_stats(first, false);
                let previous = self.links[first.0].previous;
                self.links[link.0].next = first;
                self.links[link.0].previous = previous;
                self.links[first.0].previous = link;
                self.links[previous.0].next = link;
            }
        }
        self.add_uncounted(inode);
    }

    /// Take `link` out of the ring of its inode and the size of the inode out of the totals. The
    /// other links of the ring are counted again by [`Tree::add_all_stats`].
    ///
    /// The link count of the other links stays as it is, as a refresh removes the links of the
    /// directory that it scans again, which are usually still on the filesystem.
    fn remove_link(&mut self, link: LinkId) {
        self.set_stats(link, false);
        let inode = self.inode(link);
        let Some(&first) = self.inodes.get(&inode) else {
            return;
        };
        let Link { next, previous, .. } = self.links[link.0];
        if next == link {
            self.inodes.remove(&inode);
            if let Some(uncounted) = &mut self.uncounted {
                uncounted.remove(&inode);
            }
        } else {
            if first == link {
                self.inodes.insert(inode, next);
            }
            self.add_uncounted(inode);
            self.links[next.0].previous = previous;
            self.links[previous.0].next = next;
        }
    }

    /// Mark `inode` for [`Tree::add_all_stats`].
    fn add_uncounted(&mut self, inode: Inode) {
        if let Some(uncounted) = &mut self.uncounted {
            if uncounted.len() > self.inodes.len() / 8 {
                self.uncounted = None;
            } else {
                uncounted.insert(inode);
            }
        }
    }

    /// Add the size of the inode of `link` to the totals of every directory above its links, or
    /// take it out, once per directory. A directory shares the size when the inode has more links
    /// than the directory holds. The size is that of `link`, and the link count is that of the
    /// links when they agree and the tree holds no more links than that, or else the number of
    /// links in the tree.
    ///
    /// The ring must be the same when the size is taken out as when it was added.
    fn set_stats(&mut self, link: LinkId, add: bool) {
        if self.links[link.0].counted == add {
            return;
        }
        let link_count = self.links[link.0].link_count;
        let mut found: u32 = 0;
        let mut inconsistent = false;
        let mut parents: HashMap<DirectoryId, u32> = HashMap::new();
        let mut current = link;
        loop {
            let data = &mut self.links[current.0];
            data.counted = add;
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
        // Visit every directory above the links once, as an import controls both the number of
        // links and the depth. A walk up stops at a directory that an earlier walk reached, so the
        // reached directories always include their ancestors. Reversing each walk then lists every
        // directory after the directories above it.
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
                parent = self.directories[directory.0].parent;
            }
            order[walk_start..].reverse();
        }
        // A directory counts its own links and those below it, so add each count to the parent
        // after the directories below it have added theirs.
        for &directory in order.iter().rev() {
            if let Some(parent) = self.directories[directory.0].parent
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
        let Entry { blocks, size, .. } = self.entries[self.links[link.0].entry.0];
        for (directory, count) in directories {
            let data = &mut self.directories[directory.0];
            let shared = count < link_count;
            if add {
                if shared {
                    data.shared_blocks = data.shared_blocks.saturating_add(blocks);
                    data.shared_size = data.shared_size.saturating_add(size);
                }
                let entry = &mut self.entries[data.entry.0];
                entry.blocks = add_blocks(entry.blocks, blocks);
                entry.size = entry.size.saturating_add(size);
            } else {
                if shared {
                    data.shared_blocks = data.shared_blocks.saturating_sub(blocks);
                    data.shared_size = data.shared_size.saturating_sub(size);
                }
                let entry = &mut self.entries[data.entry.0];
                entry.blocks = entry.blocks.saturating_sub(blocks);
                entry.size = entry.size.saturating_sub(size);
            }
        }
    }

    /// Add the sizes of the inodes that are left out of the totals, as after adding or removing
    /// hard links.
    pub fn add_all_stats(&mut self) {
        let inodes = mem::take(&mut self.inodes);
        match self.uncounted.take() {
            None => {
                for &link in inodes.values() {
                    self.set_stats(link, true);
                }
            }
            Some(uncounted) => {
                for inode in &uncounted {
                    if let Some(&link) = inodes.get(inode) {
                        self.set_stats(link, true);
                    }
                }
            }
        }
        self.inodes = inodes;
        self.uncounted = Some(HashSet::new());
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
pub enum FileKind {
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

    /// Paths join the names from below the root, or from the root when asked, with a `/` that is
    /// left out after a name that ends with one, and the path of a link adds a `/` and its name in
    /// any case. Upstream has no test for `Dir.fmtPath` and `Link.path`.
    #[test]
    fn paths() {
        let mut tree = Tree::default();
        let device = tree.device_id(0).expect("device fits");
        let stat = Stat::default();
        let root = tree.create_directory(None, device, &stat, None, b"/");
        let first = tree.create_directory(Some(root), device, &stat, None, b"a");
        let second = tree.create_directory(Some(first), device, &stat, None, b"b");
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
            let entry = tree.create(parent, FileKind::HardLink, &stat, None, name);
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
}
