use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::process::{Command, Stdio};

use insta::assert_snapshot;
use tempfile::TempDir;

#[cfg(feature = "test-ncdu")]
use crate::{assert_ncdu, ncdu};
use crate::{duit, redact, render, text};

/// The start of a valid `CACHEDIR.TAG`.
const CACHEDIR_TAG: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// One directory per scan option, each with a single entry that the option changes. A single entry
/// keeps the export independent of the order of `readdir`.
fn fixture() -> TempDir {
    let directory = TempDir::new().expect("temporary directory is created");
    let root = directory.path();
    let write = |path: &str, contents: &[u8]| {
        let path = root.join(path);
        fs::create_dir_all(path.parent().expect("path has a parent"))
            .expect("directory is created");
        fs::write(path, contents).expect("file is written");
    };
    write("target", b"hello");
    write("plain/file", b"x");
    write("logs/a.log", b"x");
    write("listed/skip", b"x");
    write("patterns", b"skip\n");
    write("caches/cache/CACHEDIR.TAG", CACHEDIR_TAG);
    fs::create_dir(root.join("links")).expect("directory is created");
    symlink("../target", root.join("links/link")).expect("symlink is created");
    directory
}

/// The scan options reach the scan. The scan itself is tested in `duit-scan`.
#[test]
fn options() {
    let directory = fixture();
    let root = directory.path().canonicalize().expect("path is resolved");
    let cases: &[&[&str]] = &[
        &["plain"],
        &["-e", "plain"],
        &["-e", "--no-extended", "plain"],
        &["links"],
        &["-L", "links"],
        &["-L", "--no-follow-symlinks", "links"],
        &["--exclude", "*.log", "logs"],
        &["-X", "patterns", "listed"],
        &["--exclude-caches", "caches"],
        &["--exclude-caches", "--include-caches", "caches"],
    ];
    let mut rendered = String::new();
    for case in cases {
        let arguments = [&["-o-"], *case].concat();
        #[cfg(feature = "test-ncdu")]
        assert_ncdu(directory.path(), &arguments);
        let output = duit(directory.path(), &arguments);
        let _ = writeln!(
            rendered,
            "$ duit {}\n{}\n",
            arguments.join(" "),
            redact(&render(&output), &root).trim_end()
        );
    }
    assert_snapshot!(rendered, @r#"
        $ duit -o- plain
        exit_code: 0
        ----- stdout -----
        [1,2,{[METADATA]},
        [{"name":"[ROOT]/plain","asize":[N],"dev":[N]},
        {"name":"file","asize":[N]}]]

        ----- stderr -----
        ^[[J

        $ duit -o- -e plain
        exit_code: 0
        ----- stdout -----
        [1,2,{[METADATA]},
        [{"name":"[ROOT]/plain","asize":[N],"dev":[N],"uid":[N],"gid":[N],"mode":[N],"mtime":[N]},
        {"name":"file","asize":[N],"uid":[N],"gid":[N],"mode":[N],"mtime":[N]}]]

        ----- stderr -----
        ^[[J

        $ duit -o- -e --no-extended plain
        exit_code: 0
        ----- stdout -----
        [1,2,{[METADATA]},
        [{"name":"[ROOT]/plain","asize":[N],"dev":[N]},
        {"name":"file","asize":[N]}]]

        ----- stderr -----
        ^[[J

        $ duit -o- links
        exit_code: 0
        ----- stdout -----
        [1,2,{[METADATA]},
        [{"name":"[ROOT]/links","asize":[N],"dev":[N]},
        {"name":"link","asize":[N],"notreg":true}]]

        ----- stderr -----
        ^[[J

        $ duit -o- -L links
        exit_code: 0
        ----- stdout -----
        [1,2,{[METADATA]},
        [{"name":"[ROOT]/links","asize":[N],"dev":[N]},
        {"name":"link","asize":[N]}]]

        ----- stderr -----
        ^[[J

        $ duit -o- -L --no-follow-symlinks links
        exit_code: 0
        ----- stdout -----
        [1,2,{[METADATA]},
        [{"name":"[ROOT]/links","asize":[N],"dev":[N]},
        {"name":"link","asize":[N],"notreg":true}]]

        ----- stderr -----
        ^[[J

        $ duit -o- --exclude *.log logs
        exit_code: 0
        ----- stdout -----
        [1,2,{[METADATA]},
        [{"name":"[ROOT]/logs","asize":[N],"dev":[N]},
        {"name":"a.log","excluded":"pattern"}]]

        ----- stderr -----
        ^[[J

        $ duit -o- -X patterns listed
        exit_code: 0
        ----- stdout -----
        [1,2,{[METADATA]},
        [{"name":"[ROOT]/listed","asize":[N],"dev":[N]},
        {"name":"skip","excluded":"pattern"}]]

        ----- stderr -----
        ^[[J

        $ duit -o- --exclude-caches caches
        exit_code: 0
        ----- stdout -----
        [1,2,{[METADATA]},
        [{"name":"[ROOT]/caches","asize":[N],"dev":[N]},
        {"name":"cache","excluded":"pattern"}]]

        ----- stderr -----
        ^[[J

        $ duit -o- --exclude-caches --include-caches caches
        exit_code: 0
        ----- stdout -----
        [1,2,{[METADATA]},
        [{"name":"[ROOT]/caches","asize":[N],"dev":[N]},
        [{"name":"cache","asize":[N]},
        {"name":"CACHEDIR.TAG","asize":[N]}]]]

        ----- stderr -----
        ^[[J
    "#);
}

/// `-o FILE` writes the same export as `-o-` writes to standard output.
#[test]
fn file() {
    let directory = fixture();
    let output = duit(directory.path(), &["-o", "export.json", "plain"]);
    assert_snapshot!(render(&output), @r#"
        exit_code: 0
        ----- stdout -----

        ----- stderr -----
        ^[[J
    "#);
    let stdout = duit(directory.path(), &["-o-", "plain"]).stdout;
    assert_eq!(
        text(&fs::read(directory.path().join("export.json")).expect("export is written")),
        text(&stdout)
    );
}

/// `-c` writes the export as a zstd frame that decompresses to the export without `-c`. A scan
/// that fails at the root still ends the frame.
#[test]
fn compress() {
    let directory = fixture();
    let cases: &[&[&str]] = &[
        &["-c", "-o", "export.json.zst", "plain"],
        &[
            "--compress",
            "--compress-level",
            "20",
            "-o",
            "export.json.zst",
            "plain",
        ],
        &["-c", "-o", "export.json.zst", "missing"],
    ];
    let mut rendered = String::new();
    for arguments in cases {
        #[cfg(feature = "test-ncdu")]
        assert_ncdu(directory.path(), arguments);
        let output = duit(directory.path(), arguments);
        let _ = writeln!(
            rendered,
            "$ duit {}\n{}\n",
            arguments.join(" "),
            render(&output).trim_end()
        );
        let compressed =
            fs::read(directory.path().join("export.json.zst")).expect("export is written");
        let export = zstd::decode_all(compressed.as_slice()).expect("export is a zstd frame");
        let scanned = arguments.last().expect("a directory is given");
        let plain = duit(directory.path(), &["-o-", scanned]);
        assert_eq!(text(&export), text(&plain.stdout));
    }
    assert_snapshot!(rendered, @r"
    $ duit -c -o export.json.zst plain
    exit_code: 0
    ----- stdout -----

    ----- stderr -----
    ^[[J

    $ duit --compress --compress-level 20 -o export.json.zst plain
    exit_code: 0
    ----- stdout -----

    ----- stderr -----
    ^[[J

    $ duit -c -o export.json.zst missing
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[JError opening directory: No such file or directory.
    ");
}

/// ncdu imports an export that duit compressed.
#[cfg(feature = "test-ncdu")]
#[test]
fn compress_import() {
    let directory = fixture();
    let output = duit(directory.path(), &["-c", "-o", "export.json.zst", "plain"]);
    assert!(output.status.success());
    assert_eq!(
        render(&ncdu(directory.path(), &["-f", "export.json.zst", "-o-"])),
        render(&ncdu(directory.path(), &["-o-", "plain"]))
    );
}

/// A write to a closed pipe fails, and the export ends with the error. Both programs ignore
/// `SIGPIPE`.
#[test]
fn write_error() {
    let directory = fixture();
    let run = |program: &OsStr, arguments: &[&str]| {
        let (reader, writer) = io::pipe().expect("pipe is created");
        drop(reader);
        let output = Command::new(program)
            .current_dir(directory.path())
            .arg("--ignore-config")
            .args(arguments)
            .stdout(writer)
            .stderr(Stdio::piped())
            .output()
            .expect("program runs");
        render(&output)
    };
    let executable = OsStr::new(env!("CARGO_BIN_EXE_duit"));
    let output = run(executable, &["-o-", "plain"]);
    #[cfg(feature = "test-ncdu")]
    assert_eq!(run(OsStr::new("ncdu"), &["-o-", "plain"]), output);
    // The compressed export fails with the same error.
    assert_eq!(run(executable, &["-c", "-o-", "plain"]), output);
    #[cfg(feature = "test-ncdu")]
    assert_eq!(run(OsStr::new("ncdu"), &["-c", "-o-", "plain"]), output);
    assert_snapshot!(output, @r#"
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError writing to file: BrokenPipe.
    "#);
}
