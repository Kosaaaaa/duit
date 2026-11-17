use std::fmt::Write as _;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use insta::assert_snapshot;
use tempfile::TempDir;

use crate::browse::{pty, read_output, spawn, wait_exit, wait_for};
use crate::text;

/// An export of `/root` with 3 files, of which the import reads the first before [`PAUSE`] and the
/// others after it. An import can draw only when its count of files has bits 0 and 6 clear, so it
/// draws once, at the second file, which comes after the update delay has passed.
const BEFORE_PAUSE: &[u8] = br#"[1,2,{},[{"name":"/root"},{"name":"a","dsize":4096}"#;
const AFTER_PAUSE: &[u8] = br#",{"name":"b","dsize":8192},{"name":"c"}]]"#;

/// Longer than the default update delay of 100 ms, and shorter than the 2 s of `-q`.
const PAUSE: Duration = Duration::from_millis(500);

/// The end of what duit writes to standard error, the clear of `ui.deinit` in `ui.zig`.
const CLEAR_TO_END: &[u8] = b"\x1b[J";

/// Where standard output or standard error goes.
#[derive(Debug, Clone, Copy)]
enum To {
    Terminal,
    Pipe,
}

/// An import from standard input with a pause, in `arguments`, with standard output and standard
/// error on a terminal or a pipe, and `TERM` set to `term`.
type Case<'a> = (&'a [&'a str], To, To, &'a str);

/// Run `program` for `case` in `directory`, and return what it writes to standard error.
fn import(program: &str, directory: &Path, case: Case<'_>) -> Vec<u8> {
    let (arguments, stdout, stderr, term) = case;
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

/// Run duit for each of `cases`, and render what it writes to standard error under its command
/// line. With the `test-ncdu` feature, ncdu writes the same.
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
        let (arguments, stdout, stderr, term) = case;
        let _ = writeln!(
            rendered,
            "$ TERM={term} duit {} (stdout: {stdout:?}, stderr: {stderr:?})\n{}\n",
            arguments.join(" "),
            text(&duit)
        );
    }
    rendered
}

/// With `-1`, the scan or the import writes its progress to standard error once the update delay
/// has passed, and erases it when it is done, if standard error is a terminal that takes ANSI
/// escapes. `-q` and `--slow-ui-updates` raise the update delay to 2 s, and `--fast-ui-updates`
/// sets it back to 100 ms.
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
    assert_snapshot!(table(&[
        (arguments, To::Pipe, To::Pipe, "xterm"),
        (arguments, To::Pipe, To::Terminal, "xterm"),
        (arguments, To::Pipe, To::Terminal, "dumb"),
        (quiet, To::Pipe, To::Terminal, "xterm"),
        (fast, To::Pipe, To::Terminal, "xterm"),
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
    ");
}

/// Without `-0`, `-1`, or `-2`, an export to a file shows the progress of `-1` when standard output
/// is a terminal, and an export to standard output shows none.
#[test]
fn default() {
    assert_snapshot!(table(&[
        (&["-f", "-", "-o", "export.json"], To::Terminal, To::Terminal, "xterm"),
        (&["-f", "-", "-O", "export.ncdu"], To::Terminal, To::Terminal, "xterm"),
        (&["-f", "-", "-o", "export.json"], To::Pipe, To::Terminal, "xterm"),
        (&["-f", "-", "-o-"], To::Terminal, To::Pipe, "xterm"),
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
