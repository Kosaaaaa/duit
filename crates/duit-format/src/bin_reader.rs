use std::collections::HashSet;
use std::io::{self, Read, Seek, SeekFrom};

use duit_model::{EntryType, Sink, Special, Stat};
use zstd::zstd_safe;

/// The number of decompressed data blocks that a [`BinaryReader`] keeps.
const CACHED_BLOCKS: usize = 8;

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
    /// `offset << 24 | length`, then the root reference, each a big-endian `u64`.
    index: Vec<u8>,
    /// The blocks that were decompressed last.
    blocks: Vec<Block>,
    /// The number of block lookups, which orders the blocks by their last use.
    lookups: u64,
}

/// A decompressed data block.
struct Block {
    number: u32,
    last_use: u64,
    data: Vec<u8>,
}

/// The type of an item. An item of type 0 is a directory, `Stat(EntryType::Directory)`.
#[derive(Clone, Copy)]
enum ItemType {
    Stat(EntryType),
    Special(Special),
}

/// The fields of the item that is being read.
struct Item {
    kind: ItemType,
    stat: Stat,
    name: Vec<u8>,
    read_error: bool,
    /// The entry before this one in its directory.
    previous: Option<u64>,
    /// The last entry of a directory.
    last_entry: Option<u64>,
}

/// A directory whose entries are being read.
struct OpenDirectory<D> {
    directory: D,
    /// The entry to read next, which is the entry before the one read last.
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
        read_at(&mut input, &mut header, size - 4).map_err(BinaryOpenError::Read)?;
        let header = u32::from_be_bytes(header);
        if header >> 28 != 1 || header & 7 != 0 {
            return Err(BinaryOpenError::Corrupt);
        }
        // The length without the header and the footer. ncdu reads past the end of an index that
        // is too short to hold the root reference.
        let length = (header & 0x0fff_ffff)
            .checked_sub(8)
            .filter(|&length| length >= 8 && u64::from(length) < size)
            .ok_or(BinaryOpenError::Corrupt)?;
        // An index that would start before the file is read by ncdu at a negative offset, which
        // fails as an invalid argument.
        let start = size
            .checked_sub(u64::from(length) + 4)
            .ok_or_else(|| BinaryOpenError::Read(io::ErrorKind::InvalidInput.into()))?;
        let mut index = vec![0; usize::try_from(length).map_err(|_| BinaryOpenError::Corrupt)?];
        read_at(&mut input, &mut index, start).map_err(BinaryOpenError::Read)?;
        Ok(Self {
            input,
            index,
            blocks: Vec::with_capacity(CACHED_BLOCKS),
            lookups: 0,
        })
    }

    /// Read the tree into `sink`, depth-first from the root. The entries of each directory arrive
    /// in the reverse order of the export, from the last entry to the first, as in ncdu.
    ///
    /// Each entry has the device of its directory unless it has its own. A stack of the open
    /// directories replaces recursion, so that deep nesting cannot overflow the call stack, and an
    /// item that is reached a second time is an error, so that references in a cycle end.
    pub fn import<S: Sink>(&mut self, sink: &mut S) -> Result<(), BinaryImportError> {
        let root = self.root();
        let mut item = Item {
            kind: ItemType::Stat(EntryType::Directory),
            stat: Stat::default(),
            name: Vec::new(),
            read_error: false,
            previous: None,
            last_entry: None,
        };
        let mut visited = HashSet::from([root]);
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
            if !visited.insert(itemref) {
                return Err(BinaryImportError::Corrupt { itemref });
            }
            self.read_item(itemref, open.device, &mut item)?;
            open.next = item.previous;
            match item.kind {
                ItemType::Stat(EntryType::Directory) => {
                    item.stat.entry_type = EntryType::Directory;
                    let mut child = sink
                        .add_directory(&mut open.directory, &item.name, &item.stat)
                        .map_err(BinaryImportError::Sink)?;
                    if item.read_error {
                        sink.set_read_error(&mut child)
                            .map_err(BinaryImportError::Sink)?;
                    }
                    directories.push(OpenDirectory {
                        directory: child,
                        next: item.last_entry,
                        device: item.stat.device,
                    });
                }
                ItemType::Stat(
                    entry_type @ (EntryType::Regular | EntryType::NonRegular | EntryType::HardLink),
                ) => {
                    item.stat.entry_type = entry_type;
                    sink.add_stat(&mut open.directory, &item.name, &item.stat)
                        .map_err(BinaryImportError::Sink)?;
                }
                ItemType::Special(special) => sink
                    .add_special(&mut open.directory, &item.name, special)
                    .map_err(BinaryImportError::Sink)?,
            }
        }
        Ok(())
    }

    /// The reference of the root, which ends the index.
    fn root(&self) -> u64 {
        self.index
            .last_chunk::<8>()
            .map_or(0, |root| u64::from_be_bytes(*root))
    }

    /// Read the item at `itemref` into `item`, with `device` as the device of its directory.
    fn read_item(
        &mut self,
        itemref: u64,
        device: u64,
        item: &mut Item,
    ) -> Result<(), BinaryImportError> {
        let corrupt = || BinaryImportError::Corrupt { itemref };
        // A reference has 56 bits.
        let number = u32::try_from(itemref >> 24).map_err(|_| corrupt())?;
        let block = self.block(number, itemref)?;
        let offset = usize::try_from(itemref & 0xff_ffff).map_err(|_| corrupt())?;
        block
            .get(offset..)
            .filter(|data| !data.is_empty())
            .and_then(|data| parse_item(data, itemref, device, item))
            .ok_or_else(corrupt)
    }

    /// The decompressed data block `number`, which holds the item at `itemref`.
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

    /// Read and decompress the data block `number`, which holds the item at `itemref`. As in
    /// ncdu, the header, the number, and the footer of the block are not checked.
    fn load(&mut self, number: u32, itemref: u64) -> Result<Vec<u8>, BinaryImportError> {
        let corrupt = || BinaryImportError::Corrupt { itemref };
        // As in ncdu, the root reference at the end of the index reads as the position of the
        // block after the last.
        let position = usize::try_from(number)
            .ok()
            .and_then(|number| number.checked_mul(8))
            .and_then(|start| self.index.get(start..)?.first_chunk::<8>())
            .map(|position| u64::from_be_bytes(*position))
            .ok_or_else(corrupt)?;
        let offset = position >> 24;
        let length = position & 0xff_ffff;
        if length <= 12 {
            return Err(corrupt());
        }
        let mut frame = vec![0; usize::try_from(length - 12).map_err(|_| corrupt())?];
        read_at(&mut self.input, &mut frame, offset + 8).map_err(|error| {
            if error.kind() == io::ErrorKind::UnexpectedEof {
                corrupt()
            } else {
                BinaryImportError::Read(error)
            }
        })?;
        let expected = match zstd_safe::get_frame_content_size(&frame) {
            Ok(Some(size)) if size > 0 && size < 1 << 24 => size,
            Ok(Some(_) | None) | Err(_) => return Err(corrupt()),
        };
        let mut data = vec![0; usize::try_from(expected).map_err(|_| corrupt())?];
        let result = zstd_safe::decompress(data.as_mut_slice(), &frame).unwrap_or_else(|code| code);
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

/// Fill `buffer` from `input` at `offset`.
fn read_at(input: &mut (impl Read + Seek), buffer: &mut [u8], offset: u64) -> io::Result<()> {
    input.seek(SeekFrom::Start(offset))?;
    input.read_exact(buffer)
}

/// Parse the item at `itemref` from `data`, which starts with it, into `item`, with `device` as
/// the device of its directory. `None` if it is not valid.
///
/// The fields are read in order, so a later field overrides an earlier one with the same key.
/// Unknown keys are skipped, and so are the fields that only the browser reads, such as the
/// totals of a directory.
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

    let mut cbor = Cbor { buffer: data };
    let head = cbor.next()?;
    if head.major != Major::Map {
        return None;
    }
    // The keys left in a map of definite length.
    let mut keys_left = (!head.indefinite).then_some(head.argument);
    loop {
        let key = match &mut keys_left {
            Some(0) => break,
            Some(left) => {
                *left -= 1;
                cbor.next()?
            }
            None => {
                let key = cbor.next()?;
                if key.is_break() {
                    break;
                }
                key
            }
        };
        if key.major != Major::Positive || key.argument > 31 {
            cbor.skip(key)?;
            let value = cbor.next()?;
            cbor.skip(value)?;
            continue;
        }
        let value = cbor.next()?;
        let stat = &mut item.stat;
        let extended = &mut stat.extended;
        match key.argument {
            0 => {
                item.kind = item_type(i32::try_from(value.integer()?).ok()?);
                has_type = true;
            }
            1 => {
                let name = cbor.bytes(value)?;
                item.name.clear();
                item.name.extend_from_slice(name);
            }
            2 => item.previous = Some(value.itemref(itemref)?),
            3 => stat.size = value.unsigned()?,
            4 => stat.blocks = value.unsigned::<u64>()? / 512,
            5 => stat.device = value.unsigned()?,
            6 => item.read_error = value.is_true(),
            12 => item.last_entry = Some(value.itemref(itemref)?),
            13 => stat.inode = value.unsigned()?,
            14 => stat.link_count = value.unsigned::<u32>().filter(|&count| count < 1 << 31)?,
            15 => extended.uid = Some(value.unsigned()?),
            16 => extended.gid = Some(value.unsigned()?),
            17 => extended.mode = Some(value.unsigned()?),
            18 => extended.mtime = Some(value.unsigned()?),
            7..=11 | 19.. => cbor.skip(value)?,
        }
    }
    (has_type && !item.name.is_empty()).then_some(())
}

/// The item type of the value of the `type` field. An unknown type is a pattern if it is negative
/// and a non-regular file otherwise, as in ncdu.
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

/// The major type of a CBOR data item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Major {
    Positive,
    Negative,
    Bytes,
    Text,
    Array,
    Map,
    Tag,
    Simple,
}

/// The head of a CBOR data item.
#[derive(Debug, Clone, Copy)]
struct Value {
    major: Major,
    /// Whether the length is indefinite, for a string, an array, or a map. For a simple value,
    /// this is the break that ends an item of indefinite length.
    indefinite: bool,
    argument: u64,
}

impl Value {
    fn is_break(self) -> bool {
        self.major == Major::Simple && self.indefinite
    }

    fn is_true(self) -> bool {
        self.major == Major::Simple && self.argument == 21
    }

    /// The value of an integer.
    fn integer(self) -> Option<i128> {
        match self.major {
            Major::Positive => Some(i128::from(self.argument)),
            Major::Negative => Some(-i128::from(self.argument) - 1),
            Major::Bytes | Major::Text | Major::Array | Major::Map | Major::Tag | Major::Simple => {
                None
            }
        }
    }

    fn unsigned<T: TryFrom<i128>>(self) -> Option<T> {
        T::try_from(self.integer()?).ok()
    }

    /// The item that this reference from the item at `current` refers to. A negative reference
    /// is an offset back from `current` in the same block.
    fn itemref(self, current: u64) -> Option<u64> {
        match self.major {
            Major::Positive => Some(self.argument),
            Major::Negative if self.argument < current & 0xff_ffff => {
                Some(current - self.argument - 1)
            }
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

/// A reader of the CBOR data items in a decompressed block. Every method returns `None` for data
/// that is not valid.
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

    /// The `N`-byte big-endian argument that follows a head.
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
        let (argument, indefinite) = match byte & 0x1f {
            argument @ 0..=0x17 => (u64::from(argument), false),
            0x18 => (self.argument::<1>()?, false),
            0x19 => (self.argument::<2>()?, false),
            0x1a => (self.argument::<4>()?, false),
            0x1b => (self.argument::<8>()?, false),
            0x1f => match major {
                Major::Bytes | Major::Text | Major::Array | Major::Map | Major::Simple => (0, true),
                Major::Positive | Major::Negative | Major::Tag => return None,
            },
            0x1c..=0x1e | 0x20..=u8::MAX => return None,
        };
        Some(Value {
            major,
            indefinite,
            argument,
        })
    }

    /// The next data item, after any tags.
    fn next(&mut self) -> Option<Value> {
        loop {
            let value = self.head()?;
            if value.major != Major::Tag {
                return Some(value);
            }
        }
    }

    /// The content of a byte or text string of definite length. Text is not checked to be
    /// UTF-8.
    fn bytes(&mut self, value: Value) -> Option<&'a [u8]> {
        match value.major {
            Major::Bytes | Major::Text if !value.indefinite => self.take(value.argument),
            Major::Bytes
            | Major::Text
            | Major::Positive
            | Major::Negative
            | Major::Array
            | Major::Map
            | Major::Tag
            | Major::Simple => None,
        }
    }

    /// Skip the content of `value`, with a stack instead of recursion so that deep nesting
    /// cannot overflow the call stack. As in ncdu, an array or a map of more than 2^24 entries is
    /// not valid, and the chunks of an indefinite string are skipped as any data items.
    fn skip(&mut self, value: Value) -> Option<()> {
        // The data items left to skip in each container, or `None` for a container of indefinite
        // length, which ends at a break. Containers of definite length inside one another share
        // a count, so that only containers of indefinite length deepen the stack.
        let mut containers: Vec<Option<u64>> = Vec::new();
        let mut value = value;
        loop {
            match value.major {
                Major::Bytes | Major::Text | Major::Array | Major::Map if value.indefinite => {
                    containers.push(None);
                }
                Major::Bytes | Major::Text => {
                    self.take(value.argument)?;
                }
                Major::Array | Major::Map => {
                    if value.argument > 1 << 24 {
                        return None;
                    }
                    let count = if value.major == Major::Map {
                        value.argument * 2
                    } else {
                        value.argument
                    };
                    match containers.last_mut() {
                        Some(Some(left)) => *left += count,
                        Some(None) | None => containers.push(Some(count)),
                    }
                }
                Major::Positive | Major::Negative | Major::Tag | Major::Simple => {}
            }
            value = loop {
                match containers.last_mut() {
                    None => return Some(()),
                    Some(Some(0)) => {
                        containers.pop();
                    }
                    Some(Some(left)) => {
                        *left -= 1;
                        break self.next()?;
                    }
                    Some(None) => {
                        let next = self.next()?;
                        if !next.is_break() {
                            break next;
                        }
                        containers.pop();
                    }
                }
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Cbor, Value};

    /// Port of the upstream test "CBOR int parsing".
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

    /// Port of the upstream test "CBOR string parsing".
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

    /// Port of the upstream test "CBOR skip parsing".
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
}
