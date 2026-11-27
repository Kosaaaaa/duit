use std::fmt::Write as _;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::Duration;

use duit_model::is_event_count;
use insta::assert_snapshot;
use tempfile::TempDir;

use crate::browse::{
    ENTER_ALTERNATE_SCREEN, FOOTER, LEAVE_ALTERNATE_SCREEN, fixture, pty, read_output, read_to_end,
    spawn, spawn_on, wait_exit, wait_for,
};
use crate::text;

const BEFORE_PAUSE: &[u8] = br#"[1,2,{},[{"name":"/root"},{"name":"a","dsize":4096}"#;
const FILES_BEFORE_PAUSE: usize = 1;
const AFTER_PAUSE: &[u8] = br#",{"name":"b","dsize":8192},{"name":"c"}]]"#;

const PAUSE: Duration = Duration::from_millis(500);

const CLEAR_TO_END: &[u8] = b"\x1b[J";

#[derive(Debug, Clone, Copy)]
enum To {
    Terminal,
    Pipe,
}

#[derive(Debug, Clone, Copy)]
struct Case<'a> {
    arguments: &'a [&'a str],
    stdout: To,
    stderr: To,
    term: &'a str,
}

fn import(program: &str, directory: &Path, case: Case<'_>) -> Vec<u8> {
    let Case {
        arguments,
        stdout,
        stderr,
        term,
    } = case;
    let pty = pty();
    let receiver = read_output(&pty);
    let output = |to: To| match to {
        To::Terminal => Stdio::from(pty.terminal.try_clone().expect("terminal is duplicated")),
        To::Pipe => Stdio::piped(),
    };
    let mut command = Command::new(program);
    command
        .env("TERM", term)
        .stdin(Stdio::piped())
        .stdout(output(stdout))
        .stderr(output(stderr));
    let mut child = spawn(directory, arguments, &mut command, false);
    let mut input = child.stdin.take().expect("standard input is a pipe");
    input.write_all(BEFORE_PAUSE).expect("import is written");
    thread::sleep(PAUSE);
    input.write_all(AFTER_PAUSE).expect("import is written");
    drop(input);
    let mut written = Vec::new();
    match stderr {
        To::Terminal => wait_for(&receiver, &mut written, CLEAR_TO_END),
        To::Pipe => {
            child
                .stderr
                .take()
                .expect("standard error is a pipe")
                .read_to_end(&mut written)
                .expect("standard error is read");
        }
    }
    assert!(wait_exit(&mut child).success(), "{program} {arguments:?}");
    written
}

fn table(cases: &[Case<'_>]) -> String {
    let directory = TempDir::new().expect("temporary directory is created");
    let mut rendered = String::new();
    for &case in cases {
        let duit = import(env!("CARGO_BIN_EXE_duit"), directory.path(), case);
        #[cfg(feature = "test-ncdu")]
        assert_eq!(
            text(&import("ncdu", directory.path(), case)),
            text(&duit),
            "{case:?}"
        );
        let Case {
            arguments,
            stdout,
            stderr,
            term,
        } = case;
        let _ = writeln!(
            rendered,
            "$ TERM={term} duit {} (stdout: {stdout:?}, stderr: {stderr:?})\n{}\n",
            arguments.join(" "),
            text(&duit)
        );
    }
    rendered
}

#[test]
fn line() {
    let arguments: &[&str] = &["-1", "-f", "-", "-o", "export.json"];
    let quiet: &[&str] = &["-q", "-1", "-f", "-", "-o", "export.json"];
    let fast: &[&str] = &[
        "--slow-ui-updates",
        "--fast-ui-updates",
        "-1",
        "-f",
        "-",
        "-o",
        "export.json",
    ];
    let threads: &[&str] = &["-t", "3", "-1", "-f", "-", "-o", "export.json"];
    assert_snapshot!(table(&[
        Case { arguments, stdout: To::Pipe, stderr: To::Pipe, term: "xterm" },
        Case { arguments, stdout: To::Pipe, stderr: To::Terminal, term: "xterm" },
        Case { arguments, stdout: To::Pipe, stderr: To::Terminal, term: "dumb" },
        Case { arguments: quiet, stdout: To::Pipe, stderr: To::Terminal, term: "xterm" },
        Case { arguments: fast, stdout: To::Pipe, stderr: To::Terminal, term: "xterm" },
        Case { arguments: threads, stdout: To::Pipe, stderr: To::Terminal, term: "xterm" },
    ]), @"
    $ TERM=xterm duit -1 -f - -o export.json (stdout: Pipe, stderr: Pipe)
    2 files /  12.0 KiB
      #1: /root
    ^[[J

    $ TERM=xterm duit -1 -f - -o export.json (stdout: Pipe, stderr: Terminal)
    2 files /  12.0 KiB^M
      #1: /root^M
    ^[[1F^[[2K^[[1F^[[2K^[[J

    $ TERM=dumb duit -1 -f - -o export.json (stdout: Pipe, stderr: Terminal)
    2 files /  12.0 KiB^M
      #1: /root^M
    ^[[J

    $ TERM=xterm duit -q -1 -f - -o export.json (stdout: Pipe, stderr: Terminal)
    ^[[J

    $ TERM=xterm duit --slow-ui-updates --fast-ui-updates -1 -f - -o export.json (stdout: Pipe, stderr: Terminal)
    2 files /  12.0 KiB^M
      #1: /root^M
    ^[[1F^[[2K^[[1F^[[2K^[[J

    $ TERM=xterm duit -t 3 -1 -f - -o export.json (stdout: Pipe, stderr: Terminal)
    2 files /  12.0 KiB^M
      #1: /root^M
    ^[[1F^[[2K^[[1F^[[2K^[[J
    ");
}

#[test]
fn default() {
    assert_snapshot!(table(&[
        Case {
            arguments: &["-f", "-", "-o", "export.json"],
            stdout: To::Terminal,
            stderr: To::Terminal,
            term: "xterm",
        },
        Case {
            arguments: &["-f", "-", "-O", "export.ncdu"],
            stdout: To::Terminal,
            stderr: To::Terminal,
            term: "xterm",
        },
        Case {
            arguments: &["-f", "-", "-o", "export.json"],
            stdout: To::Pipe,
            stderr: To::Terminal,
            term: "xterm",
        },
        Case {
            arguments: &["-f", "-", "-o-"],
            stdout: To::Terminal,
            stderr: To::Pipe,
            term: "xterm",
        },
    ]), @"
    $ TERM=xterm duit -f - -o export.json (stdout: Terminal, stderr: Terminal)
    2 files /  12.0 KiB^M
      #1: /root^M
    ^[[1F^[[2K^[[1F^[[2K^[[J

    $ TERM=xterm duit -f - -O export.ncdu (stdout: Terminal, stderr: Terminal)
    2 files /  12.0 KiB^M
      #1: /root^M
    ^[[1F^[[2K^[[1F^[[2K^[[J

    $ TERM=xterm duit -f - -o export.json (stdout: Pipe, stderr: Terminal)
    ^[[J

    $ TERM=xterm duit -f - -o- (stdout: Terminal, stderr: Pipe)
    ^[[J
    ");
}

#[test]
fn full() {
    let directory = fixture();
    for arguments in [&["."][..], &["-f", "tree.json"], &["-2", "."]] {
        let pty = pty();
        let receiver = read_output(&pty);
        let mut child = spawn_on(&pty, directory.path(), arguments);
        let mut output = Vec::new();
        wait_for(&receiver, &mut output, FOOTER);
        (&pty.master).write_all(b"q").expect("key is typed");
        assert_eq!(wait_exit(&mut child).code(), Some(0), "{arguments:?}");
        let browser = position(&output, FOOTER);
        assert!(position(&output, b"Finalizing") < browser, "{arguments:?}");
        assert!(
            position(&output, b"hardlinks...") < browser,
            "{arguments:?}"
        );
        assert_eq!(count(&output, ENTER_ALTERNATE_SCREEN), 1, "{arguments:?}");
    }
}

fn position(output: &[u8], needle: &[u8]) -> usize {
    output
        .windows(needle.len())
        .position(|window| window == needle)
        .unwrap_or(output.len())
}

fn count(output: &[u8], needle: &[u8]) -> usize {
    output
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

fn full_import(
    program: &str,
    directory: &Path,
    arguments: &[&str],
    keys: &[u8],
) -> (ExitStatus, Vec<u8>) {
    let pty = pty();
    let receiver = read_output(&pty);
    let terminal = || Stdio::from(pty.terminal.try_clone().expect("terminal is duplicated"));
    let mut command = Command::new(program);
    command
        .env("TERM", "xterm")
        .stdin(Stdio::piped())
        .stdout(terminal())
        .stderr(terminal());
    let mut child = spawn(directory, arguments, &mut command, true);
    drop(command);
    drop(pty.terminal);
    let mut input = child.stdin.take().expect("standard input is a pipe");
    let mut output = Vec::new();
    input.write_all(BEFORE_PAUSE).expect("import is written");
    thread::sleep(PAUSE);
    input
        .write_all(br#",{"name":"b"}"#)
        .expect("import is written");
    wait_for(&receiver, &mut output, b"items:");
    (&pty.master).write_all(keys).expect("keys are typed");
    thread::sleep(PAUSE);
    // ncdu ends at once on Ctrl-C, and the pipe may be closed.
    let _ = input.write_all(br#",{"name":"c"},{"name":"d"}]]"#);
    drop(input);
    let status = wait_exit(&mut child);
    read_to_end(&receiver, &mut output);
    (status, output)
}

#[test]
fn full_abort() {
    let directory = TempDir::new().expect("temporary directory is created");
    let cases: [(&[&str], &[u8], i32); 3] = [
        (&["-f", "-"], b"q", 0),
        (&["--confirm-quit", "-f", "-"], b"qy", 0),
        (&["-f", "-"], b"\x03", 1),
    ];
    for (arguments, keys, code) in cases {
        let (status, output) = full_import(
            env!("CARGO_BIN_EXE_duit"),
            directory.path(),
            arguments,
            keys,
        );
        assert_eq!(status.code(), Some(code), "{arguments:?} {keys:?}");
        assert_eq!(count(&output, FOOTER), 0, "{arguments:?} {keys:?}");
        assert_eq!(
            count(&output, LEAVE_ALTERNATE_SCREEN),
            1,
            "{arguments:?} {keys:?}"
        );
        #[cfg(feature = "test-ncdu")]
        {
            let (status, output) = full_import("ncdu", directory.path(), arguments, keys);
            assert_eq!(status.code(), Some(code), "ncdu {arguments:?} {keys:?}");
            assert_eq!(count(&output, b"disk"), 0, "ncdu {arguments:?} {keys:?}");
        }
    }
}

#[test]
fn full_export() {
    let directory = TempDir::new().expect("temporary directory is created");
    let arguments = &["-2", "-o", "export.json", "-f", "-"];
    let export = directory.path().join("export.json");
    let (status, output) =
        full_import(env!("CARGO_BIN_EXE_duit"), directory.path(), arguments, b"");
    assert_eq!(status.code(), Some(0));
    assert_eq!(count(&output, LEAVE_ALTERNATE_SCREEN), 1);
    let duit = text(&fs::read(&export).expect("export is written"));
    #[cfg(feature = "test-ncdu")]
    {
        let (status, _) = full_import("ncdu", directory.path(), arguments, b"");
        assert_eq!(status.code(), Some(0));
        assert_eq!(text(&fs::read(&export).expect("export is written")), duit);
    }
    assert_snapshot!(duit, @r#"
    [1,2,{[METADATA]},
    [{"name":"/root"},
    {"name":"a","dsize":4096},
    {"name":"b"},
    {"name":"c"},
    {"name":"d"}]]
    "#);
}

#[test]
fn pause_ends_before_the_first_import_event() {
    assert!(!is_event_count(FILES_BEFORE_PAUSE));
    assert!(is_event_count(FILES_BEFORE_PAUSE + 1));
}
