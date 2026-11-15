use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::{Command, Stdio};

use insta::assert_snapshot;
use tempfile::TempDir;

#[cfg(feature = "test-ncdu")]
use crate::assert_ncdu;
use crate::{duit, render, text};

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

/// An export with the fields that differ between filesystems, users, and runs replaced. `dsize`
/// is removed, because an export leaves it out when it is 0, as it is for a directory on APFS.
fn redact(export: &str, root: &Path) -> String {
    let mut redacted = export.replace(&root.display().to_string(), "[ROOT]");
    for key in [
        "asize", "dsize", "dev", "ino", "mtime", "uid", "gid", "mode",
    ] {
        let pattern = format!(",\"{key}\":");
        let mut rest = redacted.as_str();
        let mut output = String::new();
        while let Some(start) = rest.find(&pattern) {
            output.push_str(&rest[..start]);
            if key != "dsize" {
                output.push_str(&pattern);
                output.push_str("[N]");
            }
            rest = rest[start + pattern.len()..]
                .trim_start_matches(|character: char| character.is_ascii_digit());
        }
        output.push_str(rest);
        redacted = output;
    }
    redacted
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

/// A write to a closed pipe fails, and the export ends with the error. Both programs ignore
/// `SIGPIPE`.
#[test]
fn write_error() {
    let directory = fixture();
    let run = |program: &OsStr| {
        let (reader, writer) = io::pipe().expect("pipe is created");
        drop(reader);
        let output = Command::new(program)
            .current_dir(directory.path())
            .args(["--ignore-config", "-o-", "plain"])
            .stdout(writer)
            .stderr(Stdio::piped())
            .output()
            .expect("program runs");
        render(&output)
    };
    let output = run(OsStr::new(env!("CARGO_BIN_EXE_duit")));
    #[cfg(feature = "test-ncdu")]
    assert_eq!(run(OsStr::new("ncdu")), output);
    assert_snapshot!(output, @r#"
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError writing to file: BrokenPipe.
    "#);
}
