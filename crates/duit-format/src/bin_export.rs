use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::error::Error;
use std::fmt;
use std::io::{self, Write};

use duit_model::{EntryType, Sink, Special, Stat};
use zstd::bulk::Compressor;

/// The first bytes of a binary export.
pub const BINARY_SIGNATURE: &[u8; 8] = b"\xbfncduEX1";

/// The keys of the fields of an item.
#[derive(Clone, Copy)]
enum ItemKey {
    Type = 0,
    Name = 1,
    Prev = 2,
    Asize = 3,
    Dsize = 4,
    Dev = 5,
    Rderr = 6,
    Cumasize = 7,
    Cumdsize = 8,
    Shrasize = 9,
    Shrdsize = 10,
    Items = 11,
    Sub = 12,
    Ino = 13,
    Nlink = 14,
    Uid = 15,
    Gid = 16,
    Mode = 17,
    Mtime = 18,
}

/// The number of item keys.
const ITEM_KEY_COUNT: usize = 19;

/// An upper bound on the encoded size of an item without its name: 2 bytes for the start and the
/// end of the map, and 11 bytes per field, 2 for the key and 9 for a full `u64`.
const MAX_ITEM_LEN: usize = 2 + 11 * ITEM_KEY_COUNT;

/// The CBOR major types that the format uses.
const CBOR_POSITIVE: u8 = 0;
const CBOR_NEGATIVE: u8 = 1;
const CBOR_BYTES: u8 = 2;
const CBOR_MAP: u8 = 5;
const CBOR_SIMPLE: u8 = 7;

/// The CBOR simple values `false` and `true`.
const CBOR_FALSE: u64 = 20;
const CBOR_TRUE: u64 = 21;

/// The block ids of a block header.
const DATA_BLOCK: u32 = 0;
const INDEX_BLOCK: u32 = 1;

/// The largest length of a block, which its header holds in 28 bits.
const MAX_BLOCK_LENGTH: u32 = (1 << 28) - 1;

/// Why a [`BinaryExport`] failed, other than an error of its writer. It reaches the caller as the
/// inner error of an [`io::Error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryExportError {
    /// An item with its name does not fit in a block.
    PathTooLong,
    /// The export reached 1 TiB, the largest offset that the index holds.
    FileTooLarge,
    /// The index block reached the largest length of a block.
    TooManyBlocks,
}

impl fmt::Display for BinaryExportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::PathTooLong => "path too long",
            Self::FileTooLarge => "export data file has grown too large",
            Self::TooManyBlocks => "too many data blocks",
        })
    }
}

impl Error for BinaryExportError {}

impl From<BinaryExportError> for io::Error {
    fn from(error: BinaryExportError) -> Self {
        Self::other(error)
    }
}

/// The uncompressed size of the data block `number`. Without `configured`, it starts at 64 KiB
/// and grows with the export, which keeps the index block small.
fn block_size(configured: Option<usize>, number: u32) -> usize {
    configured.unwrap_or(match number {
        0..0x10_0000 => 64 << 10,
        0x10_0000..0x20_0000 => 128 << 10,
        0x20_0000..0x40_0000 => 256 << 10,
        0x40_0000..0x80_0000 => 512 << 10,
        0x80_0000..0x100_0000 => 1024 << 10,
        0x100_0000.. => 2048 << 10,
    })
}

/// The header and the footer of a block of `length` bytes, which includes both. A block too long
/// for its header is reported as [`BinaryExportError::FileTooLarge`], as ncdu has no message of its
/// own for it.
fn block_header(id: u32, length: usize) -> io::Result<[u8; 4]> {
    let length = u32::try_from(length)
        .ok()
        .filter(|&length| length <= MAX_BLOCK_LENGTH)
        .ok_or(BinaryExportError::FileTooLarge)?;
    Ok(((id << 28) | length).to_be_bytes())
}

/// A [`Sink`] that writes the ncdu binary export format.
///
/// The export is the signature, then the data blocks, then an index block. A data block holds a
/// zstd frame of items, each a CBOR map. A directory is written after its entries, as each entry
/// refers to the entry before it in the same directory, and the directory to its last entry.
/// The index block gives the offset of each data block and the reference of the root.
pub struct BinaryExport<W: Write> {
    out: W,
    extended: bool,
    compressor: Compressor<'static>,
    configured_block_size: Option<usize>,
    /// The uncompressed data of the current block.
    buffer: Vec<u8>,
    /// The size of the current block, which `buffer` does not grow beyond.
    buffer_size: usize,
    /// The number of the current block, or `None` before the first item.
    block_number: Option<u32>,
    /// The reference of the item being written.
    itemref: u64,
    /// The offset and the length of each data block by number, as `offset << 24 | length`, or 0
    /// for a block that is not written yet.
    index: Vec<u64>,
    /// The number of bytes written to `out`.
    file_offset: u64,
    root_itemref: u64,
}

/// The links to one inode that a directory and its subdirectories hold, whose size is not counted
/// yet.
#[derive(Clone, Copy)]
struct Inode {
    size: u64,
    blocks: u64,
    link_count: u32,
    found: u32,
}

/// The handle of a directory in a [`BinaryExport`].
pub struct BinaryDirectory {
    name: Vec<u8>,
    stat: Stat,
    /// The reference of the last entry written.
    last_entry: Option<u64>,
    items: u64,
    size: u64,
    blocks: u64,
    read_error: bool,
    sub_error: bool,
    shared_size: u64,
    shared_blocks: u64,
    inodes: HashMap<u64, Inode>,
}

impl BinaryDirectory {
    fn new(name: &[u8], stat: &Stat) -> Self {
        Self {
            name: name.to_vec(),
            stat: *stat,
            last_entry: None,
            items: 0,
            size: 0,
            blocks: 0,
            read_error: false,
            sub_error: false,
            shared_size: 0,
            shared_blocks: 0,
            inodes: HashMap::new(),
        }
    }

    /// Add the sizes of the hard links to this directory. With a `parent`, move the links whose
    /// every link is found to the totals of the parent, and the others to its links.
    ///
    /// An export without link counts has 0 for each link. Such a link is counted once per
    /// directory and never as shared.
    fn count_links(&mut self, parent: Option<&mut Self>) {
        for inode in self.inodes.values() {
            self.size = self.size.saturating_add(inode.size);
            self.blocks = self.blocks.saturating_add(inode.blocks);
            if inode.link_count > 1 && inode.found < inode.link_count {
                self.shared_size = self.shared_size.saturating_add(inode.size);
                self.shared_blocks = self.shared_blocks.saturating_add(inode.blocks);
            }
        }
        let Some(parent) = parent else {
            return;
        };
        for (number, inode) in self.inodes.drain() {
            if inode.link_count > 0 && inode.found >= inode.link_count {
                parent.size = parent.size.saturating_add(inode.size);
                parent.blocks = parent.blocks.saturating_add(inode.blocks);
            } else {
                match parent.inodes.entry(number) {
                    Entry::Occupied(mut entry) => {
                        let found = &mut entry.get_mut().found;
                        *found = found.saturating_add(inode.found);
                    }
                    Entry::Vacant(entry) => {
                        entry.insert(inode);
                    }
                }
            }
        }
    }
}

impl<W: Write> BinaryExport<W> {
    /// Start an export to `out` and write the signature. With `extended`, the fields of
    /// [`duit_model::Extended`] are written too. Each block is compressed with zstd at
    /// `compress_level`. `block_size` is the uncompressed size of a block in bytes, and without
    /// it the size grows with the export.
    pub fn new(
        mut out: W,
        extended: bool,
        compress_level: u8,
        block_size: Option<usize>,
    ) -> io::Result<Self> {
        let compressor = Compressor::new(i32::from(compress_level))?;
        // The signature is written before a scan or an import starts, as ncdu does.
        out.write_all(BINARY_SIGNATURE)?;
        out.flush()?;
        Ok(Self {
            out,
            extended,
            compressor,
            configured_block_size: block_size,
            buffer: Vec::new(),
            buffer_size: self::block_size(block_size, 0),
            block_number: None,
            itemref: 0,
            index: Vec::new(),
            file_offset: 8,
            root_itemref: 0,
        })
    }

    /// Write the last data block and the index block after the root is finished, flush the
    /// export, and return the writer. Without a root, the index block refers to a root at 0.
    pub fn finish(mut self) -> io::Result<W> {
        self.flush_block(0)?;
        while self.index.last() == Some(&0) {
            self.index.pop();
        }
        let length = 4 + 8 * self.index.len() + 8 + 4;
        let header = block_header(INDEX_BLOCK, length)?;
        let mut block = Vec::with_capacity(length);
        block.extend_from_slice(&header);
        for entry in &self.index {
            block.extend_from_slice(&entry.to_be_bytes());
        }
        block.extend_from_slice(&self.root_itemref.to_be_bytes());
        block.extend_from_slice(&header);
        self.out.write_all(&block)?;
        self.out.flush()?;
        Ok(self.out)
    }

    /// Write the current block, if it holds an item, and start the next one. `expected_length`
    /// is the length of the item that does not fit.
    fn flush_block(&mut self, expected_length: usize) -> io::Result<()> {
        if expected_length > self.buffer_size {
            return Err(BinaryExportError::PathTooLong.into());
        }
        if let Some(number) = self.block_number
            && !self.buffer.is_empty()
        {
            let compressed = self.compressor.compress(&self.buffer)?;
            let length = 12 + compressed.len();
            let header = block_header(DATA_BLOCK, length)?;
            let mut block = Vec::with_capacity(length);
            block.extend_from_slice(&header);
            block.extend_from_slice(&number.to_be_bytes());
            block.extend_from_slice(&compressed);
            block.extend_from_slice(&header);
            if self.file_offset >= 1 << 40 {
                return Err(BinaryExportError::FileTooLarge.into());
            }
            if let Some(entry) = usize::try_from(number)
                .ok()
                .and_then(|number| self.index.get_mut(number))
            {
                *entry = (self.file_offset << 24) + length as u64;
            }
            self.file_offset += length as u64;
            self.out.write_all(&block)?;
        }

        self.buffer.clear();
        let number =
            u32::try_from(self.index.len()).map_err(|_| BinaryExportError::TooManyBlocks)?;
        self.block_number = Some(number);
        self.index.push(0);
        if 4 + 8 * self.index.len() + 12 >= 1 << 28 {
            return Err(BinaryExportError::TooManyBlocks.into());
        }
        self.buffer_size = block_size(self.configured_block_size, number);
        self.buffer.reserve(self.buffer_size);
        Ok(())
    }

    /// Write the head of a CBOR data item of `major` type with `argument`. An argument above 23
    /// follows the head in 1, 2, 4, or 8 bytes, which the additional information `0x18` to `0x1b`
    /// selects.
    fn cbor_head(&mut self, major: u8, argument: u64) {
        let major = major << 5;
        if let Ok(argument) = u8::try_from(argument) {
            if argument <= 23 {
                self.buffer.push(major | argument);
            } else {
                self.buffer.extend_from_slice(&[major | 0x18, argument]);
            }
        } else if let Ok(argument) = u16::try_from(argument) {
            self.buffer.push(major | 0x19);
            self.buffer.extend_from_slice(&argument.to_be_bytes());
        } else if let Ok(argument) = u32::try_from(argument) {
            self.buffer.push(major | 0x1a);
            self.buffer.extend_from_slice(&argument.to_be_bytes());
        } else {
            self.buffer.push(major | 0x1b);
            self.buffer.extend_from_slice(&argument.to_be_bytes());
        }
    }

    fn field(&mut self, key: ItemKey, value: u64) {
        self.cbor_head(CBOR_POSITIVE, key as u64);
        self.cbor_head(CBOR_POSITIVE, value);
    }

    /// Write a reference to the item at `itemref`, if any. A reference into the current block is
    /// written as a negative offset from the item being written, which compresses better.
    fn reference_field(&mut self, key: ItemKey, itemref: Option<u64>) {
        let Some(itemref) = itemref else {
            return;
        };
        self.cbor_head(CBOR_POSITIVE, key as u64);
        if self.block_number.map(u64::from) == Some(itemref >> 24) {
            self.cbor_head(CBOR_NEGATIVE, self.itemref - itemref - 1);
        } else {
            self.cbor_head(CBOR_POSITIVE, itemref);
        }
    }

    /// Start an item with its type, name, and the reference to the entry before it, and return
    /// its reference. A negative `item_type` is a special entry.
    fn item_start(&mut self, item_type: i8, previous: Option<u64>, name: &[u8]) -> io::Result<u64> {
        let minimum_length = name.len() + MAX_ITEM_LEN;
        if self.block_number.is_none() || self.buffer.len() + minimum_length > self.buffer_size {
            self.flush_block(minimum_length)?;
        }
        let block_number = self.block_number.map_or(0, u64::from);
        self.itemref = (block_number << 24) | self.buffer.len() as u64;
        // An indefinite map, which `item_end` ends.
        self.buffer.push((CBOR_MAP << 5) | 31);
        self.cbor_head(CBOR_POSITIVE, ItemKey::Type as u64);
        match u64::try_from(item_type) {
            Ok(item_type) => self.cbor_head(CBOR_POSITIVE, item_type),
            Err(_) => self.cbor_head(CBOR_NEGATIVE, u64::from((-1 - item_type).unsigned_abs())),
        }
        self.cbor_head(CBOR_POSITIVE, ItemKey::Name as u64);
        self.cbor_head(CBOR_BYTES, name.len() as u64);
        self.buffer.extend_from_slice(name);
        self.reference_field(ItemKey::Prev, previous);
        Ok(self.itemref)
    }

    fn item_extended(&mut self, stat: &Stat) {
        if !self.extended {
            return;
        }
        let extended = stat.extended;
        if let Some(uid) = extended.uid {
            self.field(ItemKey::Uid, uid.into());
        }
        if let Some(gid) = extended.gid {
            self.field(ItemKey::Gid, gid.into());
        }
        if let Some(mode) = extended.mode {
            self.field(ItemKey::Mode, mode.into());
        }
        if let Some(mtime) = extended.mtime {
            self.field(ItemKey::Mtime, mtime);
        }
    }

    /// End the map of an item with the CBOR break.
    fn item_end(&mut self) {
        self.buffer.push((CBOR_SIMPLE << 5) | 31);
    }
}

/// The type of an entry as the format writes it.
fn entry_type(entry_type: EntryType) -> i8 {
    match entry_type {
        EntryType::Directory => 0,
        EntryType::Regular => 1,
        EntryType::NonRegular => 2,
        EntryType::HardLink => 3,
    }
}

fn disk_usage(blocks: u64) -> u64 {
    blocks.saturating_mul(512)
}

impl<W: Write> Sink for BinaryExport<W> {
    type Directory = BinaryDirectory;

    fn create_root(&mut self, path: &[u8], stat: &Stat) -> io::Result<BinaryDirectory> {
        Ok(BinaryDirectory::new(path, stat))
    }

    fn add_special(
        &mut self,
        directory: &mut BinaryDirectory,
        name: &[u8],
        special: Special,
    ) -> io::Result<()> {
        directory.items += 1;
        let item_type = match special {
            Special::ReadError => {
                directory.sub_error = true;
                -1
            }
            Special::Pattern => -2,
            Special::OtherFilesystem => -3,
            Special::KernelFilesystem => -4,
        };
        directory.last_entry = Some(self.item_start(item_type, directory.last_entry, name)?);
        self.item_end();
        Ok(())
    }

    fn add_stat(
        &mut self,
        directory: &mut BinaryDirectory,
        name: &[u8],
        stat: &Stat,
    ) -> io::Result<()> {
        directory.items += 1;
        if stat.entry_type != EntryType::HardLink {
            directory.size = directory.size.saturating_add(stat.size);
            directory.blocks = directory.blocks.saturating_add(stat.blocks);
        }
        directory.last_entry =
            Some(self.item_start(entry_type(stat.entry_type), directory.last_entry, name)?);
        self.field(ItemKey::Asize, stat.size);
        self.field(ItemKey::Dsize, disk_usage(stat.blocks));
        if stat.entry_type == EntryType::HardLink {
            directory
                .inodes
                .entry(stat.inode)
                .and_modify(|inode| inode.found = inode.found.saturating_add(1))
                .or_insert(Inode {
                    size: stat.size,
                    blocks: stat.blocks,
                    link_count: stat.link_count,
                    found: 1,
                });
            self.field(ItemKey::Ino, stat.inode);
            self.field(ItemKey::Nlink, stat.link_count.into());
        }
        self.item_extended(stat);
        self.item_end();
        Ok(())
    }

    fn add_directory(
        &mut self,
        parent: &mut BinaryDirectory,
        name: &[u8],
        stat: &Stat,
    ) -> io::Result<BinaryDirectory> {
        parent.items += 1;
        parent.size = parent.size.saturating_add(stat.size);
        parent.blocks = parent.blocks.saturating_add(stat.blocks);
        Ok(BinaryDirectory::new(name, stat))
    }

    fn set_read_error(&mut self, directory: &mut BinaryDirectory) -> io::Result<()> {
        directory.read_error = true;
        Ok(())
    }

    fn finish_directory(
        &mut self,
        mut directory: BinaryDirectory,
        parent: Option<&mut BinaryDirectory>,
    ) -> io::Result<()> {
        let same_device = parent
            .as_ref()
            .is_some_and(|parent| parent.stat.device == directory.stat.device);
        if let Some(parent) = parent {
            // The links on another device are counted here, as no link outside can share them.
            if !same_device {
                directory.count_links(None);
            }
            parent.items = parent.items.saturating_add(directory.items);
            parent.size = parent.size.saturating_add(directory.size);
            parent.blocks = parent.blocks.saturating_add(directory.blocks);
            if directory.sub_error || directory.read_error {
                parent.sub_error = true;
            }
            if same_device {
                directory.count_links(Some(parent));
            }
            parent.last_entry = Some(self.item_start(0, parent.last_entry, &directory.name)?);
        } else {
            directory.count_links(None);
            self.root_itemref = self.item_start(0, None, &directory.name)?;
        }

        let stat = directory.stat;
        self.field(ItemKey::Asize, stat.size);
        self.field(ItemKey::Dsize, disk_usage(stat.blocks));
        if !same_device {
            self.field(ItemKey::Dev, stat.device);
        }
        if directory.read_error || directory.sub_error {
            self.cbor_head(CBOR_POSITIVE, ItemKey::Rderr as u64);
            self.cbor_head(
                CBOR_SIMPLE,
                if directory.read_error {
                    CBOR_TRUE
                } else {
                    CBOR_FALSE
                },
            );
        }
        self.field(ItemKey::Cumasize, directory.size.saturating_add(stat.size));
        self.field(
            ItemKey::Cumdsize,
            disk_usage(directory.blocks.saturating_add(stat.blocks)),
        );
        if directory.shared_size > 0 {
            self.field(ItemKey::Shrasize, directory.shared_size);
        }
        if directory.shared_blocks > 0 {
            self.field(ItemKey::Shrdsize, disk_usage(directory.shared_blocks));
        }
        self.field(ItemKey::Items, directory.items);
        self.reference_field(ItemKey::Sub, directory.last_entry);
        self.item_extended(&stat);
        self.item_end();
        Ok(())
    }
}
