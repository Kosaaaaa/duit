use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::error::Error;
use std::fmt;
use std::io::{self, Write};

use duit_model::{EntryType, Sink, Special, Stat};
use zstd::bulk::Compressor;
use zstd::zstd_safe::compress_bound;

use crate::bin_format::{
    BLOCK_LENGTH_BITS, BlockPosition, DATA_BLOCK, FOOTER_LENGTH, HEADER_LENGTH,
    INDEFINITE_OR_BREAK, INDEX_BLOCK, INDEX_ENTRY_LENGTH, ITEM_KEY_COUNT, ItemKey, ItemRef,
    MAX_BLOCK_LENGTH, MAX_HEAD_LENGTH, Major, NUMBER_LENGTH, ROOT_REFERENCE_LENGTH, SIMPLE_FALSE,
    SIMPLE_TRUE,
};
use crate::item::ItemType;

/// The first bytes of a binary export.
pub const BINARY_SIGNATURE: &[u8; 8] = b"\xbfncduEX1";

const MAP_DELIMITERS_LENGTH: usize = 2;
const MAX_KEY_LENGTH: usize = 2;
const MAX_ITEM_LEN: usize =
    MAP_DELIMITERS_LENGTH + (MAX_KEY_LENGTH + MAX_HEAD_LENGTH) * ITEM_KEY_COUNT;

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

/// ncdu has no message for a block too long for its header, so the export reports it as too large.
const OVERSIZE_BLOCK_ERROR: BinaryExportError = BinaryExportError::FileTooLarge;

fn block_length(length: usize) -> Result<u32, BinaryExportError> {
    u32::try_from(length)
        .ok()
        .filter(|&length| length <= MAX_BLOCK_LENGTH)
        .ok_or(OVERSIZE_BLOCK_ERROR)
}

fn block_header(id: u32, length: usize) -> io::Result<[u8; 4]> {
    let length = block_length(length)?;
    Ok(((id << BLOCK_LENGTH_BITS) | length).to_be_bytes())
}

fn index_block_length(entries: usize) -> usize {
    HEADER_LENGTH + INDEX_ENTRY_LENGTH * entries + ROOT_REFERENCE_LENGTH + FOOTER_LENGTH
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
    buffer: Vec<u8>,
    buffer_size: usize,
    block_number: Option<u32>,
    itemref: ItemRef,
    index: Vec<Option<BlockPosition>>,
    file_offset: u64,
    root_itemref: ItemRef,
}

#[derive(Clone, Copy)]
struct Inode {
    size: u64,
    blocks: u64,
    link_count: u32,
    found: u32,
}

impl Inode {
    fn all_links_found(self) -> bool {
        self.link_count > 0 && self.found >= self.link_count
    }
}

/// The handle of a directory in a [`BinaryExport`].
pub struct BinaryDirectory {
    name: Vec<u8>,
    stat: Stat,
    last_entry: Option<ItemRef>,
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

    fn add_subdirectory_totals(&mut self, subdirectory: &Self) {
        self.items = self.items.saturating_add(subdirectory.items);
        self.size = self.size.saturating_add(subdirectory.size);
        self.blocks = self.blocks.saturating_add(subdirectory.blocks);
        if subdirectory.sub_error || subdirectory.read_error {
            self.sub_error = true;
        }
    }

    fn add_link_sizes(&mut self) {
        for inode in self.inodes.values() {
            self.size = self.size.saturating_add(inode.size);
            self.blocks = self.blocks.saturating_add(inode.blocks);
            if inode.link_count > 1 && inode.found < inode.link_count {
                self.shared_size = self.shared_size.saturating_add(inode.size);
                self.shared_blocks = self.shared_blocks.saturating_add(inode.blocks);
            }
        }
    }

    fn hand_links_to(&mut self, parent: &mut Self) {
        for (number, inode) in self.inodes.drain() {
            if inode.all_links_found() {
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
            itemref: ItemRef(0),
            index: Vec::new(),
            file_offset: BINARY_SIGNATURE.len() as u64,
            root_itemref: ItemRef(0),
        })
    }

    /// Write the last data block and the index block after the root is finished, flush the
    /// export, and return the writer. Without a root, the index block refers to a root at 0.
    pub fn finish(mut self) -> io::Result<W> {
        self.flush_block(0)?;
        while let Some(None) = self.index.last() {
            self.index.pop();
        }
        let length = index_block_length(self.index.len());
        let header = block_header(INDEX_BLOCK, length)?;
        let mut block = Vec::with_capacity(length);
        block.extend_from_slice(&header);
        for position in &self.index {
            let packed = position.map_or(0, |position| position.0);
            block.extend_from_slice(&packed.to_be_bytes());
        }
        block.extend_from_slice(&self.root_itemref.0.to_be_bytes());
        block.extend_from_slice(&header);
        self.out.write_all(&block)?;
        self.out.flush()?;
        Ok(self.out)
    }

    fn flush_block(&mut self, expected_length: usize) -> io::Result<()> {
        if expected_length > self.buffer_size {
            return Err(BinaryExportError::PathTooLong.into());
        }
        if let Some(number) = self.block_number
            && !self.buffer.is_empty()
        {
            // The frame is compressed into the block, as ncdu does, so that no second copy of it
            // is in memory while the block is written.
            let start = HEADER_LENGTH + NUMBER_LENGTH;
            let bound = compress_bound(self.buffer.len());
            let mut block = vec![0; start + bound + FOOTER_LENGTH];
            let compressed = self
                .compressor
                .compress_to_buffer(&self.buffer, &mut block[start..start + bound])?;
            let length = start + compressed + FOOTER_LENGTH;
            block.truncate(length);
            let header = block_header(DATA_BLOCK, length)?;
            block[..HEADER_LENGTH].copy_from_slice(&header);
            block[HEADER_LENGTH..start].copy_from_slice(&number.to_be_bytes());
            block[start + compressed..].copy_from_slice(&header);
            if self.file_offset >= 1 << 40 {
                return Err(BinaryExportError::FileTooLarge.into());
            }
            if let Some(entry) = usize::try_from(number)
                .ok()
                .and_then(|number| self.index.get_mut(number))
            {
                *entry = Some(BlockPosition::new(self.file_offset, length as u64));
            }
            self.file_offset += length as u64;
            self.out.write_all(&block)?;
        }

        self.buffer.clear();
        let number =
            u32::try_from(self.index.len()).map_err(|_| BinaryExportError::TooManyBlocks)?;
        self.block_number = Some(number);
        self.index.push(None);
        if index_block_length(self.index.len()) >= 1 << BLOCK_LENGTH_BITS {
            return Err(BinaryExportError::TooManyBlocks.into());
        }
        self.buffer_size = block_size(self.configured_block_size, number);
        self.buffer.reserve(self.buffer_size);
        Ok(())
    }

    /// Write the head of a CBOR data item of `major` type with `argument`. An argument above 23
    /// follows the head in 1, 2, 4, or 8 bytes, which the additional information `0x18` to `0x1b`
    /// selects.
    fn cbor_head(&mut self, major: Major, argument: u64) {
        let major = (major as u8) << 5;
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
        self.cbor_head(Major::Positive, key as u64);
        self.cbor_head(Major::Positive, value);
    }

    /// Write a reference to the item at `itemref`, if any. A reference into the current block is
    /// written as a negative offset from the item being written, which compresses better.
    fn reference_field(&mut self, key: ItemKey, itemref: Option<ItemRef>) {
        let Some(itemref) = itemref else {
            return;
        };
        self.cbor_head(Major::Positive, key as u64);
        if self.block_number.map(u64::from) == Some(itemref.block()) {
            self.cbor_head(Major::Negative, self.itemref.0 - itemref.0 - 1);
        } else {
            self.cbor_head(Major::Positive, itemref.0);
        }
    }

    fn item_start(
        &mut self,
        kind: ItemType,
        previous: Option<ItemRef>,
        name: &[u8],
    ) -> io::Result<ItemRef> {
        let minimum_length = name.len() + MAX_ITEM_LEN;
        if self.block_number.is_none() || self.buffer.len() + minimum_length > self.buffer_size {
            self.flush_block(minimum_length)?;
        }
        let block_number = self.block_number.map_or(0, u64::from);
        self.itemref = ItemRef::new(block_number, self.buffer.len() as u64);
        self.buffer
            .push(((Major::Map as u8) << 5) | INDEFINITE_OR_BREAK);
        self.cbor_head(Major::Positive, ItemKey::Type as u64);
        let (major, argument) = type_head(kind);
        self.cbor_head(major, argument);
        self.cbor_head(Major::Positive, ItemKey::Name as u64);
        self.cbor_head(Major::Bytes, name.len() as u64);
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

    fn item_end(&mut self) {
        self.buffer
            .push(((Major::Simple as u8) << 5) | INDEFINITE_OR_BREAK);
    }
}

fn type_head(kind: ItemType) -> (Major, u64) {
    match kind {
        ItemType::Stat(EntryType::Directory) => (Major::Positive, 0),
        ItemType::Stat(EntryType::Regular) => (Major::Positive, 1),
        ItemType::Stat(EntryType::NonRegular) => (Major::Positive, 2),
        ItemType::Stat(EntryType::HardLink) => (Major::Positive, 3),
        ItemType::Special(Special::ReadError) => (Major::Negative, 0),
        ItemType::Special(Special::Pattern) => (Major::Negative, 1),
        ItemType::Special(Special::OtherFilesystem) => (Major::Negative, 2),
        ItemType::Special(Special::KernelFilesystem) => (Major::Negative, 3),
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
        if special == Special::ReadError {
            directory.sub_error = true;
        }
        directory.last_entry =
            Some(self.item_start(ItemType::Special(special), directory.last_entry, name)?);
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
            Some(self.item_start(ItemType::Stat(stat.entry_type), directory.last_entry, name)?);
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
        let kind = ItemType::Stat(EntryType::Directory);
        match parent {
            Some(parent) if same_device => {
                parent.add_subdirectory_totals(&directory);
                directory.add_link_sizes();
                directory.hand_links_to(parent);
                parent.last_entry =
                    Some(self.item_start(kind, parent.last_entry, &directory.name)?);
            }
            Some(parent) => {
                directory.add_link_sizes();
                parent.add_subdirectory_totals(&directory);
                parent.last_entry =
                    Some(self.item_start(kind, parent.last_entry, &directory.name)?);
            }
            None => {
                directory.add_link_sizes();
                self.root_itemref = self.item_start(kind, None, &directory.name)?;
            }
        }

        let stat = directory.stat;
        self.field(ItemKey::Asize, stat.size);
        self.field(ItemKey::Dsize, disk_usage(stat.blocks));
        if !same_device {
            self.field(ItemKey::Dev, stat.device);
        }
        if directory.read_error || directory.sub_error {
            self.cbor_head(Major::Positive, ItemKey::Rderr as u64);
            self.cbor_head(
                Major::Simple,
                if directory.read_error {
                    SIMPLE_TRUE
                } else {
                    SIMPLE_FALSE
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
