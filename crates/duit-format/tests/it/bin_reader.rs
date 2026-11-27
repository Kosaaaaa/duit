use std::collections::HashSet;
use std::fmt::Write as _;
use std::io::Cursor;

use duit_format::{BinaryExport, BinaryReader, JsonExport, Listing, import};
use duit_model::{DirectoryId, EntryKind, Tree};
use insta::assert_snapshot;

use crate::bin_export::TREE;

fn tree() -> Vec<u8> {
    let (head, rest) = TREE.split_at(8);
    let mut export = BinaryExport::new(Vec::new(), true, 4, None).expect("signature is written");
    import(&mut export, rest, head).expect("input is imported");
    export.finish().expect("export is flushed")
}

fn reads(input: &[u8]) -> bool {
    let Ok(mut reader) = BinaryReader::open(Cursor::new(input)) else {
        return false;
    };
    let mut export = JsonExport::new(Vec::new(), 0, true, None).expect("header is written");
    reader.import(&mut export).is_ok()
}

fn browses(input: &[u8]) -> bool {
    let Ok(mut reader) = BinaryReader::open(Cursor::new(input)) else {
        return false;
    };
    let mut directories = vec![reader.root()];
    let mut visited = HashSet::new();
    while let Some(itemref) = directories.pop() {
        if !visited.insert(itemref) {
            continue;
        }
        match reader.read_directory(itemref, true, &mut |_, _| {}) {
            Ok(Some(listing)) => {
                directories.extend(listing.directories.iter().map(|listed| listed.itemref));
            }
            Ok(None) | Err(_) => return false,
        }
    }
    true
}

fn binary(input: &[u8]) -> Vec<u8> {
    let (head, rest) = input.split_at(8);
    let mut export = BinaryExport::new(Vec::new(), true, 4, None).expect("signature is written");
    import(&mut export, rest, head).expect("input is imported");
    export.finish().expect("export is flushed")
}

fn root_listing(input: &[u8]) -> String {
    let mut reader = BinaryReader::open(Cursor::new(input)).expect("index is read");
    let root = reader.root();
    let listing = reader
        .read_directory(root, true, &mut |_, _| {})
        .expect("root is read")
        .expect("root is a directory");
    let Listing {
        tree,
        directory,
        directories,
    } = listing;
    let mut rendered = String::new();
    let line = |rendered: &mut String, tree: &Tree, id: DirectoryId, empty: Option<bool>| {
        let directory = tree.directory(id);
        let entry = tree.entry(directory.entry);
        let _ = writeln!(
            rendered,
            "{} blocks={} size={} items={} shared_blocks={} shared_size={} read_error={} \
             sub_error={}{}",
            String::from_utf8_lossy(tree.name(directory.entry)),
            entry.blocks,
            entry.size,
            directory.items,
            directory.shared_blocks,
            directory.shared_size,
            directory.read_error,
            directory.sub_error,
            empty.map_or(String::new(), |empty| format!(" empty={empty}")),
        );
    };
    line(&mut rendered, &tree, directory, None);
    for id in tree.entries(directory) {
        let entry = tree.entry(id);
        match entry.kind {
            EntryKind::Directory(directory) => {
                let listed = directories
                    .iter()
                    .find(|listed| listed.id == directory)
                    .expect("subdirectory is listed");
                rendered.push_str("  ");
                line(&mut rendered, &tree, directory, Some(listed.empty));
            }
            EntryKind::HardLink(link) => {
                let link = tree.link(link);
                let _ = writeln!(
                    rendered,
                    "  {} blocks={} size={} inode={} link_count={}",
                    String::from_utf8_lossy(tree.name(id)),
                    entry.blocks,
                    entry.size,
                    link.inode(),
                    link.link_count()
                );
            }
            EntryKind::Regular | EntryKind::NonRegular | EntryKind::Special(_) => {
                let _ = writeln!(
                    rendered,
                    "  {} {:?} blocks={} size={}",
                    String::from_utf8_lossy(tree.name(id)),
                    entry.kind,
                    entry.blocks,
                    entry.size
                );
            }
        }
    }
    rendered
}

#[test]
fn read_directory() {
    let export = binary(
        br#"[1,2,{},
[{"name":"/root","asize":4096,"dsize":4096},
[{"name":"parent","asize":4096,"dsize":4096},
{"name":"broken","read_error":true},
{"name":"a","asize":10,"dsize":4096,"ino":42,"hlnkc":true,"nlink":2}],
[{"name":"unreadable","asize":4096,"dsize":4096,"read_error":true}],
{"name":"b","asize":10,"dsize":4096,"ino":42,"hlnkc":true,"nlink":2},
{"name":"pattern","excluded":"pattern"}]]"#,
    );
    assert_snapshot!(root_listing(&export), @"
    /root blocks=32 size=12298 items=6 shared_blocks=0 shared_size=0 read_error=false sub_error=true
      pattern Special(Pattern) blocks=0 size=0
      b blocks=8 size=10 inode=42 link_count=2
      unreadable blocks=8 size=4096 items=0 shared_blocks=0 shared_size=0 read_error=true sub_error=false empty=true
      parent blocks=16 size=4106 items=2 shared_blocks=8 shared_size=10 read_error=false sub_error=true empty=false
    ");
}

#[test]
fn truncated() {
    let export = tree();
    assert!(reads(&export));
    for length in 8..export.len() {
        assert!(!reads(&export[..length]), "prefix of {length} bytes");
    }
}

#[test]
fn corrupted() {
    let export = tree();
    for offset in 8..export.len() {
        for flip in [0x01, 0x80, 0xff] {
            let mut corrupted = export.clone();
            corrupted[offset] ^= flip;
            reads(&corrupted);
            browses(&corrupted);
        }
    }
}
