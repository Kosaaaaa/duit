use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{self, ErrorKind};

use duit_format::{BINARY_SIGNATURE, BinaryExport, BinaryExportError, JsonExport, import};
use duit_model::Sink;
use insta::assert_snapshot;

use crate::json_export::{BrokenPipe, FailingFlush, directory, file, many_files};
#[cfg(feature = "test-ncdu")]
use crate::ncdu_import;

/// The names of the item keys, by number.
const KEYS: [&str; 19] = [
    "type", "name", "prev", "asize", "dsize", "dev", "rderr", "cumasize", "cumdsize", "shrasize",
    "shrdsize", "items", "sub", "ino", "nlink", "uid", "gid", "mode", "mtime",
];

/// Every kind of entry, with hard links that are all found in a subdirectory, links that are
/// found in different directories, links that are not all found, a link without a link count, and
/// links on another device.
const TREE: &[u8] = b"[1,2,{\"progname\":\"ncdu\",\"progver\":\"2.9.2\",\"timestamp\":1},
[{\"name\":\"/root\",\"asize\":4096,\"dsize\":4096,\"dev\":1},
{\"name\":\"file\",\"asize\":100,\"dsize\":4096,\"uid\":501,\"gid\":20,\"mode\":33188,\"mtime\":1700000000},
{\"name\":\"raw \xff\xfe\"},
{\"name\":\"fifo\",\"notreg\":true},
{\"name\":\"a\",\"asize\":10,\"dsize\":4096,\"ino\":42,\"hlnkc\":true,\"nlink\":2},
{\"name\":\"error\",\"read_error\":true},
{\"name\":\"pattern\",\"excluded\":\"pattern\"},
[{\"name\":\"otherfs\",\"excluded\":\"otherfs\"}],
[{\"name\":\"kernfs\",\"excluded\":\"kernfs\"}],
[{\"name\":\"links\",\"asize\":4096,\"dsize\":4096},
{\"name\":\"b\",\"asize\":10,\"dsize\":4096,\"ino\":42,\"hlnkc\":true,\"nlink\":2},
{\"name\":\"c\",\"asize\":20,\"dsize\":4096,\"ino\":43,\"hlnkc\":true,\"nlink\":2},
{\"name\":\"d\",\"asize\":20,\"dsize\":4096,\"ino\":43,\"hlnkc\":true,\"nlink\":2},
{\"name\":\"e\",\"asize\":30,\"dsize\":4096,\"ino\":44,\"hlnkc\":true,\"nlink\":3},
{\"name\":\"old\",\"asize\":40,\"dsize\":4096,\"ino\":45,\"hlnkc\":true}],
[{\"name\":\"unreadable\",\"asize\":4096,\"dsize\":4096,\"read_error\":true},
{\"name\":\"file\",\"asize\":1,\"dsize\":512}],
[{\"name\":\"other\",\"asize\":4096,\"dsize\":4096,\"dev\":7},
{\"name\":\"f\",\"asize\":10,\"dsize\":4096,\"ino\":42,\"hlnkc\":true,\"nlink\":2},
[{\"name\":\"empty\",\"asize\":4096,\"dsize\":4096}]],
[{\"name\":\"huge\"},
{\"name\":\"first\",\"asize\":18446744073709551615,\"dsize\":18446744073709551615},
{\"name\":\"second\",\"asize\":18446744073709551615,\"dsize\":18446744073709551615}]]]
";

/// A root and a file with all extended fields, and a file with some.
const EXTENDED: &[u8] = b"[1,2,{\"progname\":\"ncdu\",\"progver\":\"2.9.2\",\"timestamp\":1},
[{\"name\":\"/root\",\"asize\":4096,\"dsize\":4096,\"dev\":1,\"uid\":501,\"gid\":20,\"mode\":16877,\"mtime\":1},
{\"name\":\"file\",\"asize\":1,\"dsize\":512,\"uid\":0,\"gid\":4294967295,\"mode\":33188,\"mtime\":1700000000},
{\"name\":\"some\",\"gid\":20,\"mtime\":5}]]
";

/// Import the JSON export `input` into a [`BinaryExport`] and return the binary export.
fn convert(input: &[u8], extended: bool, compress_level: u8, block_size: Option<usize>) -> Vec<u8> {
    let (head, rest) = input.split_at(8);
    let mut export = BinaryExport::new(Vec::new(), extended, compress_level, block_size)
        .expect("signature is written");
    import(&mut export, rest, head).expect("input is imported");
    export.finish().expect("export is flushed")
}

/// Like [`convert`] at the default compression level.
///
/// With the `test-ncdu` feature, ncdu converts the same input with `ncdu -f- -O-`. With one
/// thread, the blocks of both exports must hold the same bytes once decompressed.
fn export(input: &[u8], extended: bool, block_size: Option<usize>) -> Vec<u8> {
    let output = convert(input, extended, 4, block_size);
    #[cfg(feature = "test-ncdu")]
    {
        let kibibytes = block_size.map(|size| (size / 1024).to_string());
        let mut arguments = vec!["-O-"];
        if extended {
            arguments.push("-e");
        }
        if let Some(kibibytes) = &kibibytes {
            arguments.extend(["--export-block-size", kibibytes]);
        }
        let result = ncdu_import(input, &arguments);
        assert!(
            result.status.success(),
            "ncdu failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(Blocks::parse(&result.stdout), Blocks::parse(&output));
    }
    output
}

/// The parts of a binary export, with each data block decompressed.
#[derive(Debug, PartialEq, Eq)]
struct Blocks {
    /// The number and the decompressed data of each data block, in the order of the file.
    data: Vec<(u32, Vec<u8>)>,
    /// The itemref of the root, as the index block holds it.
    root: u64,
}

impl Blocks {
    /// Split `export` into its blocks. Every block must repeat its header as its footer, and the
    /// index block must come last and give the offset and length of every data block.
    fn parse(export: &[u8]) -> Self {
        let u32_at = |offset: usize| {
            u32::from_be_bytes(export[offset..offset + 4].try_into().expect("4 bytes"))
        };
        let u64_at = |offset: usize| {
            u64::from_be_bytes(export[offset..offset + 8].try_into().expect("8 bytes"))
        };
        assert_eq!(&export[..8], BINARY_SIGNATURE);
        let mut data = Vec::new();
        let mut positions = Vec::new();
        let mut offset = 8;
        loop {
            let header = u32_at(offset);
            let length = usize::try_from(header & 0x0fff_ffff).expect("length fits");
            assert_eq!(u32_at(offset + length - 4), header, "footer repeats header");
            if header >> 28 == 1 {
                assert_eq!(offset + length, export.len(), "index block is last");
                let entries: Vec<u64> = (offset + 4..offset + length - 12)
                    .step_by(8)
                    .map(u64_at)
                    .collect();
                assert_eq!(entries, positions, "index points to the data blocks");
                return Self {
                    data,
                    root: u64_at(offset + length - 12),
                };
            }
            assert_eq!(header >> 28, 0, "data block");
            let number = u32_at(offset + 4);
            assert_eq!(
                usize::try_from(number).expect("number fits"),
                data.len(),
                "blocks are numbered in order"
            );
            let compressed = &export[offset + 8..offset + length - 4];
            let decompressed = zstd::decode_all(compressed).expect("block is a zstd frame");
            data.push((number, decompressed));
            positions.push(
                (u64::try_from(offset).expect("offset fits") << 24)
                    + u64::try_from(length).expect("length fits"),
            );
            offset += length;
        }
    }
}

/// An item of a binary export: its itemref and its fields in the order of the file, with
/// references shown as the `block:offset` of the item they point to.
struct Item {
    itemref: u64,
    fields: Vec<(&'static str, String)>,
}

impl Item {
    fn field(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| value.as_str())
    }
}

/// The `block:offset` form of `itemref`.
fn reference(itemref: u64) -> String {
    format!("{}:{}", itemref >> 24, itemref & 0xff_ffff)
}

/// A reader of the CBOR items in one decompressed block.
struct Reader<'a> {
    data: &'a [u8],
    offset: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> u8 {
        let byte = self.data[self.offset];
        self.offset += 1;
        byte
    }

    /// Read the head of a data item and return its major type and argument, or `None` for an
    /// additional information that the writer does not use.
    fn head(&mut self) -> Option<(u8, u64)> {
        let byte = self.byte();
        let length = match byte & 0x1f {
            argument @ 0..24 => return Some((byte >> 5, u64::from(argument))),
            24 => 1,
            25 => 2,
            26 => 4,
            27 => 8,
            28.. => return None,
        };
        let argument = (0..length).fold(0, |value, _| (value << 8) | u64::from(self.byte()));
        Some((byte >> 5, argument))
    }

    /// Read the item at `itemref`, which starts at the current offset.
    fn item(&mut self, itemref: u64) -> Item {
        assert_eq!(self.byte(), 0xbf, "item is an indefinite map");
        let mut fields = Vec::new();
        while self.data[self.offset] != 0xff {
            let (major, key) = self.head().expect("argument is 0 to 27");
            assert_eq!(major, 0, "key is an unsigned integer");
            let key = KEYS[usize::try_from(key).expect("key fits")];
            let (major, argument) = self.head().expect("argument is 0 to 27");
            let value = match (major, key) {
                (0, "prev" | "sub") => Some(reference(argument)),
                (0, _) => Some(argument.to_string()),
                (1, "type") => Some(format!("-{}", argument + 1)),
                (1, "prev" | "sub") => Some(reference(itemref - argument - 1)),
                (2, _) => {
                    let length = usize::try_from(argument).expect("length fits");
                    let name = &self.data[self.offset..self.offset + length];
                    self.offset += length;
                    Some(format!("\"{}\"", name.escape_ascii()))
                }
                (7, _) if argument == 20 => Some("false".to_string()),
                (7, _) if argument == 21 => Some("true".to_string()),
                _ => None,
            }
            .expect("known major type");
            fields.push((key, value));
        }
        self.offset += 1;
        Item { itemref, fields }
    }
}

/// The items of `export` in the order of the file, and the itemref of the root.
fn items(export: &[u8]) -> (Vec<Item>, u64) {
    let blocks = Blocks::parse(export);
    let mut items = Vec::new();
    for (number, data) in &blocks.data {
        let mut reader = Reader { data, offset: 0 };
        while reader.offset < data.len() {
            let offset = u64::try_from(reader.offset).expect("offset fits");
            items.push(reader.item((u64::from(*number) << 24) | offset));
        }
    }
    (items, blocks.root)
}

/// The items of `export`, one per line under the number of their block, and the root.
fn decode(export: &[u8]) -> String {
    let (items, root) = items(export);
    let mut rendered = String::new();
    for item in &items {
        let fields: Vec<String> = item
            .fields
            .iter()
            .map(|(key, value)| format!("{key}: {value}"))
            .collect();
        let _ = writeln!(
            rendered,
            "{} {{{}}}",
            reference(item.itemref),
            fields.join(", ")
        );
    }
    let _ = write!(rendered, "root: {}", reference(root));
    rendered
}

/// Items are written after the items that they point to, and a directory after its entries. A
/// directory holds the totals of its entries, with each hard link counted once in the directory
/// that holds all of its links.
#[test]
fn tree() {
    assert_snapshot!(decode(&export(TREE, false, None)), @r#"
    0:0 {type: 1, name: "file", asize: 100, dsize: 4096}
    0:17 {type: 1, name: "raw \xff\xfe", prev: 0:0, asize: 0, dsize: 0}
    0:35 {type: 2, name: "fifo", prev: 0:17, asize: 0, dsize: 0}
    0:51 {type: 3, name: "a", prev: 0:35, asize: 10, dsize: 4096, ino: 42, nlink: 2}
    0:71 {type: -1, name: "error", prev: 0:51}
    0:84 {type: -2, name: "pattern", prev: 0:71}
    0:99 {type: -3, name: "otherfs", prev: 0:84}
    0:114 {type: -4, name: "kernfs", prev: 0:99}
    0:128 {type: 3, name: "b", asize: 10, dsize: 4096, ino: 42, nlink: 2}
    0:146 {type: 3, name: "c", prev: 0:128, asize: 20, dsize: 4096, ino: 43, nlink: 2}
    0:166 {type: 3, name: "d", prev: 0:146, asize: 20, dsize: 4096, ino: 43, nlink: 2}
    0:186 {type: 3, name: "e", prev: 0:166, asize: 30, dsize: 4096, ino: 44, nlink: 3}
    0:207 {type: 3, name: "old", prev: 0:186, asize: 40, dsize: 4096, ino: 45, nlink: 0}
    0:230 {type: 0, name: "links", prev: 0:114, asize: 4096, dsize: 4096, cumasize: 4196, cumdsize: 20480, shrasize: 40, shrdsize: 8192, items: 5, sub: 0:207}
    0:271 {type: 1, name: "file", asize: 1, dsize: 512}
    0:287 {type: 0, name: "unreadable", prev: 0:230, asize: 4096, dsize: 4096, rderr: true, cumasize: 4097, cumdsize: 4608, items: 1, sub: 0:271}
    0:328 {type: 3, name: "f", asize: 10, dsize: 4096, ino: 42, nlink: 2}
    0:346 {type: 0, name: "empty", prev: 0:328, asize: 4096, dsize: 4096, cumasize: 4096, cumdsize: 4096, items: 0}
    0:377 {type: 0, name: "other", prev: 0:287, asize: 4096, dsize: 4096, dev: 7, cumasize: 8202, cumdsize: 12288, shrasize: 10, shrdsize: 4096, items: 2, sub: 0:346}
    0:420 {type: 1, name: "first", asize: 18446744073709551615, dsize: 18446744073709551104}
    0:451 {type: 1, name: "second", prev: 0:420, asize: 18446744073709551615, dsize: 18446744073709551104}
    0:486 {type: 0, name: "huge", prev: 0:377, asize: 0, dsize: 0, cumasize: 18446744073709551615, cumdsize: 18446744073709551615, items: 2, sub: 0:451}
    0:528 {type: 0, name: "/root", asize: 4096, dsize: 4096, dev: 1, rderr: false, cumasize: 18446744073709551615, cumdsize: 18446744073709551615, shrasize: 30, shrdsize: 4096, items: 22, sub: 0:486}
    root: 0:528
    "#);
}

/// With `extended`, the extended fields that an entry has are written.
#[test]
fn extended() {
    assert_snapshot!(decode(&export(EXTENDED, true, None)), @r#"
    0:0 {type: 1, name: "file", asize: 1, dsize: 512, uid: 0, gid: 4294967295, mode: 33188, mtime: 1700000000}
    0:34 {type: 1, name: "some", prev: 0:0, asize: 0, dsize: 0, gid: 20, mtime: 5}
    0:55 {type: 0, name: "/root", asize: 4096, dsize: 4096, dev: 1, cumasize: 4097, cumdsize: 4608, items: 2, sub: 0:34, uid: 501, gid: 20, mode: 16877, mtime: 1}
    root: 0:55
    "#);
}

/// A read error marks a directory even after its first entry, unlike in the JSON export.
#[test]
fn read_error() {
    let mut export = BinaryExport::new(Vec::new(), false, 4, None).expect("signature is written");
    let build = |export: &mut BinaryExport<Vec<u8>>| -> io::Result<()> {
        let mut root = export.create_root(b"/root", &directory(1))?;
        let mut after = export.add_directory(&mut root, b"after", &directory(1))?;
        export.add_stat(&mut after, b"file", &file(1, 1))?;
        export.set_read_error(&mut after)?;
        export.set_read_error(&mut after)?;
        export.finish_directory(after, Some(&mut root))?;
        export.finish_directory(root, None)
    };
    build(&mut export).expect("items are written");
    let output = export.finish().expect("export is flushed");
    assert_snapshot!(decode(&output), @r#"
    0:0 {type: 1, name: "file", asize: 1, dsize: 512}
    0:16 {type: 0, name: "after", asize: 4096, dsize: 4096, rderr: true, cumasize: 4097, cumdsize: 4608, items: 1, sub: 0:0}
    0:49 {type: 0, name: "/root", asize: 4096, dsize: 4096, dev: 1, rderr: false, cumasize: 8193, cumdsize: 8704, items: 2, sub: 0:16}
    root: 0:49
    "#);
}

/// The JSON export of [`many_files`].
fn many_files_json() -> Vec<u8> {
    let mut export = JsonExport::new(Vec::new(), 0, false, None).expect("header is written");
    many_files(&mut export).expect("items are written");
    export.finish().expect("export is flushed")
}

/// A smaller block size splits the items over more blocks, and the references between blocks
/// lead from the root to every entry.
#[test]
fn block_size() {
    let input = many_files_json();
    let default = export(&input, false, None);
    assert_eq!(Blocks::parse(&default).data.len(), 3);
    let small = export(&input, false, Some(4096));
    assert_eq!(Blocks::parse(&small).data.len(), 37);

    let (items, root) = items(&small);
    let by_reference: HashMap<String, &Item> = items
        .iter()
        .map(|item| (reference(item.itemref), item))
        .collect();
    let mut names = Vec::new();
    let mut next = by_reference[&reference(root)].field("sub");
    while let Some(itemref) = next {
        let item = by_reference[itemref];
        names.push(item.field("name").expect("item has a name").to_string());
        next = item.field("prev");
    }
    names.reverse();
    let expected: Vec<String> = (0..5000).map(|index| format!("\"file{index}\"")).collect();
    assert_eq!(names, expected);
}

/// A higher compression level writes a smaller export of the same blocks.
#[test]
fn compress_level() {
    let input = many_files_json();
    let fastest = convert(&input, false, 1, None);
    let smallest = convert(&input, false, 19, None);
    assert!(
        smallest.len() < fastest.len(),
        "level 19 wrote {} bytes, level 1 wrote {}",
        smallest.len(),
        fastest.len()
    );
    assert_eq!(Blocks::parse(&smallest), Blocks::parse(&fastest));
}

/// An item that does not fit in an empty block ends the export with an error. An item takes its
/// name and at most 211 more bytes.
#[test]
fn path_too_long() {
    let result = |length: usize| {
        let mut export =
            BinaryExport::new(Vec::new(), false, 4, Some(4096)).expect("signature is written");
        let root = export.create_root(&vec![b'a'; length], &directory(1))?;
        export.finish_directory(root, None)
    };
    assert!(result(4096 - 211).is_ok());
    let error = result(4096 - 210).expect_err("the root does not fit");
    assert_eq!(
        error
            .get_ref()
            .and_then(|error| error.downcast_ref::<BinaryExportError>()),
        Some(&BinaryExportError::PathTooLong)
    );
}

/// The signature is written and flushed when the export starts.
#[test]
fn write_error() {
    let error = BinaryExport::new(BrokenPipe, false, 4, None)
        .err()
        .expect("the signature is not written");
    assert_eq!(error.kind(), ErrorKind::BrokenPipe);
    let error = BinaryExport::new(FailingFlush, false, 4, None)
        .err()
        .expect("the signature is not flushed");
    assert_eq!(error.kind(), ErrorKind::BrokenPipe);
}
