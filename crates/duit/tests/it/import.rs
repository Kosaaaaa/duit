use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;

use insta::assert_snapshot;
use tempfile::TempDir;

use crate::{
    BINARY_SIGNATURE, BLOCK_HEADER_LENGTH, binary_file, binary_frames, block_start, data_blocks,
    duit, index_block, item, reference, render, table, text,
};
#[cfg(feature = "test-ncdu")]
use crate::{assert_ncdu, ncdu, render_as_duit};

const EXPORT: &[u8] = b"[1,2,{\"progname\":\"ncdu\",\"progver\":\"2.9.2\",\"timestamp\":1},
[{\"name\":\"/root\",\"asize\":4096,\"dsize\":4096,\"dev\":1,\"uid\":501,\"gid\":20,\"mode\":16877,\"mtime\":1},
{\"name\":\"file\",\"asize\":100,\"dsize\":4096,\"uid\":501,\"gid\":20,\"mode\":33188,\"mtime\":1700000000},
[{\"name\":\"sub\",\"asize\":4096,\"dsize\":4096,\"read_error\":true},
{\"name\":\"link\",\"asize\":10,\"dsize\":4096,\"ino\":42,\"hlnkc\":true,\"nlink\":2},
{\"name\":\"fifo\",\"notreg\":true}],
{\"name\":\"pattern\",\"excluded\":\"pattern\"},
{\"name\":\"error\",\"read_error\":true},
{\"name\":\"raw \\u00e9 \xff\xfe\"}]]
";

const JSON_EXPORT_MAX_NAME_LENGTH: usize = (64 * 1024 - 1000) / 6;

fn fixture() -> TempDir {
    let directory = TempDir::new().expect("temporary directory is created");
    let write = |name: &str, contents: &[u8]| {
        fs::write(directory.path().join(name), contents).expect("file is written");
    };
    write("export.json", EXPORT);
    write(
        "export.json.zst",
        &zstd::encode_all(EXPORT, 3).expect("export is compressed"),
    );
    write("short.json", b"[1,0,{}");
    write("malformed.json", b"[1,0,{},[{\"name\":\"a\",}]]");
    write(
        "long.json",
        &[
            &b"[1,2,{},[{\"name\":\"/root\"},{\"name\":\""[..],
            &[b'a'; JSON_EXPORT_MAX_NAME_LENGTH + 1],
            b"\"}]]",
        ]
        .concat(),
    );
    write(
        "longest.json",
        &[
            &b"[1,2,{},[{\"name\":\"/root\"},{\"name\":\""[..],
            &[b'a'; 32 * 1024 - 5],
            b"\"}]]",
        ]
        .concat(),
    );
    fs::create_dir(directory.path().join("directory")).expect("directory is created");
    directory
}

fn with_input(program: &OsStr, directory: &Path, arguments: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new(program)
        .current_dir(directory)
        .arg("--ignore-config")
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("program runs");
    let mut stdin = child.stdin.take().expect("stdin is piped");
    // A thread writes the input, so that the program never waits on a full output pipe.
    thread::scope(|scope| {
        scope.spawn(move || {
            let _ = stdin.write_all(input);
        });
        child.wait_with_output().expect("program exits")
    })
}

#[test]
fn import() {
    let directory = fixture();
    let cases: &[&[&str]] = &[
        &["-f", "export.json", "-o-"],
        &["-f", "export.json", "-o-", "-e"],
    ];
    #[cfg(feature = "test-ncdu")]
    for arguments in cases {
        assert_ncdu(directory.path(), arguments);
    }
    assert_snapshot!(table(directory.path(), cases), @r#"
    $ duit -f export.json -o-
    exit_code: 0
    ----- stdout -----
    [1,2,{[METADATA]},
    [{"name":"/root","asize":4096,"dsize":4096,"dev":1},
    {"name":"file","asize":100,"dsize":4096},
    [{"name":"sub","asize":4096,"dsize":4096,"read_error":true},
    {"name":"link","asize":10,"dsize":4096,"ino":42,"hlnkc":true,"nlink":2},
    {"name":"fifo","notreg":true}],
    {"name":"pattern","excluded":"pattern"},
    {"name":"error","read_error":true},
    {"name":"raw é ��"}]]

    ----- stderr -----
    ^[[J

    $ duit -f export.json -o- -e
    exit_code: 0
    ----- stdout -----
    [1,2,{[METADATA]},
    [{"name":"/root","asize":4096,"dsize":4096,"dev":1,"uid":501,"gid":20,"mode":16877,"mtime":1},
    {"name":"file","asize":100,"dsize":4096,"uid":501,"gid":20,"mode":33188,"mtime":1700000000},
    [{"name":"sub","asize":4096,"dsize":4096,"read_error":true},
    {"name":"link","asize":10,"dsize":4096,"ino":42,"hlnkc":true,"nlink":2},
    {"name":"fifo","notreg":true}],
    {"name":"pattern","excluded":"pattern"},
    {"name":"error","read_error":true},
    {"name":"raw é ��"}]]

    ----- stderr -----
    ^[[J
    "#);

    let plain = render(&duit(directory.path(), &["-f", "export.json", "-o-"]));
    let compressed = render(&duit(directory.path(), &["-f", "export.json.zst", "-o-"]));
    assert_eq!(compressed, plain);
    let executable = OsStr::new(env!("CARGO_BIN_EXE_duit"));
    let stdin = render(&with_input(
        executable,
        directory.path(),
        &["-f", "-", "-o-"],
        EXPORT,
    ));
    assert_eq!(stdin, plain);
    let compressed_export = zstd::encode_all(EXPORT, 3).expect("export is compressed");
    let compressed_stdin = render(&with_input(
        executable,
        directory.path(),
        &["-f", "-", "-o-"],
        &compressed_export,
    ));
    assert_eq!(compressed_stdin, plain);
    #[cfg(feature = "test-ncdu")]
    {
        assert_ncdu(directory.path(), &["-f", "export.json.zst", "-o-"]);
        for input in [EXPORT, &compressed_export] {
            let ncdu = with_input(OsStr::new("ncdu"), directory.path(), &["-f-", "-o-"], input);
            assert_eq!(render_as_duit(&ncdu), stdin);
        }
    }
}

#[test]
fn round_trip() {
    let directory = fixture();
    let root = directory.path();
    fs::create_dir_all(root.join("tree/sub")).expect("directory is created");
    fs::write(root.join("tree/file"), b"hello").expect("file is written");
    fs::write(root.join("tree/sub/inner"), b"x").expect("file is written");
    let scan = duit(root, &["-o-", "tree"]);
    assert!(scan.status.success());
    for arguments in [&["-o", "scan.json"][..], &["-c", "-o", "scan.json"]] {
        assert!(
            duit(root, &[arguments, &["tree"]].concat())
                .status
                .success()
        );
        let import = duit(root, &["-f", "scan.json", "-o-"]);
        assert!(import.status.success());
        assert_eq!(text(&import.stdout), text(&scan.stdout));
    }
}

#[test]
fn import_compress() {
    let directory = fixture();
    let output = duit(
        directory.path(),
        &["-f", "export.json", "-c", "-o", "export.zst"],
    );
    assert!(output.status.success());
    let compressed = fs::read(directory.path().join("export.zst")).expect("export is written");
    let export = zstd::decode_all(compressed.as_slice()).expect("export is a zstd frame");
    let plain = duit(directory.path(), &["-f", "export.json", "-o-"]);
    assert_eq!(text(&export), text(&plain.stdout));
}

#[test]
fn scan_options() {
    let directory = fixture();
    let arguments = [
        "-f",
        "export.json",
        "-o-",
        "-t",
        "2",
        "-x",
        "-L",
        "--exclude",
        "file",
        "--exclude-caches",
        "missing",
    ];
    #[cfg(feature = "test-ncdu")]
    assert_ncdu(directory.path(), &arguments);
    let output = duit(directory.path(), &arguments);
    assert!(output.status.success());
    assert_eq!(
        render(&output),
        render(&duit(directory.path(), &["-f", "export.json", "-o-"]))
    );
}

#[cfg(feature = "test-ncdu")]
#[test]
fn ncdu_export() {
    let directory = fixture();
    let root = directory.path();
    fs::create_dir(root.join("tree")).expect("directory is created");
    fs::write(root.join("tree/file"), b"hello").expect("file is written");
    fs::create_dir(root.join("tree/sub")).expect("directory is created");
    fs::write(root.join("tree/sub/inner"), b"x").expect("file is written");
    for arguments in [&["-o", "ncdu.json"][..], &["-c", "-o", "ncdu.json"]] {
        assert!(
            ncdu(root, &[arguments, &["-e", "tree"]].concat())
                .status
                .success()
        );
        assert_ncdu(root, &["-f", "ncdu.json", "-o-", "-e"]);
    }
}

#[test]
fn errors() {
    let directory = fixture();
    let cases: &[&[&str]] = &[
        &["-f", "missing", "-o-"],
        &["-f", "directory", "-o-"],
        &["-f", "short.json", "-o-"],
        &["-f", "-", "-o-"],
        &["-f", "malformed.json", "-o-"],
        &["-f", "malformed.json", "-c", "-o-"],
        &["-f", "malformed.json", "-o", "export.out"],
        &["-f", "long.json", "-o-"],
        &["-f", "missing", "-O", "missing.ncdu"],
        &["-f", "malformed.json", "-O", "malformed.ncdu"],
        &[
            "-f",
            "long.json",
            "--export-block-size",
            "4",
            "-O",
            "long.ncdu",
        ],
    ];
    #[cfg(feature = "test-ncdu")]
    for arguments in cases {
        assert_ncdu(directory.path(), arguments);
    }
    assert_snapshot!(table(directory.path(), cases), @"
    $ duit -f missing -o-
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[JError reading file 'missing': No such file or directory.

    $ duit -f directory -o-
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[JError reading file 'directory': Is a directory.

    $ duit -f short.json -o-
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[JError reading file 'short.json': Unexpected end of file.

    $ duit -f - -o-
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[JError reading file '-': Unexpected end of file.

    $ duit -f malformed.json -o-
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[JError importing file on line 1:23: expected string.

    $ duit -f malformed.json -c -o-
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[JError importing file on line 1:23: expected string.

    $ duit -f malformed.json -o export.out
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[JError importing file on line 1:23: expected string.

    $ duit -f long.json -o-
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[JError writing JSON export: path too long.

    $ duit -f missing -O missing.ncdu
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[JError reading file 'missing': No such file or directory.

    $ duit -f malformed.json -O malformed.ncdu
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[JError importing file on line 1:23: expected string.

    $ duit -f long.json --export-block-size 4 -O long.ncdu
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[JError writing data: path too long.
    ");
    assert_eq!(
        fs::read(directory.path().join("export.out")).expect("export file is created"),
        b""
    );
    for name in ["missing.ncdu", "malformed.ncdu", "long.ncdu"] {
        assert_eq!(
            fs::read(directory.path().join(name)).expect("export file is created"),
            b"\xbfncduEX1",
            "{name}"
        );
    }
}

#[test]
fn binary_export() {
    let directory = fixture();
    let root = directory.path();
    let cases: &[&[&str]] = &[
        &["-f", "export.json", "-O", "duit.ncdu"],
        &["-f", "export.json", "-O", "duit.ncdu", "-e"],
        &["-f", "longest.json", "-O", "duit.ncdu"],
    ];
    let mut rendered = String::new();
    for arguments in cases {
        rendered.push_str(&table(root, &[arguments]));
        #[cfg(feature = "test-ncdu")]
        {
            let ncdu_arguments = [&arguments[..3], &["ncdu.ncdu"], &arguments[4..]].concat();
            assert!(ncdu(root, &ncdu_arguments).status.success());
            assert_eq!(
                render(&ncdu(root, &["-f", "duit.ncdu", "-o-", "-e"])),
                render(&ncdu(root, &["-f", "ncdu.ncdu", "-o-", "-e"])),
                "arguments: {arguments:?}"
            );
        }
    }
    assert_snapshot!(rendered, @"
    $ duit -f export.json -O duit.ncdu
    exit_code: 0
    ----- stdout -----

    ----- stderr -----
    ^[[J

    $ duit -f export.json -O duit.ncdu -e
    exit_code: 0
    ----- stdout -----

    ----- stderr -----
    ^[[J

    $ duit -f longest.json -O duit.ncdu
    exit_code: 0
    ----- stdout -----

    ----- stderr -----
    ^[[J
    ");
}

#[test]
fn binary() {
    let directory = fixture();
    let root = directory.path();
    assert!(
        duit(root, &["-f", "export.json", "-O", "duit.ncdu", "-e"])
            .status
            .success()
    );
    let cases: &[&[&str]] = &[
        &["-f", "duit.ncdu", "-o-"],
        &["-f", "duit.ncdu", "-o-", "-e"],
    ];
    #[cfg(feature = "test-ncdu")]
    {
        for arguments in cases {
            assert_ncdu(root, arguments);
        }
        assert!(
            ncdu(root, &["-f", "export.json", "-O", "ncdu.ncdu", "-e"])
                .status
                .success()
        );
        assert_ncdu(root, &["-f", "ncdu.ncdu", "-o-", "-e"]);
    }
    assert_snapshot!(table(root, cases), @r#"
    $ duit -f duit.ncdu -o-
    exit_code: 0
    ----- stdout -----
    [1,2,{[METADATA]},
    [{"name":"/root","asize":4096,"dsize":4096,"dev":1},
    {"name":"raw é ��"},
    {"name":"error","read_error":true},
    {"name":"pattern","excluded":"pattern"},
    [{"name":"sub","asize":4096,"dsize":4096,"read_error":true},
    {"name":"fifo","notreg":true},
    {"name":"link","asize":10,"dsize":4096,"ino":42,"hlnkc":true,"nlink":2}],
    {"name":"file","asize":100,"dsize":4096}]]

    ----- stderr -----
    ^[[J

    $ duit -f duit.ncdu -o- -e
    exit_code: 0
    ----- stdout -----
    [1,2,{[METADATA]},
    [{"name":"/root","asize":4096,"dsize":4096,"dev":1,"uid":501,"gid":20,"mode":16877,"mtime":1},
    {"name":"raw é ��"},
    {"name":"error","read_error":true},
    {"name":"pattern","excluded":"pattern"},
    [{"name":"sub","asize":4096,"dsize":4096,"read_error":true},
    {"name":"fifo","notreg":true},
    {"name":"link","asize":10,"dsize":4096,"ino":42,"hlnkc":true,"nlink":2}],
    {"name":"file","asize":100,"dsize":4096,"uid":501,"gid":20,"mode":33188,"mtime":1700000000}]]

    ----- stderr -----
    ^[[J
    "#);

    let original = render(&duit(root, &["-f", "export.json", "-o-", "-e"]));
    for arguments in [
        &["-f", "duit.ncdu", "-O", "copy.ncdu", "-e"][..],
        &[
            "-f",
            "duit.ncdu",
            "-O",
            "copy.ncdu",
            "-e",
            "--compress-level",
            "19",
        ],
        &[
            "-f",
            "duit.ncdu",
            "-O",
            "copy.ncdu",
            "-e",
            "--export-block-size",
            "4",
        ],
    ] {
        assert!(duit(root, arguments).status.success(), "{arguments:?}");
        assert_eq!(
            render(&duit(root, &["-f", "copy.ncdu", "-o-", "-e"])),
            original,
            "{arguments:?}"
        );
        #[cfg(feature = "test-ncdu")]
        {
            let ncdu_arguments = [&arguments[..3], &["ncdu-copy.ncdu"], &arguments[4..]].concat();
            assert!(ncdu(root, &ncdu_arguments).status.success());
            assert_eq!(
                render(&ncdu(root, &["-f", "copy.ncdu", "-o-", "-e"])),
                render(&ncdu(root, &["-f", "ncdu-copy.ncdu", "-o-", "-e"])),
                "arguments: {arguments:?}"
            );
        }
    }

    let output = duit(root, &["-f", "duit.ncdu", "-c", "-o", "export.zst"]);
    assert!(output.status.success());
    let compressed = fs::read(root.join("export.zst")).expect("export is written");
    let export = zstd::decode_all(compressed.as_slice()).expect("export is a zstd frame");
    let plain = duit(root, &["-f", "duit.ncdu", "-o-"]);
    assert_eq!(text(&export), text(&plain.stdout));
}

#[test]
fn binary_stdin() {
    let directory = fixture();
    let root = directory.path();
    assert!(
        duit(root, &["-f", "export.json", "-O", "duit.ncdu"])
            .status
            .success()
    );
    let export = fs::read(root.join("duit.ncdu")).expect("export is written");
    let run = |program: &OsStr| {
        let file = fs::File::open(root.join("duit.ncdu")).expect("export is opened");
        Command::new(program)
            .current_dir(root)
            .args(["--ignore-config", "-f", "-", "-o-"])
            .stdin(file)
            .output()
            .expect("program runs")
    };
    let executable = OsStr::new(env!("CARGO_BIN_EXE_duit"));
    assert_eq!(
        render(&run(executable)),
        render(&duit(root, &["-f", "duit.ncdu", "-o-"]))
    );
    let pipe = render(&with_input(executable, root, &["-f", "-", "-o-"], &export));
    #[cfg(feature = "test-ncdu")]
    {
        assert_eq!(
            render_as_duit(&run(OsStr::new("ncdu"))),
            render(&run(executable))
        );
        let ncdu = with_input(OsStr::new("ncdu"), root, &["-f", "-", "-o-"], &export);
        assert_eq!(render_as_duit(&ncdu), pipe);
    }
    assert_snapshot!(pipe, @"
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[JError reading file '-': Unseekable.
    ");
}

#[test]
fn binary_blocks() {
    let directory = fixture();
    let root = directory.path();
    for index in 0..300 {
        let subdirectory = root.join(format!("tree/directory-{}", index % 7));
        fs::create_dir_all(&subdirectory).expect("directory is created");
        fs::write(
            subdirectory.join(format!("file-with-a-long-name-{index}")),
            b"x",
        )
        .expect("file is written");
    }
    let scan = duit(root, &["-o-", "-e", "tree"]);
    assert!(scan.status.success());
    let block_size = ["--export-block-size", "4"];
    for arguments in [
        &["-O", "first.ncdu", "-e", "tree"][..],
        &["-f", "first.ncdu", "-O", "second.ncdu", "-e"],
    ] {
        assert!(
            duit(root, &[arguments, &block_size].concat())
                .status
                .success()
        );
    }
    let first = fs::read(root.join("first.ncdu")).expect("export is written");
    let blocks = data_blocks(&first);
    assert!(blocks >= 4, "{blocks} data blocks");
    let second = duit(root, &["-f", "second.ncdu", "-o-", "-e"]);
    assert_eq!(render(&second), render(&scan));
    #[cfg(feature = "test-ncdu")]
    {
        assert_ncdu(root, &["-f", "first.ncdu", "-o-", "-e"]);
        assert!(
            ncdu(
                root,
                &[
                    &["-O", "threads.ncdu", "-e", "-t", "4", "tree"][..],
                    &block_size
                ]
                .concat()
            )
            .status
            .success()
        );
        assert_ncdu(root, &["-f", "threads.ncdu", "-o-", "-e"]);
        assert!(
            duit(
                root,
                &[
                    &["-O", "duit-threads.ncdu", "-e", "-t", "4", "tree"][..],
                    &block_size
                ]
                .concat()
            )
            .status
            .success()
        );
        assert_ncdu(root, &["-f", "duit-threads.ncdu", "-o-", "-e"]);
        assert!(
            ncdu(
                root,
                &[
                    &["-f", "first.ncdu", "-O", "ncdu.ncdu", "-e"][..],
                    &block_size
                ]
                .concat()
            )
            .status
            .success()
        );
        assert_eq!(
            render(&ncdu(root, &["-f", "second.ncdu", "-o-", "-e"])),
            render(&ncdu(root, &["-f", "ncdu.ncdu", "-o-", "-e"]))
        );
    }
}

fn directory_item(fields: &[u8]) -> Vec<u8> {
    item(&[b"\x00\x00\x01\x41r", fields].concat())
}

const FILE: &[u8] = b"\xbf\x00\x01\x01\x41f\xff";

#[test]
fn binary_items() {
    let directory = fixture();
    let root = directory.path();
    let prev = |number| reference(0x02, number);
    let blocks: Vec<Vec<u8>> = vec![
        item(b"\x00\x01\x01\x64text\x03\x19\x01\x00\x04\x19\x03\xff"),
        item(&[&b"\x00\x05\x01\x47unknown"[..], &prev(0)].concat()),
        item(&[&b"\x00\x26\x01\x48negative"[..], &prev(1)].concat()),
        item(&[&b"\x00\x20\x01\x45error"[..], &prev(2)].concat()),
        item(&[&b"\x00\x22\x01\x47otherfs"[..], &prev(3)].concat()),
        item(&[&b"\x00\x23\x01\x46kernfs"[..], &prev(4)].concat()),
        [&b"\xa3\x00\x01\x01\x48definite"[..], &prev(5)].concat(),
        item(
            &[
                &b"\x00\xc0\x01\x01\xd8\x20\x44tags\x18\x1f\x82\x01\xa1\x00\x00\x61x\x01\x20\x9f\xff\xf9\x00\x00\x01"[..],
                &prev(6),
            ]
            .concat(),
        ),
        item(&[&b"\x00\x00\x01\x41a\x00\x01\x01\x48override"[..], &prev(7)].concat()),
        item(
            &[
                &b"\x00\x03\x01\x44link\x03\x0a\x04\x1a\x00\x00\x10\x00\x0d\x18\x2a\x0e\x02\x0f\x19\x01\xf5\x10\x14\x11\x19\x81\xa4\x12\x1a\x65\x53\xf1\x00"[..],
                &prev(8),
            ]
            .concat(),
        ),
        item(&[&b"\x00\x00\x01\x47errored\x06\xf5"[..], &prev(9)].concat()),
        item(b"\x00\x01\x01\x46nested"),
        item(
            &[
                &b"\x00\x00\x01\x45other\x06\xf4"[..],
                &reference(0x0c, 11),
                &prev(10),
            ]
            .concat(),
        ),
        item(
            &[
                &b"\x00\x00\x01\x45/root\x05\x01\x0f\x00\x18\x13\x01"[..],
                &reference(0x0c, 12),
            ]
            .concat(),
        ),
    ];
    let blocks: Vec<&[u8]> = blocks.iter().map(Vec::as_slice).collect();
    fs::write(
        root.join("items.ncdu"),
        binary_file(&blocks, block_start(13)),
    )
    .expect("export is written");
    let cases: &[&[&str]] = &[&["-f", "items.ncdu", "-o-", "-e"]];
    #[cfg(feature = "test-ncdu")]
    for arguments in cases {
        assert_ncdu(root, arguments);
    }
    assert_snapshot!(table(root, cases), @r#"
    $ duit -f items.ncdu -o- -e
    exit_code: 0
    ----- stdout -----
    [1,2,{[METADATA]},
    [{"name":"/root","dev":1,"uid":0},
    [{"name":"other"},
    {"name":"nested"}],
    [{"name":"errored","read_error":true}],
    {"name":"link","asize":10,"dsize":4096,"ino":42,"hlnkc":true,"nlink":2,"uid":501,"gid":20,"mode":33188,"mtime":1700000000},
    {"name":"override"},
    {"name":"tags"},
    {"name":"definite"},
    [{"name":"kernfs","excluded":"kernfs"}],
    [{"name":"otherfs","excluded":"otherfs"}],
    {"name":"error","read_error":true},
    {"name":"negative","excluded":"pattern"},
    {"name":"unknown","notreg":true},
    {"name":"text","asize":256,"dsize":512}]]

    ----- stderr -----
    ^[[J
    "#);
}

enum Ncdu {
    Same,
    Crashes,
    Loops,
    ReadsOtherwise,
}

#[test]
fn binary_malformed() {
    let directory = fixture();
    let root = directory.path();
    let signature: &[u8] = b"\xbfncduEX1";
    let file_then_root = [FILE, &directory_item(b"\x0c\x26")].concat();
    let frame = zstd::bulk::compress(FILE, 3).expect("block is compressed");
    let mut deep_directories = b"\xbf\x00\x00\x01\x41d\x06\xf4\xff".to_vec();
    for _ in 0..1_000_000 {
        deep_directories.extend_from_slice(b"\xbf\x00\x00\x01\x41d\x0c\x28\xff");
    }
    let deep_root = u64::try_from(deep_directories.len() - 9).expect("offset fits");
    let mut renumbered = binary_file(&[&directory_item(b"")], 0);
    let block_number = BINARY_SIGNATURE.len() + BLOCK_HEADER_LENGTH;
    let renumber = 1_u32.to_be_bytes();
    renumbered[block_number..block_number + renumber.len()].copy_from_slice(&renumber);
    let deep_value = directory_item(
        &[
            &b"\x18\x1f"[..],
            &b"\x81\x9f".repeat(1_000_000),
            &b"\xff".repeat(1_000_000),
        ]
        .concat(),
    );
    let rows: Vec<(&str, Vec<u8>, Ncdu)> = vec![
        (
            "an export of one file",
            binary_file(&[&file_then_root], 7),
            Ncdu::Same,
        ),
        ("the signature only", signature.to_vec(), Ncdu::Same),
        ("15 bytes", [signature, &[0; 7]].concat(), Ncdu::Same),
        (
            "an index header of a data block",
            [signature, &[0, 0, 0, 0x10], &[0; 8], &[0, 0, 0, 0x10]].concat(),
            Ncdu::Same,
        ),
        (
            "an index length that is not a multiple of 8",
            [signature, &[0; 8], b"\x10\0\0\x11"].concat(),
            Ncdu::Same,
        ),
        (
            "an index length below 8",
            [signature, &[0; 8], b"\x10\0\0\0"].concat(),
            Ncdu::Same,
        ),
        (
            "an index longer than the file",
            [signature, &[0; 8], b"\x10\0\x01\0"].concat(),
            Ncdu::Same,
        ),
        (
            "an index that starts before the file",
            [signature, &[0; 5], b"\x10\0\0\x18"].concat(),
            Ncdu::Same,
        ),
        (
            "an index without a root reference",
            [signature, &[0; 8], b"\x10\0\0\x08"].concat(),
            Ncdu::Crashes,
        ),
        (
            "an index without data blocks",
            [signature, &index_block(&[0; 8])].concat(),
            Ncdu::Same,
        ),
        (
            "a root past the index",
            binary_file(&[FILE], block_start(5)),
            Ncdu::Same,
        ),
        (
            "a reference of 57 bits",
            binary_file(&[FILE], 1 << 56),
            Ncdu::Same,
        ),
        (
            "a block of 12 bytes",
            [
                signature,
                &index_block(&[0, 0, 0, 0, 0x08, 0, 0, 12, 0, 0, 0, 0, 0, 0, 0, 0]),
            ]
            .concat(),
            Ncdu::Same,
        ),
        (
            "a block past the end of the file",
            [
                signature,
                &index_block(&[0, 0, 0, 0, 0x08, 0, 0, 100, 0, 0, 0, 0, 0, 0, 0, 0]),
            ]
            .concat(),
            Ncdu::Same,
        ),
        (
            "a block that is not a zstd frame",
            binary_frames(&[b"not zstd"], 0),
            Ncdu::Same,
        ),
        (
            "a zstd frame without its size",
            binary_frames(
                &[&zstd::encode_all(file_then_root.as_slice(), 3).expect("block is compressed")],
                7,
            ),
            Ncdu::Same,
        ),
        (
            "an empty zstd frame",
            binary_frames(
                &[&zstd::bulk::compress(b"", 3).expect("block is compressed")],
                0,
            ),
            Ncdu::Same,
        ),
        (
            "a truncated zstd frame",
            binary_frames(&[&frame[..frame.len() - 1]], 0),
            Ncdu::Same,
        ),
        (
            "a block whose header holds another number",
            renumbered,
            Ncdu::ReadsOtherwise,
        ),
        (
            "an item past the end of its block",
            binary_file(&[FILE], 7),
            Ncdu::Same,
        ),
        (
            "an item that is not a map",
            binary_file(&[b"\x80"], 0),
            Ncdu::Same,
        ),
        (
            "an item without a type",
            binary_file(&[&item(b"\x01\x41r")], 0),
            Ncdu::Same,
        ),
        (
            "an item without a name",
            binary_file(&[&item(b"\x00\x00")], 0),
            Ncdu::Same,
        ),
        (
            "an empty name",
            binary_file(&[&item(b"\x00\x00\x01\x40")], 0),
            Ncdu::Same,
        ),
        (
            "a name of indefinite length",
            binary_file(&[&item(b"\x00\x00\x01\x5f\x41r\xff")], 0),
            Ncdu::Same,
        ),
        (
            "a root that is not a directory",
            binary_file(&[FILE], 0),
            Ncdu::Same,
        ),
        (
            "an item without its end",
            binary_file(&[b"\xbf\x00\x00\x01\x41r"], 0),
            Ncdu::Same,
        ),
        (
            "a reserved additional information",
            binary_file(&[&directory_item(b"\x1c")], 0),
            Ncdu::Same,
        ),
        (
            "a tag of indefinite length",
            binary_file(&[&directory_item(b"\x18\x1f\xdf")], 0),
            Ncdu::Same,
        ),
        (
            "a type out of range",
            binary_file(&[&item(b"\x00\x1a\x80\0\0\0\x01\x41r")], 0),
            Ncdu::Same,
        ),
        (
            "a negative size",
            binary_file(&[&directory_item(b"\x03\x20")], 0),
            Ncdu::Same,
        ),
        (
            "a link count of 32 bits",
            binary_file(&[&directory_item(b"\x0e\x1a\x80\0\0\0")], 0),
            Ncdu::Same,
        ),
        (
            "a negative reference before its block",
            binary_file(&[&directory_item(b"\x0c\x20")], 0),
            Ncdu::Same,
        ),
        (
            "a reference that is a string",
            binary_file(&[&directory_item(b"\x0c\x40")], 0),
            Ncdu::Same,
        ),
        (
            "a reference past the index",
            binary_file(&[&directory_item(&reference(0x0c, 2))], 0),
            Ncdu::Same,
        ),
        (
            "an entry that refers to itself",
            binary_file(
                &[
                    &item(&[&b"\x00\x01\x01\x41f"[..], &reference(0x02, 0)].concat()),
                    &directory_item(&reference(0x0c, 0)),
                ],
                block_start(1),
            ),
            Ncdu::Loops,
        ),
        (
            "a directory that contains itself",
            binary_file(&[&directory_item(&reference(0x0c, 0))], 0),
            Ncdu::Crashes,
        ),
        (
            "two directories that share an entry",
            binary_file(
                &[
                    FILE,
                    &directory_item(&reference(0x0c, 0)),
                    &directory_item(&[reference(0x0c, 0), reference(0x02, 1)].concat()),
                    &directory_item(&reference(0x0c, 2)),
                ],
                block_start(3),
            ),
            Ncdu::ReadsOtherwise,
        ),
        (
            "directories nested a million deep",
            binary_file(&[&deep_directories], deep_root),
            Ncdu::Crashes,
        ),
        (
            "a value nested a million deep",
            binary_file(&[&deep_value], 0),
            Ncdu::Crashes,
        ),
    ];
    let mut rendered = String::new();
    for (index, (description, contents, ncdu_outcome)) in rows.iter().enumerate() {
        let name = format!("{index}.ncdu");
        fs::write(root.join(&name), contents).expect("export is written");
        let arguments = ["-f", name.as_str(), "-o-"];
        let unlike_ncdu = match ncdu_outcome {
            Ncdu::Same => "",
            Ncdu::Crashes | Ncdu::Loops | Ncdu::ReadsOtherwise => " (unlike ncdu)",
        };
        #[cfg(feature = "test-ncdu")]
        match ncdu_outcome {
            Ncdu::Same => assert_ncdu(root, &arguments),
            Ncdu::Crashes | Ncdu::Loops | Ncdu::ReadsOtherwise => {}
        }
        let output = duit(root, &arguments);
        let stderr = text(&output.stderr);
        let _ = writeln!(
            rendered,
            "{description}{unlike_ncdu}\n  {}: {}",
            output.status.code().unwrap_or(-1),
            stderr.trim_start_matches("^[[J").trim_end()
        );
    }
    assert_snapshot!(rendered, @"
    an export of one file
      0: 
    the signature only
      1: Error reading file '1.ncdu': Unexpected end of file.
    15 bytes
      1: Error reading file '2.ncdu': Unexpected end of file.
    an index header of a data block
      1: Error reading from file
    an index length that is not a multiple of 8
      1: Error reading from file
    an index length below 8
      1: Error reading from file
    an index longer than the file
      1: Error reading from file
    an index that starts before the file
      1: Error reading file '7.ncdu': Unexpected.
    an index without a root reference (unlike ncdu)
      1: Error reading from file
    an index without data blocks
      1: Error reading item 0 from file
    a root past the index
      1: Error reading item 5000000 from file
    a reference of 57 bits
      1: Error reading item 100000000000000 from file
    a block of 12 bytes
      1: Error reading item 0 from file
    a block past the end of the file
      1: Error reading item 0 from file
    a block that is not a zstd frame
      1: Error reading item 0 from file
    a zstd frame without its size
      1: Error reading item 7 from file
    an empty zstd frame
      1: Error reading item 0 from file
    a truncated zstd frame
      1: Error decompressing block 0 (expected 7 got 18446744073709551544)
    a block whose header holds another number (unlike ncdu)
      1: Error reading item 0 from file
    an item past the end of its block
      1: Error reading item 7 from file
    an item that is not a map
      1: Error reading item 0 from file
    an item without a type
      1: Error reading item 0 from file
    an item without a name
      1: Error reading item 0 from file
    an empty name
      1: Error reading item 0 from file
    a name of indefinite length
      1: Error reading item 0 from file
    a root that is not a directory
      1: Error reading item 0 from file
    an item without its end
      1: Error reading item 0 from file
    a reserved additional information
      1: Error reading item 0 from file
    a tag of indefinite length
      1: Error reading item 0 from file
    a type out of range
      1: Error reading item 0 from file
    a negative size
      1: Error reading item 0 from file
    a link count of 32 bits
      1: Error reading item 0 from file
    a negative reference before its block
      1: Error reading item 0 from file
    a reference that is a string
      1: Error reading item 0 from file
    a reference past the index
      1: Error reading item 2000000 from file
    an entry that refers to itself (unlike ncdu)
      1: Error reading item 0 from file
    a directory that contains itself (unlike ncdu)
      1: Error reading item 0 from file
    two directories that share an entry (unlike ncdu)
      1: Error reading item 0 from file
    directories nested a million deep (unlike ncdu)
      0: 
    a value nested a million deep (unlike ncdu)
      0:
    ");
}

#[test]
fn binary_devices() {
    let directory = fixture();
    let root = directory.path();
    fs::write(
        root.join("devices.json"),
        br#"[1,2,{},[{"name":"/r","dev":1},[{"name":"a"},{"name":"af"}],{"name":"mid"},[{"name":"other","dev":7},{"name":"of"},[{"name":"deep","dev":9}]],{"name":"last"}]]"#,
    )
    .expect("export is written");
    assert!(
        duit(root, &["-f", "devices.json", "-O", "devices.ncdu"])
            .status
            .success()
    );
    assert_snapshot!(table(root, &[&["-f", "devices.ncdu", "-o-"]]), @r#"
    $ duit -f devices.ncdu -o-
    exit_code: 0
    ----- stdout -----
    [1,2,{[METADATA]},
    [{"name":"/r","dev":1},
    {"name":"last"},
    [{"name":"other","dev":7},
    [{"name":"deep","dev":9}],
    {"name":"of"}],
    {"name":"mid"},
    [{"name":"a"},
    {"name":"af"}]]]

    ----- stderr -----
    ^[[J
    "#);
}
