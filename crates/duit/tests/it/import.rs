use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;

use insta::assert_snapshot;
use tempfile::TempDir;

#[cfg(feature = "test-ncdu")]
use crate::{assert_ncdu, ncdu, render_as_duit};
use crate::{duit, render, table, text};

/// An export with every kind of entry, extended fields, and a name that is not UTF-8.
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

/// A directory with [`EXPORT`], plain and compressed, and the inputs of the error cases.
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
    // A name that is too long for a JSON export.
    write(
        "long.json",
        &[
            &b"[1,2,{},[{\"name\":\"/root\"},{\"name\":\""[..],
            &[b'a'; 10757],
            b"\"}]]",
        ]
        .concat(),
    );
    write("binary.ncdu", b"\xbfncduEX1\x00\x00\x00\x00");
    fs::create_dir(directory.path().join("directory")).expect("directory is created");
    directory
}

/// Run `program` with `arguments` in `directory` and `input` on standard input.
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

/// `-f` imports an export, from a file or from standard input, plain or compressed, and `-o`
/// writes it again. `-e` keeps the extended fields.
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
    #[cfg(feature = "test-ncdu")]
    {
        assert_ncdu(directory.path(), &["-f", "export.json.zst", "-o-"]);
        let ncdu = with_input(
            OsStr::new("ncdu"),
            directory.path(),
            &["-f-", "-o-"],
            EXPORT,
        );
        assert_eq!(render_as_duit(&ncdu), stdin);
    }
}

/// An import of an export, plain or compressed, writes the export of the scan again.
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

/// `-c` compresses the export of an import.
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

/// The scan directory and the options that only change the scan have no effect on an import.
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

/// duit imports what ncdu exports, and writes what ncdu writes for that import.
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

/// A file that cannot be read, is too short, or is not a valid export ends the import with an
/// error, and nothing is written to the export.
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
    ");
    // As in ncdu, the export file is created before the import is read.
    assert_eq!(
        fs::read(directory.path().join("export.out")).expect("export file is created"),
        b""
    );
}

/// The binary export is not read yet.
#[test]
fn binary() {
    let directory = fixture();
    assert_snapshot!(table(directory.path(), &[&["-f", "binary.ncdu", "-o-"]]), @"
    $ duit -f binary.ncdu -o-
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[JImporting a binary export is not supported yet.
    ");
}
