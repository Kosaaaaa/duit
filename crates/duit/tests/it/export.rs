use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::process::{Command, Stdio};

use insta::assert_snapshot;
use tempfile::TempDir;

use crate::{BINARY_SIGNATURE, data_blocks, duit, redact, render, table, text};
#[cfg(feature = "test-ncdu")]
use crate::{assert_ncdu, ncdu};

const CACHEDIR_TAG: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

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

fn sorted(export: &str) -> String {
    let mut lines = export.lines();
    let mut output = lines.next().unwrap_or_default().to_owned();
    let mut open: Vec<(String, Vec<String>)> = vec![(String::new(), Vec::new())];
    for line in lines {
        let (item, rest) = line.split_at(line.rfind('}').map_or(0, |end| end + 1));
        match item.strip_prefix('[') {
            Some(directory) => open.push((directory.to_owned(), Vec::new())),
            None => {
                if let Some((_, entries)) = open.last_mut() {
                    entries.push(item.to_owned());
                }
            }
        }
        for _ in rest.matches(']') {
            if let Some((directory, mut entries)) = open.pop() {
                entries.sort();
                let closed = format!("[{directory}\n{}]", entries.join("\n"));
                match open.last_mut() {
                    Some((_, parent)) => parent.push(closed),
                    None => output.push_str(&closed),
                }
            }
        }
    }
    output
}

#[test]
fn threads() {
    let directory = TempDir::new().expect("temporary directory is created");
    let root = directory.path();
    for index in 0..40 {
        let path = root.join(format!("d{index:02}"));
        fs::create_dir_all(path.join("sub")).expect("directory is created");
        fs::write(path.join("file"), vec![0; 1000 * (index + 1)]).expect("file is written");
        fs::write(path.join("skip"), b"x").expect("file is written");
        fs::write(path.join("sub/nested"), b"x").expect("file is written");
    }
    fs::hard_link(root.join("d00/file"), root.join("d01/link")).expect("hard link is created");
    let exports = TempDir::new().expect("temporary directory is created");
    for flags in [&["--exclude", "skip"][..], &["-e", "--exclude", "skip"]] {
        let arguments = |threads| [&["-t", threads, "-o-"][..], flags].concat();
        #[cfg(feature = "test-ncdu")]
        assert_ncdu(root, &arguments("4"));
        let export = |arguments: &[&str]| {
            let output = duit(root, arguments);
            assert!(output.status.success(), "{}", render(&output));
            sorted(&text(&output.stdout))
        };
        assert_eq!(
            export(&arguments("4")),
            export(&arguments("1")),
            "{flags:?}"
        );
        let binary = |threads| {
            let file = exports.path().join(format!("threads-{threads}.ncdu"));
            let file = file.to_str().expect("path is UTF-8");
            let arguments = [
                &["-t", threads, "-O", file, "--export-block-size", "4"][..],
                flags,
            ]
            .concat();
            let output = duit(root, &arguments);
            assert!(output.status.success(), "{}", render(&output));
            let written = fs::read(file).expect("export is written");
            let blocks = data_blocks(&written);
            assert!(blocks >= 2, "{blocks} data blocks");
            export(&["-f", file, "-o-", "-e"])
        };
        assert_eq!(binary("4"), binary("1"), "{flags:?}");
    }
    let with_threads = |threads| duit(root, &["-t", threads, "-0", "--quit-after-scan"]);
    let output = with_threads("4");
    assert_eq!(render(&output), render(&with_threads("1")));
    assert!(output.status.success());
}

#[test]
fn threads_missing_root_exits_like_one_thread() {
    let directory = TempDir::new().expect("temporary directory is created");
    for export in ["-o-", "-O-"] {
        let with_threads = |threads| duit(directory.path(), &["-t", threads, export, "missing"]);
        let output = with_threads("4");
        assert_eq!(render(&output), render(&with_threads("1")), "{export}");
        assert!(!output.status.success(), "{export}");
    }
}

/// A thread that cannot be started ends duit with the Zig error of `std.Thread.spawn`, as
/// `ui.die` in `scan.zig` does, and an export writes nothing. Linux counts threads against the
/// process limit of the user, so a limit of 1 makes `pthread_create` fail with `EAGAIN`. Assumes
/// that the tests do not run as root, which ignores the limit.
#[cfg(target_os = "linux")]
#[test]
fn thread_spawn_error() {
    let directory = TempDir::new().expect("temporary directory is created");
    let mut rendered = String::new();
    for arguments in [&["-o-"][..], &["-0", "--quit-after-scan"]] {
        let output = Command::new("bash")
            .current_dir(directory.path())
            .args(["-c", r#"ulimit -u 1 && exec "$0" "$@""#])
            .arg(env!("CARGO_BIN_EXE_duit"))
            .args([&["--ignore-config", "-t", "2"][..], arguments].concat())
            .output()
            .expect("bash runs");
        let _ = writeln!(
            rendered,
            "$ duit -t 2 {}\n{}\n",
            arguments.join(" "),
            render(&output).trim_end()
        );
    }
    assert_snapshot!(rendered, @"
    $ duit -t 2 -o-
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[JError spawning thread: error.SystemResources

    $ duit -t 2 -0 --quit-after-scan
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[JError spawning thread: error.SystemResources
    ");
}

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
    assert!(
        duit(directory.path(), &["-O", "export.ncdu", "plain"])
            .status
            .success()
    );
    for arguments in [
        &["-c", "-o-", "plain"][..],
        &["-O-", "plain"],
        &["-f", "missing", "-O-"],
        &["-f", "export.ncdu", "-o-"],
    ] {
        assert_eq!(
            run(executable, arguments),
            output,
            "arguments: {arguments:?}"
        );
        #[cfg(feature = "test-ncdu")]
        assert_eq!(
            run(OsStr::new("ncdu"), arguments),
            output,
            "arguments: {arguments:?}"
        );
    }
    assert_snapshot!(output, @r#"
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError writing to file: BrokenPipe.
    "#);
}

#[test]
fn binary() {
    let directory = fixture();
    let root = directory.path();
    fs::create_dir(root.join("many")).expect("directory is created");
    for index in 0..300 {
        fs::write(
            root.join(format!("many/file-with-a-long-name-{index}")),
            b"x",
        )
        .expect("file is written");
    }
    let cases: &[&[&str]] = &[
        &["plain"],
        &["-e", "plain"],
        &["--compress-level", "20", "plain"],
        &["--export-block-size", "4", "many"],
    ];
    let mut rendered = String::new();
    for case in cases {
        let arguments = [&["-O", "duit.ncdu"], *case].concat();
        let output = duit(root, &arguments);
        let _ = writeln!(
            rendered,
            "$ duit {}\n{}\n",
            arguments.join(" "),
            render(&output).trim_end()
        );
        let export = fs::read(root.join("duit.ncdu")).expect("export is written");
        assert!(export.starts_with(BINARY_SIGNATURE));
        #[cfg(feature = "test-ncdu")]
        {
            let arguments = [&["-O", "ncdu.ncdu"], *case].concat();
            assert_eq!(render(&ncdu(root, &arguments)), render(&output));
            assert_eq!(
                render(&ncdu(root, &["-f", "duit.ncdu", "-o-", "-e"])),
                render(&ncdu(root, &["-f", "ncdu.ncdu", "-o-", "-e"])),
                "arguments: {arguments:?}"
            );
        }
    }
    assert_snapshot!(rendered, @"
    $ duit -O duit.ncdu plain
    exit_code: 0
    ----- stdout -----

    ----- stderr -----
    ^[[J

    $ duit -O duit.ncdu -e plain
    exit_code: 0
    ----- stdout -----

    ----- stderr -----
    ^[[J

    $ duit -O duit.ncdu --compress-level 20 plain
    exit_code: 0
    ----- stdout -----

    ----- stderr -----
    ^[[J

    $ duit -O duit.ncdu --export-block-size 4 many
    exit_code: 0
    ----- stdout -----

    ----- stderr -----
    ^[[J
    ");
}

#[test]
fn binary_stdout() {
    let directory = fixture();
    let file = duit(directory.path(), &["-O", "export.ncdu", "plain"]);
    assert!(file.status.success());
    let stdout = duit(directory.path(), &["-O-", "plain"]);
    assert!(stdout.status.success());
    assert_eq!(stdout.stderr, b"\x1b[J");
    assert_eq!(
        stdout.stdout,
        fs::read(directory.path().join("export.ncdu")).expect("export is written")
    );
}

#[test]
fn binary_errors() {
    let directory = fixture();
    let cases: &[&[&str]] = &[
        &["-O", "export.ncdu", "missing"],
        &["-O", "missing/export.ncdu", "plain"],
    ];
    #[cfg(feature = "test-ncdu")]
    for arguments in cases {
        assert_ncdu(directory.path(), arguments);
    }
    assert_snapshot!(table(directory.path(), cases), @"
    $ duit -O export.ncdu missing
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[JError opening directory: No such file or directory.

    $ duit -O missing/export.ncdu plain
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[JError opening export file: No such file or directory.
    ");
    let export = fs::read(directory.path().join("export.ncdu")).expect("export is written");
    let index = [&b"\x10\0\0\x10"[..], &[0; 8], b"\x10\0\0\x10"].concat();
    assert_eq!(export, [BINARY_SIGNATURE, &index].concat());
    #[cfg(feature = "test-ncdu")]
    {
        let output = ncdu(directory.path(), &["-O", "ncdu.ncdu", "missing"]);
        assert!(!output.status.success());
        assert_eq!(
            fs::read(directory.path().join("ncdu.ncdu")).expect("export is written"),
            export
        );
    }
}
