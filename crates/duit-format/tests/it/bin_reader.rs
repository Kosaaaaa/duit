use std::io::Cursor;

use duit_format::{BinaryExport, BinaryReader, JsonExport, import};

use crate::bin_export::TREE;

/// The binary export of [`TREE`].
fn tree() -> Vec<u8> {
    let (head, rest) = TREE.split_at(8);
    let mut export = BinaryExport::new(Vec::new(), true, 4, None).expect("signature is written");
    import(&mut export, rest, head).expect("input is imported");
    export.finish().expect("export is flushed")
}

/// Whether `input`, a binary export with its signature, is read without an error.
fn reads(input: &[u8]) -> bool {
    let Ok(mut reader) = BinaryReader::open(Cursor::new(input)) else {
        return false;
    };
    let mut export = JsonExport::new(Vec::new(), 0, true, None).expect("header is written");
    reader.import(&mut export).is_ok()
}

/// Every prefix of an export is an error.
#[test]
fn truncated() {
    let export = tree();
    assert!(reads(&export));
    for length in 8..export.len() {
        assert!(!reads(&export[..length]), "prefix of {length} bytes");
    }
}

/// An export with any one byte changed is read or is an error, and never panics.
#[test]
fn corrupted() {
    let export = tree();
    for offset in 8..export.len() {
        for flip in [0x01, 0x80, 0xff] {
            let mut corrupted = export.clone();
            corrupted[offset] ^= flip;
            reads(&corrupted);
        }
    }
}
