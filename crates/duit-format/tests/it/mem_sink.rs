use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::io;

use duit_format::import;
use duit_model::{
    DirectoryId, EntryId, EntryKind, EntryType, Extended, MemorySink, Sink, Special, Stat, Tree,
};
use insta::assert_snapshot;

use crate::bin_export::{Item, TREE, export, items, reference};
use crate::json_export::{directory, file};

/// The JSON export that `crates/duit/tests/fixtures/generate.sh` converts with ncdu.
const SOURCE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../duit/tests/fixtures/source.json"
));

/// The binary export that ncdu 2.9.2 wrote of [`SOURCE`].
const SOURCE_BINARY: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../duit/tests/fixtures/tree.ncdu"
));

/// The compressed JSON export of a tree over several blocks, which ncdu 2.9.2 wrote.
const BLOCKS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../duit/tests/fixtures/blocks.json.zst"
));

/// The binary export that ncdu 2.9.2 wrote of the same tree as [`BLOCKS`].
const BLOCKS_BINARY: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../duit/tests/fixtures/blocks.ncdu"
));

/// Import the JSON export `input` into a [`MemorySink`] and return the tree.
fn tree_of(input: &[u8], extended: bool) -> Tree {
    let (head, rest) = input.split_at(8);
    let mut sink = MemorySink::new(extended);
    import(&mut sink, rest, head).expect("input is imported");
    sink.finish()
}

/// The tree that `build` adds to a [`MemorySink`].
fn build(extended: bool, build: impl FnOnce(&mut MemorySink) -> io::Result<()>) -> Tree {
    let mut sink = MemorySink::new(extended);
    build(&mut sink).expect("entries are added");
    sink.finish()
}

/// The root of `tree`.
fn root(tree: &Tree) -> DirectoryId {
    tree.root().expect("tree has a root")
}

/// The entry `name` of `directory`.
fn find(tree: &Tree, directory: DirectoryId, name: &[u8]) -> EntryId {
    tree.entries(directory)
        .find(|&entry| tree.name(entry) == name)
        .expect("entry exists")
}

/// The directory of the entry `name` of `directory`.
fn find_directory(tree: &Tree, directory: DirectoryId, name: &[u8]) -> DirectoryId {
    match tree.entry(find(tree, directory, name)).kind {
        EntryKind::Directory(directory) => Some(directory),
        EntryKind::HardLink(_)
        | EntryKind::Regular
        | EntryKind::NonRegular
        | EntryKind::Special(_) => None,
    }
    .expect("entry is a directory")
}

/// Every entry of `tree`, one per line and indented by depth, with the entries of a directory in
/// the order of the tree.
fn render(tree: &Tree) -> String {
    let mut rendered = String::new();
    if let Some(root) = tree.root() {
        render_entry(tree, tree.directory(root).entry, 0, &mut rendered);
    }
    rendered
}

fn render_entry(tree: &Tree, id: EntryId, depth: usize, rendered: &mut String) {
    let entry = tree.entry(id);
    let mut fields = vec![
        format!("asize: {}", entry.size),
        format!("blocks: {}", entry.blocks),
    ];
    let kind = match entry.kind {
        EntryKind::Directory(directory) => {
            let directory = tree.directory(directory);
            fields.push(format!("items: {}", directory.items));
            fields.push(format!("dev: {}", tree.device(directory.device)));
            if directory.shared_size > 0 || directory.shared_blocks > 0 {
                fields.push(format!("shared_asize: {}", directory.shared_size));
                fields.push(format!("shared_blocks: {}", directory.shared_blocks));
            }
            if directory.read_error {
                fields.push("read_error".to_string());
            }
            if directory.sub_error {
                fields.push("sub_error".to_string());
            }
            "directory".to_string()
        }
        EntryKind::HardLink(link) => {
            let link = tree.link(link);
            fields.push(format!("ino: {}", link.inode));
            fields.push(format!("nlink: {}", link.link_count));
            "hard link".to_string()
        }
        EntryKind::Regular => "regular".to_string(),
        EntryKind::NonRegular => "non-regular".to_string(),
        EntryKind::Special(special) => format!("{special:?}"),
    };
    if let Some(entry_extended) = tree.extended(id) {
        let extended = &entry_extended.extended;
        let known = [
            ("uid", extended.uid.map(u64::from)),
            ("gid", extended.gid.map(u64::from)),
            ("mode", extended.mode.map(u64::from)),
            ("mtime", extended.mtime),
        ];
        for (key, value) in known {
            if let Some(value) = value {
                fields.push(format!("{key}: {value}"));
            }
        }
        fields.push(format!("latest_mtime: {}", entry_extended.latest_mtime));
    }
    let _ = writeln!(
        rendered,
        "{:indent$}\"{}\" {kind} {{{}}}",
        "",
        tree.name(id).escape_ascii(),
        fields.join(", "),
        indent = depth * 2,
    );
    if let EntryKind::Directory(directory) = entry.kind {
        for child in tree.entries(directory) {
            render_entry(tree, child, depth + 1, rendered);
        }
    }
}

/// The totals of each directory of `tree` by name: the item count, the cumulative sizes, and the
/// shared sizes when they are above zero, as ncdu writes them to a binary export.
fn totals(tree: &Tree) -> BTreeMap<String, String> {
    fn walk(tree: &Tree, directory: DirectoryId, totals: &mut BTreeMap<String, String>) {
        for child in tree.entries(directory) {
            if let EntryKind::Directory(directory) = tree.entry(child).kind {
                walk(tree, directory, totals);
            }
        }
        let data = tree.directory(directory);
        let entry = tree.entry(data.entry);
        let mut rendered = format!(
            "items: {}, cumasize: {}, cumdsize: {}",
            data.items,
            entry.size,
            entry.blocks.saturating_mul(512)
        );
        if data.shared_size > 0 {
            let _ = write!(rendered, ", shrasize: {}", data.shared_size);
        }
        if data.shared_blocks > 0 {
            let _ = write!(
                rendered,
                ", shrdsize: {}",
                data.shared_blocks.saturating_mul(512)
            );
        }
        totals.insert(
            format!("\"{}\"", tree.name(data.entry).escape_ascii()),
            rendered,
        );
    }
    let mut totals = BTreeMap::new();
    walk(tree, root(tree), &mut totals);
    totals
}

/// The totals that ncdu wrote for each directory of the binary export `export`, as in [`totals`].
fn ncdu_totals(export: &[u8]) -> BTreeMap<String, String> {
    fn walk(items: &HashMap<String, &Item>, item: &Item, totals: &mut BTreeMap<String, String>) {
        let field = |key| item.field(key).expect("directory has the field");
        let mut next = item.field("sub");
        while let Some(child) = next.map(|reference| items[reference]) {
            if child.field("type") == Some("0") {
                walk(items, child, totals);
            }
            next = child.field("prev");
        }
        let mut rendered = format!(
            "items: {}, cumasize: {}, cumdsize: {}",
            field("items"),
            field("cumasize"),
            field("cumdsize")
        );
        for key in ["shrasize", "shrdsize"] {
            if let Some(value) = item.field(key) {
                let _ = write!(rendered, ", {key}: {value}");
            }
        }
        totals.insert(field("name").to_string(), rendered);
    }
    let (items, root) = items(export);
    let by_reference: HashMap<String, &Item> = items
        .iter()
        .map(|item| (reference(item.itemref), item))
        .collect();
    let mut totals = BTreeMap::new();
    walk(&by_reference, by_reference[&reference(root)], &mut totals);
    totals
}

/// The tree of the golden source has every kind of entry. The entries of a directory are in
/// reverse order, and a directory holds the totals of its entries, with the two links to one inode
/// counted once. The directory that holds only one of them shares its size. Each directory has the
/// totals that ncdu wrote.
#[test]
fn golden() {
    let tree = tree_of(SOURCE, true);
    assert_eq!(totals(&tree), ncdu_totals(SOURCE_BINARY));
    assert_snapshot!(render(&tree), @r#"
    "/home/user/project" directory {asize: 1071497, blocks: 64, items: 15, dev: 2049, sub_error, uid: 1000, gid: 1000, mode: 16877, mtime: 1700000000, latest_mtime: 1700000800}
      "raw \xff\xfe" regular {asize: 2, blocks: 0}
      "quote \" backslash \\ tab \t control \x01 \xc3\xa9" regular {asize: 1, blocks: 0}
      "sparse" regular {asize: 1048576, blocks: 0, mtime: 1700000800, latest_mtime: 1700000800}
      "proc" KernelFilesystem {asize: 0, blocks: 0}
      "mnt" OtherFilesystem {asize: 0, blocks: 0}
      "node_modules" Pattern {asize: 0, blocks: 0}
      "broken" ReadError {asize: 0, blocks: 0}
      "socket" non-regular {asize: 0, blocks: 0, uid: 1000, gid: 1000, mode: 49645, mtime: 1700000700, latest_mtime: 1700000700}
      "also-linked" hard link {asize: 300, blocks: 8, ino: 104, nlink: 2, uid: 1000, gid: 1000, mode: 33188, mtime: 1700000400, latest_mtime: 1700000400}
      "unreadable" directory {asize: 4096, blocks: 8, items: 0, dev: 2049, read_error, uid: 0, gid: 0, mode: 16832, mtime: 1700000600, latest_mtime: 1700000600}
      "empty" directory {asize: 4096, blocks: 8, items: 0, dev: 2049, uid: 1000, gid: 1000, mode: 16877, mtime: 1700000500, latest_mtime: 1700000500}
      "src" directory {asize: 9396, blocks: 32, items: 2, dev: 2049, shared_asize: 300, shared_blocks: 8, uid: 1000, gid: 1000, mode: 16877, mtime: 1700000200, latest_mtime: 1700000400}
        "linked" hard link {asize: 300, blocks: 8, ino: 104, nlink: 2, uid: 1000, gid: 1000, mode: 33188, mtime: 1700000400, latest_mtime: 1700000400}
        "main.rs" regular {asize: 5000, blocks: 16, uid: 1000, gid: 1000, mode: 33188, mtime: 1700000300, latest_mtime: 1700000300}
      "README.md" regular {asize: 1234, blocks: 8, uid: 1000, gid: 1000, mode: 33188, mtime: 1700000100, latest_mtime: 1700000100}
    "#);
}

/// The totals of a tree over several blocks without hard links are those that ncdu wrote.
#[test]
fn golden_blocks() {
    let tree = tree_of(BLOCKS, false);
    let totals = totals(&tree);
    assert_eq!(totals.len(), 8);
    assert_eq!(totals, ncdu_totals(BLOCKS_BINARY));
}

/// The hard links of [`TREE`]: links in one directory, links in two directories, links that are
/// not all found, a link without a link count, and a link on another device. Each directory has
/// the totals that ncdu wrote to its binary export, except for the shared sizes of the root. The
/// binary export never counts a link below another device as shared above that device, and the
/// in-memory tree of ncdu does: only one of the two links of `f` on device 7 is in the tree, so
/// the root shares its size too.
#[test]
fn hard_links() {
    let tree = tree_of(TREE, false);
    let mut expected = ncdu_totals(&export(TREE, false, None));
    let root = expected.get_mut("\"/root\"").expect("root has totals");
    *root = root.replace(
        "shrasize: 30, shrdsize: 4096",
        "shrasize: 40, shrdsize: 8192",
    );
    assert_eq!(totals(&tree), expected);
}

/// Links to one inode with different link counts count as many links as the tree holds, so the
/// directory that holds both of them shares nothing.
#[test]
fn inconsistent_link_count() {
    let tree = build(false, |sink| {
        let mut root = sink.create_root(b"/root", &directory(1))?;
        let mut sub = sink.add_directory(&mut root, b"sub", &directory(1))?;
        for (name, link_count) in [(&b"two"[..], 2), (b"three", 3)] {
            let stat = Stat {
                link_count,
                ..link(1, 10)
            };
            sink.add_stat(&mut sub, name, &stat)?;
        }
        sink.finish_directory(sub, Some(&mut root))?;
        sink.finish_directory(root, None)
    });
    assert_snapshot!(render(&tree), @r#"
    "/root" directory {asize: 8202, blocks: 17, items: 3, dev: 1}
      "sub" directory {asize: 4106, blocks: 9, items: 2, dev: 1}
        "three" hard link {asize: 10, blocks: 1, ino: 1, nlink: 3}
        "two" hard link {asize: 10, blocks: 1, ino: 1, nlink: 2}
    "#);
}

/// A directory counts the links of an inode in the directories below it, however deep, so only
/// the directory that holds one of the two links shares its size.
#[test]
fn nested_links() {
    let tree = build(false, |sink| {
        let mut root = sink.create_root(b"/root", &directory(1))?;
        let mut outer = sink.add_directory(&mut root, b"outer", &directory(1))?;
        let mut middle = sink.add_directory(&mut outer, b"middle", &directory(1))?;
        let mut inner = sink.add_directory(&mut middle, b"inner", &directory(1))?;
        sink.add_stat(&mut inner, b"deep", &link(1, 10))?;
        sink.finish_directory(inner, Some(&mut middle))?;
        sink.add_stat(&mut middle, b"shallow", &link(1, 10))?;
        sink.finish_directory(middle, Some(&mut outer))?;
        sink.finish_directory(outer, Some(&mut root))?;
        sink.finish_directory(root, None)
    });
    assert_snapshot!(render(&tree), @r#"
    "/root" directory {asize: 16394, blocks: 33, items: 5, dev: 1}
      "outer" directory {asize: 12298, blocks: 25, items: 4, dev: 1}
        "middle" directory {asize: 8202, blocks: 17, items: 3, dev: 1}
          "shallow" hard link {asize: 10, blocks: 1, ino: 1, nlink: 2}
          "inner" directory {asize: 4106, blocks: 9, items: 1, dev: 1, shared_asize: 10, shared_blocks: 1}
            "deep" hard link {asize: 10, blocks: 1, ino: 1, nlink: 2}
    "#);
}

/// Entries on several devices, with read errors below the root and extended fields on only some
/// of the directories.
fn mixed(sink: &mut MemorySink) -> io::Result<()> {
    let dated = |mtime| Extended {
        mtime: Some(mtime),
        ..Extended::default()
    };
    let owned = Stat {
        extended: Extended {
            uid: Some(501),
            ..Extended::default()
        },
        ..directory(1)
    };
    let mut root = sink.create_root(b"/root", &owned)?;
    let early = Stat {
        extended: dated(100),
        ..file(1, 1)
    };
    sink.add_stat(&mut root, b"early", &early)?;
    let mut plain = sink.add_directory(&mut root, b"plain", &directory(1))?;
    let late = Stat {
        extended: dated(500),
        ..file(2, 1)
    };
    sink.add_stat(&mut plain, b"late", &late)?;
    sink.finish_directory(plain, Some(&mut root))?;
    let with_mtime = Stat {
        extended: dated(50),
        ..directory(1)
    };
    let mut nested = sink.add_directory(&mut root, b"dated", &with_mtime)?;
    let later = Stat {
        extended: dated(300),
        ..file(3, 1)
    };
    sink.add_stat(&mut nested, b"later", &later)?;
    sink.finish_directory(nested, Some(&mut root))?;
    let mut other = sink.add_directory(&mut root, b"other", &directory(2))?;
    let mut back = sink.add_directory(&mut other, b"back", &directory(1))?;
    let mut deeper = sink.add_directory(&mut back, b"deeper", &directory(1))?;
    sink.set_read_error(&mut deeper)?;
    sink.finish_directory(deeper, Some(&mut back))?;
    sink.finish_directory(back, Some(&mut other))?;
    sink.finish_directory(other, Some(&mut root))?;
    let mut broken = sink.add_directory(&mut root, b"broken", &directory(1))?;
    sink.add_special(&mut broken, b"error", Special::ReadError)?;
    sink.add_special(&mut broken, b"pattern", Special::Pattern)?;
    sink.finish_directory(broken, Some(&mut root))?;
    sink.finish_directory(root, None)
}

/// A device that comes back has the identifier that it had first. A directory gains the latest
/// mtime of its entries, and of the directories below that have extended fields, even when it has
/// no mtime of its own. A read error marks every directory above it. Without `extended`, no entry
/// has extended fields.
#[test]
fn tree() {
    let tree = build(true, mixed);
    let root = root(&tree);
    let other = find_directory(&tree, root, b"other");
    let back = find_directory(&tree, other, b"back");
    assert_eq!(tree.directory(back).device, tree.directory(root).device);
    assert_ne!(tree.directory(root).device, tree.directory(other).device);
    assert_snapshot!(render(&tree), @r#"
    "/root" directory {asize: 28678, blocks: 59, items: 11, dev: 1, sub_error, uid: 501, latest_mtime: 300}
      "broken" directory {asize: 4096, blocks: 8, items: 2, dev: 1, sub_error}
        "pattern" Pattern {asize: 0, blocks: 0}
        "error" ReadError {asize: 0, blocks: 0}
      "other" directory {asize: 12288, blocks: 24, items: 2, dev: 2, sub_error}
        "back" directory {asize: 8192, blocks: 16, items: 1, dev: 1, sub_error}
          "deeper" directory {asize: 4096, blocks: 8, items: 0, dev: 1, read_error}
      "dated" directory {asize: 4099, blocks: 9, items: 1, dev: 1, mtime: 50, latest_mtime: 300}
        "later" regular {asize: 3, blocks: 1, mtime: 300, latest_mtime: 300}
      "plain" directory {asize: 4098, blocks: 9, items: 1, dev: 1}
        "late" regular {asize: 2, blocks: 1, mtime: 500, latest_mtime: 500}
      "early" regular {asize: 1, blocks: 1, mtime: 100, latest_mtime: 100}
    "#);
    assert_snapshot!(render(&build(false, mixed)), @r#"
    "/root" directory {asize: 28678, blocks: 59, items: 11, dev: 1, sub_error}
      "broken" directory {asize: 4096, blocks: 8, items: 2, dev: 1, sub_error}
        "pattern" Pattern {asize: 0, blocks: 0}
        "error" ReadError {asize: 0, blocks: 0}
      "other" directory {asize: 12288, blocks: 24, items: 2, dev: 2, sub_error}
        "back" directory {asize: 8192, blocks: 16, items: 1, dev: 1, sub_error}
          "deeper" directory {asize: 4096, blocks: 8, items: 0, dev: 1, read_error}
      "dated" directory {asize: 4099, blocks: 9, items: 1, dev: 1}
        "later" regular {asize: 3, blocks: 1}
      "plain" directory {asize: 4098, blocks: 9, items: 1, dev: 1}
        "late" regular {asize: 2, blocks: 1}
      "early" regular {asize: 1, blocks: 1}
    "#);
}

/// Zeroing a directory takes its totals from the directories above it and clears it and its
/// entries. The read errors that were below it still mark its parent until the parent is updated.
#[test]
fn zero_stats() {
    let mut tree = build(false, mixed);
    let root = root(&tree);
    for name in [&b"other"[..], b"broken"] {
        let entry = find(&tree, root, name);
        tree.zero_stats(entry, Some(root));
    }
    assert_snapshot!(render(&tree), @r#"
    "/root" directory {asize: 12294, blocks: 27, items: 5, dev: 1, sub_error}
      "broken" directory {asize: 0, blocks: 0, items: 0, dev: 1}
        "pattern" Pattern {asize: 0, blocks: 0}
        "error" ReadError {asize: 0, blocks: 0}
      "other" directory {asize: 0, blocks: 0, items: 0, dev: 2}
        "back" directory {asize: 0, blocks: 0, items: 0, dev: 1}
          "deeper" directory {asize: 0, blocks: 0, items: 0, dev: 1}
      "dated" directory {asize: 4099, blocks: 9, items: 1, dev: 1}
        "later" regular {asize: 3, blocks: 1}
      "plain" directory {asize: 4098, blocks: 9, items: 1, dev: 1}
        "late" regular {asize: 2, blocks: 1}
      "early" regular {asize: 1, blocks: 1}
    "#);
    tree.update_sub_error(root);
    assert!(!tree.directory(root).sub_error);
}

/// A hard link of `inode` with a link count of 2.
fn link(inode: u64, size: u64) -> Stat {
    Stat {
        entry_type: EntryType::HardLink,
        inode,
        link_count: 2,
        ..file(size, 1)
    }
}

/// Zeroing a directory takes the hard links below it out of the accounting, and
/// [`Tree::add_all_stats`] counts the links that remain elsewhere again, now as shared. Zeroing a
/// hard link takes its size from the directories above it twice, as ncdu does: once with the
/// accounting of its inode, and once as the size of the entry.
#[test]
fn zero_stats_links() {
    let mut tree = build(false, |sink| {
        let mut root = sink.create_root(b"/root", &directory(1))?;
        sink.add_stat(&mut root, b"a", &link(1, 10))?;
        let mut links = sink.add_directory(&mut root, b"links", &directory(1))?;
        sink.add_stat(&mut links, b"b", &link(1, 10))?;
        sink.add_stat(&mut links, b"c", &link(2, 20))?;
        sink.add_stat(&mut links, b"d", &link(2, 20))?;
        sink.finish_directory(links, Some(&mut root))?;
        sink.finish_directory(root, None)
    });
    let root = root(&tree);
    assert_snapshot!(render(&tree), @r#"
    "/root" directory {asize: 8222, blocks: 18, items: 5, dev: 1}
      "links" directory {asize: 4126, blocks: 10, items: 3, dev: 1, shared_asize: 10, shared_blocks: 1}
        "d" hard link {asize: 20, blocks: 1, ino: 2, nlink: 2}
        "c" hard link {asize: 20, blocks: 1, ino: 2, nlink: 2}
        "b" hard link {asize: 10, blocks: 1, ino: 1, nlink: 2}
      "a" hard link {asize: 10, blocks: 1, ino: 1, nlink: 2}
    "#);
    let links = find(&tree, root, b"links");
    tree.zero_stats(links, Some(root));
    tree.add_all_stats();
    assert_snapshot!(render(&tree), @r#"
    "/root" directory {asize: 4106, blocks: 9, items: 1, dev: 1, shared_asize: 10, shared_blocks: 1}
      "links" directory {asize: 0, blocks: 0, items: 0, dev: 1}
        "d" hard link {asize: 0, blocks: 0, ino: 2, nlink: 2}
        "c" hard link {asize: 0, blocks: 0, ino: 2, nlink: 2}
        "b" hard link {asize: 0, blocks: 0, ino: 1, nlink: 2}
      "a" hard link {asize: 10, blocks: 1, ino: 1, nlink: 2}
    "#);
    let a = find(&tree, root, b"a");
    tree.zero_stats(a, Some(root));
    tree.add_all_stats();
    assert_snapshot!(render(&tree), @r#"
    "/root" directory {asize: 4086, blocks: 7, items: 0, dev: 1}
      "links" directory {asize: 0, blocks: 0, items: 0, dev: 1}
        "d" hard link {asize: 0, blocks: 0, ino: 2, nlink: 2}
        "c" hard link {asize: 0, blocks: 0, ino: 2, nlink: 2}
        "b" hard link {asize: 0, blocks: 0, ino: 1, nlink: 2}
      "a" hard link {asize: 0, blocks: 0, ino: 1, nlink: 2}
    "#);
}
