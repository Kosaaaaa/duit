use std::io::{self, Write};

use duit_format::JsonExport;
use duit_model::{EntryType, Extended, Sink, Special, Stat};
use insta::assert_snapshot;

#[cfg(feature = "test-ncdu")]
use crate::ncdu_import;

/// Run `build` against a [`JsonExport`] with timestamp 0 and return the export as bytes.
fn write_export(
    extended: bool,
    build: impl FnOnce(&mut JsonExport<Vec<u8>>) -> io::Result<()>,
) -> Vec<u8> {
    let mut export = JsonExport::new(Vec::new(), 0, extended, None).expect("header is written");
    build(&mut export).expect("items are written");
    export.finish().expect("export is flushed")
}

/// Like [`write_export`], compressed with zstd at `level`.
fn write_compressed(
    level: u8,
    build: impl FnOnce(&mut JsonExport<Vec<u8>>) -> io::Result<()>,
) -> Vec<u8> {
    let mut export = JsonExport::new(Vec::new(), 0, false, Some(level)).expect("header is written");
    build(&mut export).expect("items are written");
    export.finish().expect("export is flushed")
}

/// Like [`write_export`], with the version replaced by `[VERSION]` so that snapshots survive a
/// version bump.
///
/// With the `test-ncdu` feature, the export is also imported by ncdu and exported again. ncdu
/// must write the same items back. The snapshots, not this check, prove that duit writes the
/// fields that ncdu writes for the same entry.
fn export(
    extended: bool,
    build: impl FnOnce(&mut JsonExport<Vec<u8>>) -> io::Result<()>,
) -> String {
    let output = write_export(extended, build);
    #[cfg(feature = "test-ncdu")]
    assert_ncdu_reexport(&output, extended);
    String::from_utf8_lossy(&output).replace(env!("CARGO_PKG_VERSION"), "[VERSION]")
}

/// Assert that `ncdu -f- -o-` writes the same items as `output`. The first line holds the
/// metadata block, which differs between the two programs.
#[cfg(feature = "test-ncdu")]
fn assert_ncdu_reexport(output: &[u8], extended: bool) {
    let result = ncdu_import(output, if extended { &["-e"] } else { &[] });
    assert!(
        result.status.success(),
        "ncdu failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let items = |export: &[u8]| -> Vec<u8> {
        let start = export.iter().position(|&byte| byte == b'\n').unwrap_or(0);
        export[start..].to_vec()
    };
    assert_eq!(
        String::from_utf8_lossy(&items(&result.stdout)),
        String::from_utf8_lossy(&items(output)),
    );
    assert_eq!(items(&result.stdout), items(output));
}

pub(crate) fn file(size: u64, blocks: u64) -> Stat {
    Stat {
        size,
        blocks,
        ..Stat::default()
    }
}

pub(crate) fn directory(device: u64) -> Stat {
    Stat {
        entry_type: EntryType::Directory,
        size: 4096,
        blocks: 8,
        device,
        ..Stat::default()
    }
}

#[test]
fn tree() {
    let output = export(false, |export| {
        let mut root = export.create_root(b"/root", &directory(1))?;
        export.add_stat(&mut root, b"file", &file(100, 8))?;
        let mut nested = export.add_directory(&mut root, b"nested", &directory(1))?;
        export.add_stat(&mut nested, b"inner", &file(10, 1))?;
        export.finish_directory(nested, Some(&mut root))?;
        let empty = export.add_directory(&mut root, b"empty", &directory(1))?;
        export.finish_directory(empty, Some(&mut root))?;
        export.add_stat(&mut root, b"last", &file(1, 0))?;
        export.finish_directory(root, None)
    });
    assert_snapshot!(output, @r#"
    [1,2,{"progname":"duit","progver":"[VERSION]","timestamp":0},
    [{"name":"/root","asize":4096,"dsize":4096,"dev":1},
    {"name":"file","asize":100,"dsize":4096},
    [{"name":"nested","asize":4096,"dsize":4096},
    {"name":"inner","asize":10,"dsize":512}],
    [{"name":"empty","asize":4096,"dsize":4096}],
    {"name":"last","asize":1}]]
    "#);
}

#[test]
fn read_error() {
    let output = export(false, |export| {
        let mut root = export.create_root(b"/root", &directory(1))?;

        // Written: the error is set before the first child.
        let mut before = export.add_directory(&mut root, b"before", &directory(1))?;
        export.set_read_error(&mut before)?;
        export.set_read_error(&mut before)?;
        export.add_stat(&mut before, b"file", &file(1, 1))?;
        export.finish_directory(before, Some(&mut root))?;

        // Dropped: the error is set after the first child.
        let mut after = export.add_directory(&mut root, b"after", &directory(1))?;
        export.add_stat(&mut after, b"file", &file(1, 1))?;
        export.set_read_error(&mut after)?;
        export.finish_directory(after, Some(&mut root))?;

        let mut empty = export.add_directory(&mut root, b"empty", &directory(1))?;
        export.set_read_error(&mut empty)?;
        export.finish_directory(empty, Some(&mut root))?;

        export.finish_directory(root, None)
    });
    assert_snapshot!(output, @r#"
    [1,2,{"progname":"duit","progver":"[VERSION]","timestamp":0},
    [{"name":"/root","asize":4096,"dsize":4096,"dev":1},
    [{"name":"before","asize":4096,"dsize":4096,"read_error":true},
    {"name":"file","asize":1,"dsize":512}],
    [{"name":"after","asize":4096,"dsize":4096},
    {"name":"file","asize":1,"dsize":512}],
    [{"name":"empty","asize":4096,"dsize":4096,"read_error":true}]]]
    "#);
}

#[test]
fn special() {
    let output = export(false, |export| {
        let mut root = export.create_root(b"/root", &directory(1))?;
        export.add_special(&mut root, b"error", Special::ReadError)?;
        export.add_special(&mut root, b"pattern", Special::Pattern)?;
        export.add_special(&mut root, b"otherfs", Special::OtherFilesystem)?;
        export.add_special(&mut root, b"kernfs", Special::KernelFilesystem)?;
        export.finish_directory(root, None)
    });
    assert_snapshot!(output, @r#"
    [1,2,{"progname":"duit","progver":"[VERSION]","timestamp":0},
    [{"name":"/root","asize":4096,"dsize":4096,"dev":1},
    {"name":"error","read_error":true},
    {"name":"pattern","excluded":"pattern"},
    [{"name":"otherfs","excluded":"otherfs"}],
    [{"name":"kernfs","excluded":"kernfs"}]]]
    "#);
}

#[test]
fn name_escape() {
    let output = export(false, |export| {
        let mut root = export.create_root(b"/root", &directory(1))?;
        export.add_stat(
            &mut root,
            b"quote\" backslash\\ \n\r\t\x08\x0c \x01\x1f\x7f \xc3\xa9t\xc3\xa9",
            &file(0, 0),
        )?;
        export.finish_directory(root, None)
    });
    assert_snapshot!(output, @r#"
    [1,2,{"progname":"duit","progver":"[VERSION]","timestamp":0},
    [{"name":"/root","asize":4096,"dsize":4096,"dev":1},
    {"name":"quote\" backslash\\ \n\r\t\b\f \u0001\u001f\u007f été"}]]
    "#);
}

#[test]
fn name_invalid_utf8() {
    let output = write_export(false, |export| {
        let mut root = export.create_root(b"/root", &directory(1))?;
        export.add_stat(&mut root, b"\xff\xfe", &file(0, 0))?;
        export.finish_directory(root, None)
    });
    assert!(output.windows(2).any(|bytes| bytes == b"\xff\xfe"));
    #[cfg(feature = "test-ncdu")]
    assert_ncdu_reexport(&output, false);
}

#[test]
fn entry_type() {
    let output = export(false, |export| {
        let mut root = export.create_root(b"/root", &directory(1))?;
        let link = Stat {
            entry_type: EntryType::HardLink,
            inode: 42,
            link_count: 3,
            ..file(10, 8)
        };
        export.add_stat(&mut root, b"link", &link)?;
        let fifo = Stat {
            entry_type: EntryType::NonRegular,
            ..file(0, 0)
        };
        export.add_stat(&mut root, b"fifo", &fifo)?;
        export.finish_directory(root, None)
    });
    assert_snapshot!(output, @r#"
    [1,2,{"progname":"duit","progver":"[VERSION]","timestamp":0},
    [{"name":"/root","asize":4096,"dsize":4096,"dev":1},
    {"name":"link","asize":10,"dsize":4096,"ino":42,"hlnkc":true,"nlink":3},
    {"name":"fifo","notreg":true}]]
    "#);
}

#[test]
fn device() {
    let output = export(false, |export| {
        let mut root = export.create_root(b"/root", &directory(0))?;
        let same = export.add_directory(&mut root, b"same", &directory(0))?;
        export.finish_directory(same, Some(&mut root))?;
        let mut other = export.add_directory(&mut root, b"other", &directory(7))?;
        let nested = export.add_directory(&mut other, b"nested", &directory(7))?;
        export.finish_directory(nested, Some(&mut other))?;
        let back = export.add_directory(&mut other, b"back", &directory(0))?;
        export.finish_directory(back, Some(&mut other))?;
        export.finish_directory(other, Some(&mut root))?;
        export.finish_directory(root, None)
    });
    assert_snapshot!(output, @r#"
    [1,2,{"progname":"duit","progver":"[VERSION]","timestamp":0},
    [{"name":"/root","asize":4096,"dsize":4096},
    [{"name":"same","asize":4096,"dsize":4096}],
    [{"name":"other","asize":4096,"dsize":4096,"dev":7},
    [{"name":"nested","asize":4096,"dsize":4096}],
    [{"name":"back","asize":4096,"dsize":4096,"dev":0}]]]]
    "#);
}

#[test]
fn disk_usage_saturates() {
    let output = write_export(false, |export| {
        let mut root = export.create_root(b"/root", &directory(1))?;
        export.add_stat(&mut root, b"huge", &file(u64::MAX, u64::MAX))?;
        export.finish_directory(root, None)
    });
    let output = String::from_utf8_lossy(&output).replace(env!("CARGO_PKG_VERSION"), "[VERSION]");
    assert_snapshot!(output, @r#"
    [1,2,{"progname":"duit","progver":"[VERSION]","timestamp":0},
    [{"name":"/root","asize":4096,"dsize":4096,"dev":1},
    {"name":"huge","asize":18446744073709551615,"dsize":18446744073709551615}]]
    "#);
}

/// A root with enough files of different names and sizes to fill several 64 KiB buffers.
pub(crate) fn many_files(export: &mut JsonExport<Vec<u8>>) -> io::Result<()> {
    let mut root = export.create_root(b"/root", &directory(1))?;
    for index in 0..5000 {
        let name = format!("file{index}");
        export.add_stat(&mut root, name.as_bytes(), &file(index * 7919, index))?;
    }
    export.finish_directory(root, None)
}

/// A compressed export is one zstd frame that decompresses to the export without compression.
#[test]
fn compressed() {
    let compressed = write_compressed(4, many_files);
    let output = zstd::decode_all(compressed.as_slice()).expect("export is a zstd frame");
    assert_eq!(output, write_export(false, many_files));
}

/// A higher compression level writes a smaller export.
#[test]
fn compress_level() {
    let fastest = write_compressed(1, many_files);
    let smallest = write_compressed(19, many_files);
    assert!(
        smallest.len() < fastest.len(),
        "level 19 wrote {} bytes, level 1 wrote {}",
        smallest.len(),
        fastest.len()
    );
}

fn extended_file() -> Stat {
    Stat {
        extended: Extended {
            mtime: Some(1_700_000_000),
            uid: None,
            gid: Some(20),
            mode: Some(0o100_644),
        },
        ..file(1, 1)
    }
}

#[test]
fn extended_disabled() {
    let output = export(false, |export| {
        let mut root = export.create_root(b"/root", &directory(1))?;
        export.add_stat(&mut root, b"file", &extended_file())?;
        export.finish_directory(root, None)
    });
    assert_snapshot!(output, @r#"
    [1,2,{"progname":"duit","progver":"[VERSION]","timestamp":0},
    [{"name":"/root","asize":4096,"dsize":4096,"dev":1},
    {"name":"file","asize":1,"dsize":512}]]
    "#);
}

#[test]
fn extended_enabled() {
    let output = export(true, |export| {
        let root_stat = Stat {
            extended: Extended {
                mtime: Some(1),
                uid: Some(501),
                gid: Some(20),
                mode: Some(0o40_755),
            },
            ..directory(1)
        };
        let mut root = export.create_root(b"/root", &root_stat)?;
        export.add_stat(&mut root, b"file", &extended_file())?;
        export.add_stat(&mut root, b"bare", &file(1, 1))?;
        export.finish_directory(root, None)
    });
    assert_snapshot!(output, @r#"
    [1,2,{"progname":"duit","progver":"[VERSION]","timestamp":0},
    [{"name":"/root","asize":4096,"dsize":4096,"dev":1,"uid":501,"gid":20,"mode":16877,"mtime":1},
    {"name":"file","asize":1,"dsize":512,"gid":20,"mode":33188,"mtime":1700000000},
    {"name":"bare","asize":1,"dsize":512}]]
    "#);
}

/// A writer whose every write fails.
struct BrokenPipe;

impl Write for BrokenPipe {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn write_error() {
    let mut export = JsonExport::new(BrokenPipe, 0, false, None).expect("header is buffered");
    let root = export
        .create_root(b"/root", &directory(1))
        .expect("root is buffered");
    export
        .finish_directory(root, None)
        .expect("root is buffered");
    let error = export.finish().err().expect("the final flush fails");
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
}

/// A writer that accepts every write and fails to flush, as a buffered writer does when the
/// buffered bytes cannot be written.
struct FailingFlush;

impl Write for FailingFlush {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::ErrorKind::BrokenPipe.into())
    }
}

#[test]
fn flush_error() {
    for compress_level in [None, Some(4)] {
        let mut export =
            JsonExport::new(FailingFlush, 0, false, compress_level).expect("header is buffered");
        let root = export
            .create_root(b"/root", &directory(1))
            .expect("root is buffered");
        export
            .finish_directory(root, None)
            .expect("root is buffered");
        let error = export.finish().err().expect("the final flush fails");
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }
}
