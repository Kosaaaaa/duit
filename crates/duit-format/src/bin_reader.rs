use std::collections::HashSet;
use std::io::{self, Read, Seek, SeekFrom};

use duit_model::{
    DirectoryId, EntryExtended, EntryType, Extended, FileKind, Sink, Special, Stat, Tree, TreeError,
};
use zstd::zstd_safe;

use crate::bin_format::{
    BLOCK_LENGTH_BITS, BlockPosition, FOOTER_LENGTH, HEADER_LENGTH, INDEFINITE_OR_BREAK,
    INDEX_BLOCK, INDEX_ENTRY_LENGTH, ItemKey, ItemRef, MAX_BLOCK_LENGTH, Major, NUMBER_LENGTH,
    ROOT_REFERENCE_LENGTH, SIMPLE_TRUE,
};
use crate::item::{ItemType, add_entry};

const CACHED_BLOCKS: usize = 8;

/// The most entries that ncdu accepts in an array or a map, `skip` in `bin_reader.zig`. It also
/// keeps the item count of a map, twice its length, from overflowing before the buffer check, and
/// below the sentinel [`Remaining::UNTIL_BREAK`].
const MAX_CONTAINER_LENGTH: u64 = 1 << 24;
const _: () = assert!(2 * MAX_CONTAINER_LENGTH < u32::MAX as u64);

/// Why [`BinaryReader::open`] failed.
#[derive(Debug)]
pub enum BinaryOpenError {
    /// The input cannot seek, as a pipe cannot.
    Seek(io::Error),
    /// A read failed, or the input ended early with [`io::ErrorKind::UnexpectedEof`].
    Read(io::Error),
    /// The index block is not valid.
    Corrupt,
}

/// Why [`BinaryReader::import`] failed.
#[derive(Debug)]
pub enum BinaryImportError {
    /// The item at `itemref` is not valid, or the data block that holds it is not, or the item is
    /// reached a second time.
    Corrupt { itemref: u64 },
    /// A data block decompressed to a size other than its zstd frame gives. `result` is what zstd
    /// returned: the size, or an error code.
    Decompress {
        block: u32,
        expected: u64,
        result: usize,
    },
    /// A read of a data block failed.
    Read(io::Error),
    /// The sink failed.
    Sink(io::Error),
}

/// A reader of the ncdu binary export format, which [`crate::BinaryExport`] writes.
///
/// The format is read with random access: the index block at the end of the input gives the
/// position of each data block and the reference of the root, and each item refers to the entry
/// before it in its directory and, for a directory, to its last entry. An item reference is the
/// number of its block shifted left by 24 bits, plus its offset in the decompressed block.
pub struct BinaryReader<R> {
    input: R,
    /// The index block without its header and footer: the position of each data block as
    /// [`INDEX_ENTRY_LENGTH`] big-endian bytes, then the root reference.
    index: Vec<u8>,
    root: u64,
    blocks: Vec<Block>,
    lookups: u64,
    /// The compressed data block read last, kept for the next.
    compressed: Vec<u8>,
}

struct Block {
    number: u32,
    last_use: u64,
    data: Vec<u8>,
}

struct Item {
    kind: ItemType,
    stat: Stat,
    name: Vec<u8>,
    read_error: bool,
    previous: Option<u64>,
    last_entry: Option<u64>,
}

impl Item {
    fn new() -> Self {
        Self {
            kind: ItemType::Stat(EntryType::Directory),
            stat: Stat::default(),
            name: Vec::new(),
            read_error: false,
            previous: None,
            last_entry: None,
        }
    }
}

/// The items that [`BinaryReader::import`] has read, to stop at an item that it reaches a second
/// time.
///
/// An export with one thread writes the entries of a directory before the directory, and numbers
/// its blocks in the order that it writes them, so the import reads it from the end to the start:
/// each item has a lower reference than the item read before it, and none can be a second visit.
/// While that holds, only the last reference is kept. At the first item that breaks it, the import
/// reads the items before it again to collect the set of all of them, which grows with each item
/// after that. An export with several threads, as from `ncdu -t`, fills the blocks of its threads
/// side by side, so it takes the set.
enum Visited {
    /// Every item had a lower reference than the one before it. `last` is the reference of the
    /// item read last, and `count` the number of items read after the root.
    Descending { last: u64, count: u64 },
    /// The references of all the items read.
    All(HashSet<u64>),
}

struct ListedItem {
    kind: ItemType,
    name: Vec<u8>,
    stat: Stat,
    items: u32,
    shared_blocks: u64,
    shared_size: u64,
    read_error: bool,
    sub_error: bool,
    previous: Option<u64>,
    last_entry: Option<u64>,
}

/// A directory of a binary export and its entries, as [`BinaryReader::read_directory`] reads them
/// for the browser.
#[derive(Debug)]
pub struct Listing {
    /// The directory and its entries, with the totals, item counts, and shared sizes that the
    /// export stores for each directory rather than totals of the tree. The subdirectories have no
    /// entries.
    pub tree: Tree,
    /// The directory, the one in the tree without a parent.
    pub directory: DirectoryId,
    /// The subdirectories, `dir_refs` in `browser.zig`, sorted by [`DirectoryId`].
    pub directories: Vec<ListedDirectory>,
}

/// A subdirectory of a [`Listing`].
#[derive(Debug, Clone, Copy)]
pub struct ListedDirectory {
    pub id: DirectoryId,
    /// The reference of its item, which [`BinaryReader::read_directory`] reads it from.
    pub itemref: u64,
    /// Whether it has no entries, which the tree of the listing does not hold.
    pub empty: bool,
}

struct OpenDirectory<D> {
    directory: D,
    next: Option<u64>,
    device: u64,
}

impl<R: Read + Seek> BinaryReader<R> {
    /// Read the index block of `input`, whose signature is already read.
    pub fn open(mut input: R) -> Result<Self, BinaryOpenError> {
        let size = input
            .seek(SeekFrom::End(0))
            .map_err(BinaryOpenError::Seek)?;
        if size < 16 {
            return Err(BinaryOpenError::Read(io::ErrorKind::UnexpectedEof.into()));
        }
        let mut header = [0; 4];
        read_at(&mut input, &mut header, size - FOOTER_LENGTH as u64)
            .map_err(BinaryOpenError::Read)?;
        let header = u32::from_be_bytes(header);
        if header >> BLOCK_LENGTH_BITS != INDEX_BLOCK || header & 7 != 0 {
            return Err(BinaryOpenError::Corrupt);
        }
        let length = usize::try_from(header & MAX_BLOCK_LENGTH)
            .ok()
            .and_then(|length| length.checked_sub(HEADER_LENGTH + FOOTER_LENGTH))
            .filter(|&length| length >= ROOT_REFERENCE_LENGTH && (length as u64) < size)
            .ok_or(BinaryOpenError::Corrupt)?;
        let start = size
            .checked_sub((length + FOOTER_LENGTH) as u64)
            .ok_or_else(negative_offset_error)?;
        let mut index = vec![0; length];
        read_at(&mut input, &mut index, start).map_err(BinaryOpenError::Read)?;
        let root = index
            .last_chunk::<ROOT_REFERENCE_LENGTH>()
            .map(|root| u64::from_be_bytes(*root))
            .ok_or(BinaryOpenError::Corrupt)?;
        Ok(Self {
            input,
            index,
            root,
            blocks: Vec::with_capacity(CACHED_BLOCKS),
            lookups: 0,
            compressed: Vec::new(),
        })
    }

    /// Read the tree into `sink`, depth-first from the root. The entries of each directory arrive
    /// in the reverse order of the export, from the last entry to the first, as in ncdu.
    ///
    /// Each entry has the device of its directory unless it has its own. A stack of the open
    /// directories replaces recursion, so that deep nesting cannot overflow the call stack, and an
    /// item that is reached a second time is an error, so that references in a cycle end. See
    /// [`Visited`] for how the items read so far are kept.
    pub fn import<S: Sink>(&mut self, sink: &mut S) -> Result<(), BinaryImportError> {
        let root = self.root();
        let mut item = Item::new();
        let mut visited = Visited::Descending {
            last: root,
            count: 0,
        };
        self.read_item(root, 0, &mut item)?;
        let ItemType::Stat(EntryType::Directory) = item.kind else {
            return Err(BinaryImportError::Corrupt { itemref: root });
        };
        item.stat.entry_type = EntryType::Directory;
        let mut directory = sink
            .create_root(&item.name, &item.stat)
            .map_err(BinaryImportError::Sink)?;
        if item.read_error {
            sink.set_read_error(&mut directory)
                .map_err(BinaryImportError::Sink)?;
        }
        let mut directories = vec![OpenDirectory {
            directory,
            next: item.last_entry,
            device: item.stat.device,
        }];
        while let Some(open) = directories.last_mut() {
            let Some(itemref) = open.next else {
                if let Some(open) = directories.pop() {
                    let parent = directories.last_mut().map(|parent| &mut parent.directory);
                    sink.finish_directory(open.directory, parent)
                        .map_err(BinaryImportError::Sink)?;
                }
                continue;
            };
            self.visit(&mut visited, itemref)?;
            self.read_item(itemref, open.device, &mut item)?;
            open.next = item.previous;
            let child = add_entry(
                sink,
                &mut open.directory,
                item.kind,
                &item.name,
                &item.stat,
                item.read_error,
            )
            .map_err(BinaryImportError::Sink)?;
            if let Some(child) = child {
                directories.push(OpenDirectory {
                    directory: child,
                    next: item.last_entry,
                    device: item.stat.device,
                });
            }
        }
        Ok(())
    }

    /// Record that [`BinaryReader::import`] reads `itemref` next, or return an error if it read
    /// the item before.
    fn visit(&mut self, visited: &mut Visited, itemref: u64) -> Result<(), BinaryImportError> {
        match visited {
            Visited::Descending { last, count } if itemref < *last => {
                *last = itemref;
                *count += 1;
            }
            Visited::Descending { count, .. } => {
                let mut all = self.replay(*count)?;
                if !all.insert(itemref) {
                    return Err(BinaryImportError::Corrupt { itemref });
                }
                *visited = Visited::All(all);
            }
            Visited::All(all) => {
                if !all.insert(itemref) {
                    return Err(BinaryImportError::Corrupt { itemref });
                }
            }
        }
        Ok(())
    }

    /// The references of the root and of the first `count` items after it that
    /// [`BinaryReader::import`] read, in the same order, which it already read once without an
    /// error.
    fn replay(&mut self, count: u64) -> Result<HashSet<u64>, BinaryImportError> {
        let root = self.root();
        let mut item = Item::new();
        self.read_item(root, 0, &mut item)?;
        let mut visited = HashSet::from([root]);
        let mut next = vec![item.last_entry];
        let mut left = count;
        while left > 0
            && let Some(open) = next.last_mut()
        {
            let Some(itemref) = *open else {
                next.pop();
                continue;
            };
            visited.insert(itemref);
            left -= 1;
            self.read_item(itemref, 0, &mut item)?;
            *open = item.previous;
            if item.kind == ItemType::Stat(EntryType::Directory) {
                next.push(item.last_entry);
            }
        }
        Ok(visited)
    }

    /// Read the directory at `itemref` and its entries into a [`Listing`], as the browser opens a
    /// directory: `get` in `bin_reader.zig` on the directory, then on each of its entries as
    /// `loadDir` in `browser.zig` follows them, from the last one. `None` if the item is not a
    /// directory. With `extended`, an entry keeps its extended information, as with `-e`. After
    /// each entry, `progress` gets the name of the directory and the number of entries read so far.
    ///
    /// An entry that is reached a second time in the same directory is an error, so that
    /// references in a cycle end.
    pub fn read_directory(
        &mut self,
        itemref: u64,
        extended: bool,
        progress: &mut dyn FnMut(&[u8], u64),
    ) -> Result<Option<Listing>, BinaryImportError> {
        let directory = self.read_listed(itemref)?;
        if directory.kind != ItemType::Stat(EntryType::Directory) {
            return Ok(None);
        }
        let mut entries = Vec::new();
        let mut visited = HashSet::new();
        let mut next = directory.last_entry;
        while let Some(entry) = next {
            if !visited.insert(entry) {
                return Err(BinaryImportError::Corrupt { itemref: entry });
            }
            let item = self.read_listed(entry)?;
            next = item.previous;
            entries.push((entry, item));
            progress(
                &directory.name,
                u64::try_from(entries.len()).unwrap_or(u64::MAX),
            );
        }

        let mut tree = Tree::default();
        let id = add_listed_directory(&mut tree, None, &directory, extended)
            .map_err(|_| BinaryImportError::Corrupt { itemref })?;
        let mut directories = Vec::new();
        for (entry, item) in entries.iter().rev() {
            let kind = match item.kind {
                ItemType::Stat(EntryType::Directory) => {
                    let subdirectory = add_listed_directory(&mut tree, Some(id), item, extended)
                        .map_err(|_| BinaryImportError::Corrupt { itemref: *entry })?;
                    directories.push(ListedDirectory {
                        id: subdirectory,
                        itemref: *entry,
                        empty: item.last_entry.is_none(),
                    });
                    continue;
                }
                ItemType::Stat(EntryType::Regular) => FileKind::Regular,
                ItemType::Stat(EntryType::NonRegular) => FileKind::NonRegular,
                ItemType::Stat(EntryType::HardLink) => FileKind::HardLink,
                ItemType::Special(special) => FileKind::Special(special),
            };
            let entry_extended = EntryExtended::of(&item.stat, extended);
            tree.prepend_file(id, kind, &item.stat, entry_extended, &item.name)
                .map_err(|_| BinaryImportError::Corrupt { itemref: *entry })?;
        }
        Ok(Some(Listing {
            tree,
            directory: id,
            directories,
        }))
    }

    /// The reference of the root, which ends the index, `getRoot` in `bin_reader.zig`.
    pub fn root(&self) -> u64 {
        self.root
    }

    fn block_position(&self, number: u32) -> Option<BlockPosition> {
        let start = usize::try_from(number)
            .ok()?
            .checked_mul(INDEX_ENTRY_LENGTH)?;
        // As in ncdu, the root reference at the end of the index reads as the position of the
        // block after the last.
        let position = self
            .index
            .get(start..)?
            .first_chunk::<INDEX_ENTRY_LENGTH>()?;
        Some(BlockPosition(u64::from_be_bytes(*position)))
    }

    fn read_item(
        &mut self,
        itemref: u64,
        device: u64,
        item: &mut Item,
    ) -> Result<(), BinaryImportError> {
        let data = self.item_data(itemref)?;
        parse_item(data, itemref, device, item).ok_or(BinaryImportError::Corrupt { itemref })
    }

    fn read_listed(&mut self, itemref: u64) -> Result<ListedItem, BinaryImportError> {
        let data = self.item_data(itemref)?;
        parse_listed(data, itemref).ok_or(BinaryImportError::Corrupt { itemref })
    }

    fn item_data(&mut self, itemref: u64) -> Result<&[u8], BinaryImportError> {
        let corrupt = || BinaryImportError::Corrupt { itemref };
        let reference = ItemRef(itemref);
        let number = u32::try_from(reference.block()).map_err(|_| corrupt())?;
        let block = self.block(number, itemref)?;
        let offset = usize::try_from(reference.offset()).map_err(|_| corrupt())?;
        block
            .get(offset..)
            .filter(|data| !data.is_empty())
            .ok_or_else(corrupt)
    }

    fn block(&mut self, number: u32, itemref: u64) -> Result<&[u8], BinaryImportError> {
        self.lookups += 1;
        let cached = self.blocks.iter().position(|block| block.number == number);
        let position = if let Some(position) = cached {
            position
        } else {
            let block = Block {
                number,
                last_use: 0,
                data: self.load(number, itemref)?,
            };
            if self.blocks.len() < CACHED_BLOCKS {
                self.blocks.push(block);
                self.blocks.len() - 1
            } else {
                let (position, oldest) = self
                    .blocks
                    .iter_mut()
                    .enumerate()
                    .min_by_key(|(_, block)| block.last_use)
                    .ok_or(BinaryImportError::Corrupt { itemref })?;
                *oldest = block;
                position
            }
        };
        let block = self
            .blocks
            .get_mut(position)
            .ok_or(BinaryImportError::Corrupt { itemref })?;
        block.last_use = self.lookups;
        Ok(&block.data)
    }

    fn load(&mut self, number: u32, itemref: u64) -> Result<Vec<u8>, BinaryImportError> {
        let corrupt = || BinaryImportError::Corrupt { itemref };
        let position = self.block_position(number).ok_or_else(corrupt)?;
        let offset = position.offset();
        let length = usize::try_from(position.length()).map_err(|_| corrupt())?;
        if length <= HEADER_LENGTH + NUMBER_LENGTH + FOOTER_LENGTH {
            return Err(corrupt());
        }
        let block = &mut self.compressed;
        block.clear();
        block.resize(length - HEADER_LENGTH - FOOTER_LENGTH, 0);
        read_at(&mut self.input, block, offset + HEADER_LENGTH as u64).map_err(|error| {
            if error.kind() == io::ErrorKind::UnexpectedEof {
                corrupt()
            } else {
                BinaryImportError::Read(error)
            }
        })?;
        let (stored, frame) = block.split_first_chunk::<4>().ok_or_else(corrupt)?;
        if *stored != number.to_be_bytes() {
            return Err(corrupt());
        }
        let expected = match zstd_safe::get_frame_content_size(frame) {
            Ok(Some(size)) if size > 0 && size < 1 << 24 => size,
            Ok(Some(_) | None) | Err(_) => return Err(corrupt()),
        };
        let mut data = vec![0; usize::try_from(expected).map_err(|_| corrupt())?];
        let result = zstd_safe::decompress(data.as_mut_slice(), frame).unwrap_or_else(|code| code);
        if result != data.len() {
            return Err(BinaryImportError::Decompress {
                block: number,
                expected,
                result,
            });
        }
        Ok(data)
    }
}

fn negative_offset_error() -> BinaryOpenError {
    BinaryOpenError::Read(io::ErrorKind::InvalidInput.into())
}

fn read_at(input: &mut (impl Read + Seek), buffer: &mut [u8], offset: u64) -> io::Result<()> {
    input.seek(SeekFrom::Start(offset))?;
    input.read_exact(buffer)
}

fn fields<'a>(
    data: &'a [u8],
    mut field: impl FnMut(&mut Cbor<'a>, ItemKey, Value) -> Option<()>,
) -> Option<()> {
    let mut cbor = Cbor { buffer: data };
    let head = cbor.next()?;
    if head.major != Major::Map {
        return None;
    }
    let mut keys_left = head.definite();
    loop {
        let key = match &mut keys_left {
            Some(0) => return Some(()),
            Some(left) => {
                *left -= 1;
                cbor.next()?
            }
            None => {
                let key = cbor.next()?;
                if key.is_break() {
                    return Some(());
                }
                key
            }
        };
        if key.major != Major::Positive {
            cbor.skip(key)?;
            let value = cbor.next()?;
            cbor.skip(value)?;
            continue;
        }
        let value = cbor.next()?;
        match key.definite().and_then(ItemKey::from_u64) {
            Some(key) => field(&mut cbor, key, value)?,
            None => cbor.skip(value)?,
        }
    }
}

fn parse_item(data: &[u8], itemref: u64, device: u64, item: &mut Item) -> Option<()> {
    item.stat = Stat {
        device,
        ..Stat::default()
    };
    item.name.clear();
    item.read_error = false;
    item.previous = None;
    item.last_entry = None;
    let mut has_type = false;
    fields(data, |cbor, key, value| {
        let stat = &mut item.stat;
        let extended = &mut stat.extended;
        match key {
            ItemKey::Type => {
                item.kind = item_type(i32::try_from(value.integer()?).ok()?);
                has_type = true;
            }
            ItemKey::Name => {
                let name = cbor.bytes(value)?;
                item.name.clear();
                item.name.extend_from_slice(name);
            }
            ItemKey::Prev => item.previous = Some(value.itemref(itemref)?),
            ItemKey::Asize => stat.size = value.unsigned()?,
            ItemKey::Dsize => stat.blocks = value.unsigned::<u64>()? / 512,
            ItemKey::Dev => stat.device = value.unsigned()?,
            ItemKey::Rderr => item.read_error = value.is_true(),
            ItemKey::Sub => item.last_entry = Some(value.itemref(itemref)?),
            ItemKey::Ino => stat.inode = value.unsigned()?,
            ItemKey::Nlink => stat.link_count = link_count(value)?,
            ItemKey::Uid => extended.uid = Some(value.unsigned()?),
            ItemKey::Gid => extended.gid = Some(value.unsigned()?),
            ItemKey::Mode => extended.mode = Some(value.unsigned()?),
            ItemKey::Mtime => extended.mtime = Some(value.unsigned()?),
            ItemKey::Cumasize
            | ItemKey::Cumdsize
            | ItemKey::Shrasize
            | ItemKey::Shrdsize
            | ItemKey::Items => cbor.skip(value)?,
        }
        Some(())
    })?;
    (has_type && !item.name.is_empty()).then_some(())
}

/// The link count of the value of an `nlink` field, which ncdu reads as a 31-bit integer.
fn link_count(value: Value) -> Option<u32> {
    value.unsigned::<u32>().filter(|&count| count < 1 << 31)
}

fn parse_listed(data: &[u8], itemref: u64) -> Option<ListedItem> {
    let mut kind = None;
    let mut name: &[u8] = b"";
    let mut extended = Extended::default();
    fields(data, |cbor, key, value| {
        match key {
            ItemKey::Type => kind = Some(item_type(i32::try_from(value.integer()?).ok()?)),
            ItemKey::Name => name = cbor.bytes(value)?,
            ItemKey::Uid => extended.uid = Some(value.unsigned()?),
            ItemKey::Gid => extended.gid = Some(value.unsigned()?),
            ItemKey::Mode => extended.mode = Some(value.unsigned()?),
            ItemKey::Mtime => extended.mtime = Some(value.unsigned()?),
            ItemKey::Prev
            | ItemKey::Asize
            | ItemKey::Dsize
            | ItemKey::Dev
            | ItemKey::Rderr
            | ItemKey::Cumasize
            | ItemKey::Cumdsize
            | ItemKey::Shrasize
            | ItemKey::Shrdsize
            | ItemKey::Items
            | ItemKey::Sub
            | ItemKey::Ino
            | ItemKey::Nlink => cbor.skip(value)?,
        }
        Some(())
    })?;
    let kind = kind.filter(|_| !name.is_empty())?;
    let directory = kind == ItemType::Stat(EntryType::Directory);
    let link = kind == ItemType::Stat(EntryType::HardLink);
    let mut item = ListedItem {
        kind,
        name: name.to_vec(),
        stat: Stat {
            extended,
            ..Stat::default()
        },
        items: 0,
        shared_blocks: 0,
        shared_size: 0,
        read_error: false,
        sub_error: false,
        previous: None,
        last_entry: None,
    };
    fields(data, |cbor, key, value| {
        let stat = &mut item.stat;
        match key {
            ItemKey::Prev => item.previous = Some(value.itemref(itemref)?),
            ItemKey::Asize if !directory => stat.size = value.unsigned()?,
            ItemKey::Dsize if !directory => stat.blocks = value.unsigned::<u64>()? / 512,
            ItemKey::Rderr if directory => {
                if value.is_true() {
                    item.read_error = true;
                } else {
                    item.sub_error = true;
                }
            }
            ItemKey::Dev if directory => stat.device = value.unsigned()?,
            ItemKey::Cumasize => stat.size = value.unsigned()?,
            ItemKey::Cumdsize => stat.blocks = value.unsigned::<u64>()? / 512,
            ItemKey::Shrasize if directory => item.shared_size = value.unsigned()?,
            ItemKey::Shrdsize if directory => {
                item.shared_blocks = value.unsigned::<u64>()? / 512;
            }
            ItemKey::Items if directory => {
                item.items = u32::try_from(value.unsigned::<u64>()?).unwrap_or(u32::MAX);
            }
            ItemKey::Sub if directory => item.last_entry = Some(value.itemref(itemref)?),
            ItemKey::Ino if link => stat.inode = value.unsigned()?,
            ItemKey::Nlink if link => stat.link_count = link_count(value)?,
            ItemKey::Type
            | ItemKey::Name
            | ItemKey::Asize
            | ItemKey::Dsize
            | ItemKey::Dev
            | ItemKey::Rderr
            | ItemKey::Shrasize
            | ItemKey::Shrdsize
            | ItemKey::Items
            | ItemKey::Sub
            | ItemKey::Ino
            | ItemKey::Nlink
            | ItemKey::Uid
            | ItemKey::Gid
            | ItemKey::Mode
            | ItemKey::Mtime => cbor.skip(value)?,
        }
        Some(())
    })?;
    Some(item)
}

fn add_listed_directory(
    tree: &mut Tree,
    parent: Option<DirectoryId>,
    item: &ListedItem,
    extended: bool,
) -> Result<DirectoryId, TreeError> {
    let device = tree.device_id(item.stat.device)?;
    let entry_extended = EntryExtended::of(&item.stat, extended);
    let id = tree.prepend_directory(parent, device, &item.stat, entry_extended, &item.name)?;
    tree.set_totals(
        id,
        item.items,
        item.shared_blocks,
        item.shared_size,
        item.read_error,
        item.sub_error,
    );
    Ok(id)
}

fn item_type(value: i32) -> ItemType {
    match value {
        0 => ItemType::Stat(EntryType::Directory),
        1 => ItemType::Stat(EntryType::Regular),
        3 => ItemType::Stat(EntryType::HardLink),
        -1 => ItemType::Special(Special::ReadError),
        -3 => ItemType::Special(Special::OtherFilesystem),
        -4 => ItemType::Special(Special::KernelFilesystem),
        i32::MIN..=-5 | -2 => ItemType::Special(Special::Pattern),
        2..=i32::MAX => ItemType::Stat(EntryType::NonRegular),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Argument {
    Definite(u64),
    Indefinite,
    Break,
}

#[derive(Debug, Clone, Copy)]
struct Value {
    major: Major,
    argument: Argument,
}

impl Value {
    fn definite(self) -> Option<u64> {
        match self.argument {
            Argument::Definite(argument) => Some(argument),
            Argument::Indefinite | Argument::Break => None,
        }
    }

    fn is_break(self) -> bool {
        self.argument == Argument::Break
    }

    fn is_true(self) -> bool {
        self.major == Major::Simple && self.argument == Argument::Definite(SIMPLE_TRUE)
    }

    fn integer(self) -> Option<i128> {
        match self.major {
            Major::Positive => Some(i128::from(self.definite()?)),
            Major::Negative => Some(-i128::from(self.definite()?) - 1),
            Major::Bytes | Major::Text | Major::Array | Major::Map | Major::Tag | Major::Simple => {
                None
            }
        }
    }

    fn unsigned<T: TryFrom<i128>>(self) -> Option<T> {
        T::try_from(self.integer()?).ok()
    }

    fn itemref(self, current: u64) -> Option<u64> {
        let argument = self.definite()?;
        match self.major {
            Major::Positive => Some(argument),
            Major::Negative if argument < ItemRef(current).offset() => Some(current - argument - 1),
            Major::Negative
            | Major::Bytes
            | Major::Text
            | Major::Array
            | Major::Map
            | Major::Tag
            | Major::Simple => None,
        }
    }
}

/// The number of items left in a container that [`Cbor::skip`] skips, or
/// [`Remaining::UNTIL_BREAK`] for a container of indefinite length. A sentinel keeps the stack at
/// 4 bytes per level, where an enum would take 8 because `u32` has no niche.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Remaining(u32);

impl Remaining {
    const UNTIL_BREAK: Self = Self(u32::MAX);
}

const _: () = assert!(size_of::<Remaining>() == 4);

struct Cbor<'a> {
    buffer: &'a [u8],
}

impl<'a> Cbor<'a> {
    fn take(&mut self, length: u64) -> Option<&'a [u8]> {
        let length = usize::try_from(length).ok()?;
        let (taken, rest) = self.buffer.split_at_checked(length)?;
        self.buffer = rest;
        Some(taken)
    }

    fn argument<const N: usize>(&mut self) -> Option<u64> {
        let bytes = self.take(N as u64)?;
        Some(
            bytes
                .iter()
                .fold(0, |argument, &byte| (argument << 8) | u64::from(byte)),
        )
    }

    fn head(&mut self) -> Option<Value> {
        let byte = *self.take(1)?.first()?;
        let major = match byte >> 5 {
            0 => Major::Positive,
            1 => Major::Negative,
            2 => Major::Bytes,
            3 => Major::Text,
            4 => Major::Array,
            5 => Major::Map,
            6 => Major::Tag,
            7..=u8::MAX => Major::Simple,
        };
        let argument = match byte & 0x1f {
            argument @ 0..=0x17 => Argument::Definite(u64::from(argument)),
            0x18 => Argument::Definite(self.argument::<1>()?),
            0x19 => Argument::Definite(self.argument::<2>()?),
            0x1a => Argument::Definite(self.argument::<4>()?),
            0x1b => Argument::Definite(self.argument::<8>()?),
            INDEFINITE_OR_BREAK => match major {
                Major::Bytes | Major::Text | Major::Array | Major::Map => Argument::Indefinite,
                Major::Simple => Argument::Break,
                Major::Positive | Major::Negative | Major::Tag => return None,
            },
            0x1c..=0x1e | 0x20..=u8::MAX => return None,
        };
        Some(Value { major, argument })
    }

    fn next(&mut self) -> Option<Value> {
        loop {
            let value = self.head()?;
            if value.major != Major::Tag {
                return Some(value);
            }
        }
    }

    fn bytes(&mut self, value: Value) -> Option<&'a [u8]> {
        match value.major {
            Major::Bytes | Major::Text => self.take(value.definite()?),
            Major::Positive
            | Major::Negative
            | Major::Array
            | Major::Map
            | Major::Tag
            | Major::Simple => None,
        }
    }

    fn skip(&mut self, value: Value) -> Option<()> {
        let mut containers: Vec<Remaining> = Vec::new();
        let mut value = value;
        loop {
            match value.major {
                Major::Bytes | Major::Text | Major::Array | Major::Map
                    if value.argument == Argument::Indefinite =>
                {
                    containers.push(Remaining::UNTIL_BREAK);
                }
                Major::Bytes | Major::Text => {
                    self.take(value.definite()?)?;
                }
                Major::Array | Major::Map => {
                    let length = value.definite()?;
                    if length > MAX_CONTAINER_LENGTH {
                        return None;
                    }
                    let count = if value.major == Major::Map {
                        length * 2
                    } else {
                        length
                    };
                    let enclosing = match containers.last() {
                        Some(&Remaining::UNTIL_BREAK) | None => 0,
                        Some(&Remaining(left)) => {
                            containers.pop();
                            u64::from(left)
                        }
                    };
                    let left = enclosing + count;
                    if left > u64::try_from(self.buffer.len()).ok()? {
                        return None;
                    }
                    let left = u32::try_from(left)
                        .ok()
                        .filter(|&left| Remaining(left) != Remaining::UNTIL_BREAK)?;
                    containers.push(Remaining(left));
                }
                Major::Positive | Major::Negative | Major::Tag | Major::Simple => {}
            }
            value = loop {
                match containers.last_mut() {
                    None => return Some(()),
                    Some(&mut Remaining::UNTIL_BREAK) => {
                        let next = self.next()?;
                        if !next.is_break() {
                            break next;
                        }
                        containers.pop();
                    }
                    Some(Remaining(0)) => {
                        containers.pop();
                    }
                    Some(Remaining(left)) => {
                        *left -= 1;
                        break self.next()?;
                    }
                }
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Cbor, Value};

    #[test]
    fn cbor_int_parsing() {
        let cases: &[(&[u8], i128)] = &[
            (b"\x00", 0),
            (b"\x01", 1),
            (b"\x18\x18", 0x18),
            (b"\x18\xff", 0xff),
            (b"\x19\x07\xff", 0x7ff),
            (b"\x19\xff\xff", 0xffff),
            (b"\x1a\x00\x01\x00\x00", 0x10000),
            (
                b"\x1b\x7f\xff\xff\xff\xff\xff\xff\xff",
                i128::from(i64::MAX),
            ),
            (
                b"\x1b\xff\xff\xff\xff\xff\xff\xff\xff",
                i128::from(u64::MAX),
            ),
            (b"\x20", -1),
            (b"\x38\x18", -0x19),
            (b"\x39\x01\xf3", -500),
            (b"\x3a\xfe\xdc\xba\x97", -0xfedc_ba98),
            (
                b"\x3b\x7f\xff\xff\xff\xff\xff\xff\xff",
                i128::from(i64::MIN),
            ),
            (b"\x3b\xff\xff\xff\xff\xff\xff\xff\xff", -(1 << 64)),
        ];
        for &(input, expected) in cases {
            let mut cbor = Cbor { buffer: input };
            assert_eq!(
                cbor.next().and_then(Value::integer),
                Some(expected),
                "{input:?}"
            );
            assert_eq!(cbor.buffer, b"", "{input:?}");
        }
    }

    #[test]
    fn cbor_string_parsing() {
        let mut cbor = Cbor { buffer: b"\x40" };
        let value = cbor.next().expect("head is read");
        assert_eq!(cbor.bytes(value), Some(&b""[..]));
        cbor.buffer = b"\x45\x00\x01\x02\x03\x04x";
        let value = cbor.next().expect("head is read");
        assert_eq!(cbor.bytes(value), Some(&b"\x00\x01\x02\x03\x04"[..]));
        assert_eq!(cbor.buffer, b"x");
        cbor.buffer = b"\x78\x241234567890abcdefghijklmnopqrstuvwxyz-end";
        let value = cbor.next().expect("head is read");
        assert_eq!(
            cbor.bytes(value),
            Some(&b"1234567890abcdefghijklmnopqrstuvwxyz"[..])
        );
        assert_eq!(cbor.buffer, b"-end");
    }

    #[test]
    fn cbor_skip_parsing() {
        let cases: &[&[u8]] = &[
            b"\x00",
            b"\x40",
            b"\x41a",
            b"\x5f\xff",
            b"\x5f\x41a\xff",
            b"\x80",
            b"\x81\x00",
            b"\x9f\xff",
            b"\x9f\x9f\xff\xff",
            b"\x9f\x9f\x81\x00\xff\xff",
            b"\x9f\x82\x9f\x80\x00\xff\x00\xff",
            b"\xa0",
            b"\xa1\x00\x01",
            b"\xbf\xff",
            b"\xbf\xc0\x00\x9f\xff\xff",
        ];
        for &input in cases {
            let input = [input, b"garbage"].concat();
            let mut cbor = Cbor { buffer: &input };
            let value = cbor.next().expect("head is read");
            assert_eq!(cbor.skip(value), Some(()), "{input:?}");
            assert_eq!(cbor.buffer, b"garbage", "{input:?}");
        }
    }

    #[test]
    fn cbor_skip_count_beyond_buffer() {
        let cases: &[(&[u8], Option<()>)] = &[
            (b"\x83\x00\x00\x00", Some(())),
            (b"\x83\x00\x00", None),
            (b"\x82\x00\x82\x00\x00", Some(())),
            (b"\x82\x00\x82\x00", None),
            (b"\x9a\x00\x01\x00\x00\x00\x00", None),
            (b"\xb9\x00\x02\x00\x00\x00", None),
        ];
        for &(input, expected) in cases {
            let mut cbor = Cbor { buffer: input };
            let value = cbor.next().expect("head is read");
            assert_eq!(cbor.skip(value), expected, "{input:?}");
        }
    }
}
