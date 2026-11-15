use std::fs;
use std::path::Path;

use insta::assert_snapshot;
use tempfile::TempDir;

use crate::{BINARY_SIGNATURE, duit, render, table, text};
#[cfg(feature = "test-ncdu")]
use crate::{assert_ncdu, ncdu, render_as_duit};

/// A directory with a copy of the golden files: the exports that ncdu 2.9.2 wrote of
/// `source.json`, and of a tree over several blocks, by `tests/fixtures/generate.sh`.
fn fixture() -> TempDir {
    let directory = TempDir::new().expect("temporary directory is created");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for entry in fs::read_dir(fixtures).expect("fixtures are listed") {
        let path = entry.expect("fixture is listed").path();
        let name = path.file_name().expect("fixture has a name");
        fs::copy(&path, directory.path().join(name)).expect("fixture is copied");
    }
    directory
}

/// duit reads the JSON, compressed JSON, and binary exports that ncdu wrote, with and without
/// `-e`. The binary reader lists the entries of each directory in reverse order, as in ncdu. An
/// export without extended fields gains none with `-e`, and one with them loses them without it.
#[test]
fn golden() {
    let directory = fixture();
    let root = directory.path();
    let cases: &[&[&str]] = &[
        &["-f", "tree.json", "-o-"],
        &["-f", "tree-extended.json", "-o-", "-e"],
        &["-f", "tree.ncdu", "-o-"],
        &["-f", "tree-extended.ncdu", "-o-", "-e"],
    ];
    // Each run writes the same output as the case after it.
    let same: &[(&[&str], &[&str])] = &[
        (&["-f", "tree.json", "-o-", "-e"], cases[0]),
        (&["-f", "tree-extended.json", "-o-"], cases[0]),
        (&["-f", "tree-extended.json.zst", "-o-", "-e"], cases[1]),
        (&["-f", "tree.ncdu", "-o-", "-e"], cases[2]),
        (&["-f", "tree-extended.ncdu", "-o-"], cases[2]),
    ];
    #[cfg(feature = "test-ncdu")]
    for arguments in cases
        .iter()
        .chain(same.iter().map(|(arguments, _)| arguments))
    {
        assert_ncdu(root, arguments);
    }
    for (arguments, reference) in same {
        assert_eq!(
            render(&duit(root, arguments)),
            render(&duit(root, reference)),
            "arguments: {arguments:?}"
        );
    }
    assert_snapshot!(table(root, cases), @r#"
    $ duit -f tree.json -o-
    exit_code: 0
    ----- stdout -----
    [1,2,{[METADATA]},
    [{"name":"/home/user/project","asize":4096,"dsize":4096,"dev":2049},
    {"name":"README.md","asize":1234,"dsize":4096},
    [{"name":"src","asize":4096,"dsize":4096},
    {"name":"main.rs","asize":5000,"dsize":8192},
    {"name":"linked","asize":300,"dsize":4096,"ino":104,"hlnkc":true,"nlink":2}],
    [{"name":"empty","asize":4096,"dsize":4096}],
    [{"name":"unreadable","asize":4096,"dsize":4096,"read_error":true}],
    {"name":"also-linked","asize":300,"dsize":4096,"ino":104,"hlnkc":true,"nlink":2},
    {"name":"socket","notreg":true},
    {"name":"broken","read_error":true},
    {"name":"node_modules","excluded":"pattern"},
    [{"name":"mnt","excluded":"otherfs"}],
    [{"name":"proc","excluded":"kernfs"}],
    {"name":"sparse","asize":1048576},
    {"name":"quote \" backslash \\ tab \t control \u0001 é","asize":1},
    {"name":"raw ��","asize":2}]]

    ----- stderr -----
    ^[[J

    $ duit -f tree-extended.json -o- -e
    exit_code: 0
    ----- stdout -----
    [1,2,{[METADATA]},
    [{"name":"/home/user/project","asize":4096,"dsize":4096,"dev":2049,"uid":1000,"gid":1000,"mode":16877,"mtime":1700000000},
    {"name":"README.md","asize":1234,"dsize":4096,"uid":1000,"gid":1000,"mode":33188,"mtime":1700000100},
    [{"name":"src","asize":4096,"dsize":4096,"uid":1000,"gid":1000,"mode":16877,"mtime":1700000200},
    {"name":"main.rs","asize":5000,"dsize":8192,"uid":1000,"gid":1000,"mode":33188,"mtime":1700000300},
    {"name":"linked","asize":300,"dsize":4096,"ino":104,"hlnkc":true,"nlink":2,"uid":1000,"gid":1000,"mode":33188,"mtime":1700000400}],
    [{"name":"empty","asize":4096,"dsize":4096,"uid":1000,"gid":1000,"mode":16877,"mtime":1700000500}],
    [{"name":"unreadable","asize":4096,"dsize":4096,"uid":0,"gid":0,"mode":16832,"mtime":1700000600,"read_error":true}],
    {"name":"also-linked","asize":300,"dsize":4096,"ino":104,"hlnkc":true,"nlink":2,"uid":1000,"gid":1000,"mode":33188,"mtime":1700000400},
    {"name":"socket","notreg":true,"uid":1000,"gid":1000,"mode":49645,"mtime":1700000700},
    {"name":"broken","read_error":true},
    {"name":"node_modules","excluded":"pattern"},
    [{"name":"mnt","excluded":"otherfs"}],
    [{"name":"proc","excluded":"kernfs"}],
    {"name":"sparse","asize":1048576,"mtime":1700000800},
    {"name":"quote \" backslash \\ tab \t control \u0001 é","asize":1},
    {"name":"raw ��","asize":2}]]

    ----- stderr -----
    ^[[J

    $ duit -f tree.ncdu -o-
    exit_code: 0
    ----- stdout -----
    [1,2,{[METADATA]},
    [{"name":"/home/user/project","asize":4096,"dsize":4096,"dev":2049},
    {"name":"raw ��","asize":2},
    {"name":"quote \" backslash \\ tab \t control \u0001 é","asize":1},
    {"name":"sparse","asize":1048576},
    [{"name":"proc","excluded":"kernfs"}],
    [{"name":"mnt","excluded":"otherfs"}],
    {"name":"node_modules","excluded":"pattern"},
    {"name":"broken","read_error":true},
    {"name":"socket","notreg":true},
    {"name":"also-linked","asize":300,"dsize":4096,"ino":104,"hlnkc":true,"nlink":2},
    [{"name":"unreadable","asize":4096,"dsize":4096,"read_error":true}],
    [{"name":"empty","asize":4096,"dsize":4096}],
    [{"name":"src","asize":4096,"dsize":4096},
    {"name":"linked","asize":300,"dsize":4096,"ino":104,"hlnkc":true,"nlink":2},
    {"name":"main.rs","asize":5000,"dsize":8192}],
    {"name":"README.md","asize":1234,"dsize":4096}]]

    ----- stderr -----
    ^[[J

    $ duit -f tree-extended.ncdu -o- -e
    exit_code: 0
    ----- stdout -----
    [1,2,{[METADATA]},
    [{"name":"/home/user/project","asize":4096,"dsize":4096,"dev":2049,"uid":1000,"gid":1000,"mode":16877,"mtime":1700000000},
    {"name":"raw ��","asize":2},
    {"name":"quote \" backslash \\ tab \t control \u0001 é","asize":1},
    {"name":"sparse","asize":1048576,"mtime":1700000800},
    [{"name":"proc","excluded":"kernfs"}],
    [{"name":"mnt","excluded":"otherfs"}],
    {"name":"node_modules","excluded":"pattern"},
    {"name":"broken","read_error":true},
    {"name":"socket","notreg":true,"uid":1000,"gid":1000,"mode":49645,"mtime":1700000700},
    {"name":"also-linked","asize":300,"dsize":4096,"ino":104,"hlnkc":true,"nlink":2,"uid":1000,"gid":1000,"mode":33188,"mtime":1700000400},
    [{"name":"unreadable","asize":4096,"dsize":4096,"uid":0,"gid":0,"mode":16832,"mtime":1700000600,"read_error":true}],
    [{"name":"empty","asize":4096,"dsize":4096,"uid":1000,"gid":1000,"mode":16877,"mtime":1700000500}],
    [{"name":"src","asize":4096,"dsize":4096,"uid":1000,"gid":1000,"mode":16877,"mtime":1700000200},
    {"name":"linked","asize":300,"dsize":4096,"ino":104,"hlnkc":true,"nlink":2,"uid":1000,"gid":1000,"mode":33188,"mtime":1700000400},
    {"name":"main.rs","asize":5000,"dsize":8192,"uid":1000,"gid":1000,"mode":33188,"mtime":1700000300}],
    {"name":"README.md","asize":1234,"dsize":4096,"uid":1000,"gid":1000,"mode":33188,"mtime":1700000100}]]

    ----- stderr -----
    ^[[J
    "#);
}

/// The number of data blocks of a binary export. The index block ends with its header, whose
/// length counts 8 bytes per data block.
fn data_blocks(root: &Path, name: &str) -> u32 {
    let export = fs::read(root.join(name)).expect("export is read");
    let header: [u8; 4] = export[export.len() - 4..].try_into().expect("4 bytes");
    ((u32::from_be_bytes(header) & 0x0fff_ffff) - 16) / 8
}

/// duit reads a binary export that ncdu wrote over several blocks, and writes it back over several
/// blocks. Two conversions restore the order of the entries, so the second reads as the JSON export
/// of the same tree.
#[test]
fn golden_blocks() {
    let directory = fixture();
    let root = directory.path();
    let arguments = [
        "-f",
        "blocks.ncdu",
        "-O",
        "twice.ncdu",
        "--export-block-size",
        "4",
    ];
    assert!(duit(root, &arguments).status.success());
    for name in ["blocks.ncdu", "twice.ncdu"] {
        let blocks = data_blocks(root, name);
        assert!(blocks >= 4, "{name}: {blocks} data blocks");
    }
    let json = render(&duit(root, &["-f", "blocks.json.zst", "-o-"]));
    assert_eq!(render(&duit(root, &["-f", "twice.ncdu", "-o-"])), json);
    #[cfg(feature = "test-ncdu")]
    {
        assert_ncdu(root, &["-f", "blocks.ncdu", "-o-"]);
        assert_ncdu(root, &["-f", "blocks.json.zst", "-o-"]);
        assert_eq!(
            render_as_duit(&ncdu(root, &["-f", "twice.ncdu", "-o-"])),
            json
        );
    }
}

/// The export that a run with `arguments` wrote, to the file of `-o` or `-O`, or else to `stdout`.
fn written(root: &Path, arguments: &[&str], stdout: &[u8]) -> Vec<u8> {
    match arguments
        .windows(2)
        .find(|pair| pair[0] == "-o" || pair[0] == "-O")
    {
        Some(pair) => fs::read(root.join(pair[1])).expect("export is written"),
        None => stdout.to_vec(),
    }
}

/// The first bytes of a zstd frame.
const ZSTD_MAGIC: &[u8] = b"\x28\xb5\x2f\xfd";

/// An export as text: a JSON export as [`text`] shows it, after a line that marks it if it is
/// compressed, and a binary export byte for byte.
fn shown(export: &[u8]) -> String {
    if export.starts_with(BINARY_SIGNATURE) {
        export.escape_ascii().to_string()
    } else if export.starts_with(ZSTD_MAGIC) {
        let export = zstd::decode_all(export).expect("export is a zstd frame");
        format!("compressed:\n{}", text(&export))
    } else {
        text(export)
    }
}

/// An export as ncdu reads it: a JSON export as [`shown`] shows it, and a binary export as the JSON
/// export that ncdu converts it to. The layout of compressed blocks may differ between versions of
/// zstd, so binary exports are compared by what they hold.
#[cfg(feature = "test-ncdu")]
fn read_by_ncdu(root: &Path, export: &[u8]) -> String {
    if !export.starts_with(BINARY_SIGNATURE) {
        return shown(export);
    }
    fs::write(root.join("read.ncdu"), export).expect("export is written");
    render(&ncdu(root, &["-f", "read.ncdu", "-o-", "-e"]))
}

/// The conversions that no other test covers write the export of a conversion that one does:
/// `-O-` writes the export of `-O FILE`, `-o FILE` that of `-o-`, `-c -o-` that of `-c -o FILE`,
/// a compressed import that of a plain one, `-c` has no effect on `-O`, and `--export-block-size`
/// none on `-o`. ncdu writes the same exports.
#[test]
fn conversions() {
    let directory = fixture();
    let root = directory.path();
    let cases: &[(&[&str], &[&str])] = &[
        (
            &["-f", "tree-extended.json", "-O-", "-e"],
            &["-f", "tree-extended.json", "-O", "reference", "-e"],
        ),
        (
            &["-f", "tree-extended.ncdu", "-O-", "-e"],
            &["-f", "tree-extended.ncdu", "-O", "reference", "-e"],
        ),
        (
            &["-f", "tree-extended.json", "-c", "-O", "export", "-e"],
            &["-f", "tree-extended.json", "-O", "reference", "-e"],
        ),
        (
            &["-f", "tree-extended.ncdu", "-o", "export", "-e"],
            &["-f", "tree-extended.ncdu", "-o-", "-e"],
        ),
        (
            &["-f", "tree-extended.ncdu", "-c", "-o-", "-e"],
            &["-f", "tree-extended.ncdu", "-c", "-o", "reference", "-e"],
        ),
        (
            &["-f", "tree-extended.json.zst", "-O", "export", "-e"],
            &["-f", "tree-extended.json", "-O", "reference", "-e"],
        ),
        (
            &["-f", "tree-extended.json.zst", "-c", "-o", "export", "-e"],
            &["-f", "tree-extended.json", "-c", "-o-", "-e"],
        ),
        (
            &["-f", "tree.json", "--export-block-size", "4", "-o-"],
            &["-f", "tree.json", "-o-"],
        ),
    ];
    for (arguments, reference) in cases {
        #[cfg(feature = "test-ncdu")]
        let ncdu_export = {
            let output = ncdu(root, arguments);
            assert!(output.status.success(), "arguments: {arguments:?}");
            written(root, arguments, &output.stdout)
        };
        let output = duit(root, arguments);
        assert!(output.status.success(), "arguments: {arguments:?}");
        assert_eq!(output.stderr, b"\x1b[J", "arguments: {arguments:?}");
        let export = written(root, arguments, &output.stdout);
        let expected = duit(root, reference);
        assert_eq!(
            shown(&export),
            shown(&written(root, reference, &expected.stdout)),
            "arguments: {arguments:?}"
        );
        #[cfg(feature = "test-ncdu")]
        assert_eq!(
            read_by_ncdu(root, &export),
            read_by_ncdu(root, &ncdu_export),
            "arguments: {arguments:?}"
        );
    }
}
