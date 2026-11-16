use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use insta::assert_snapshot;
use rustix::process::{ioctl_tiocsctty, setsid};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::stdio::stderr;
use rustix::termios::{Winsize, tcsetwinsize};
use tempfile::TempDir;

#[cfg(feature = "test-ncdu")]
use crate::assert_ncdu;
use crate::{render, table};

/// How long a test waits for duit to draw or to exit.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Text in the footer of the browser, which duit draws once the tree is loaded.
const FOOTER: &[u8] = b"Total disk usage: ";

/// The sequence that leaves the alternate screen, which duit writes when it restores the terminal.
const LEAVE_ALTERNATE_SCREEN: &[u8] = b"\x1b[?1049l";

/// A directory with a copy of the golden files, and a JSON export with a syntax error.
fn fixture() -> TempDir {
    let directory = TempDir::new().expect("temporary directory is created");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for entry in fs::read_dir(fixtures).expect("fixtures are listed") {
        let path = entry.expect("fixture is listed").path();
        let name = path.file_name().expect("fixture has a name");
        fs::copy(&path, directory.path().join(name)).expect("fixture is copied");
    }
    fs::write(
        directory.path().join("malformed.json"),
        b"[1,0,{},[{\"name\":\"a\",}]]",
    )
    .expect("file is written");
    directory
}

/// `--quit-after-scan` scans or imports into memory and exits without opening the browser, so it
/// needs no terminal. A binary export is only opened, not read. The cases pass `-0`, because duit
/// shows no scan progress yet, and ncdu opens its full-screen progress on the terminal without it.
#[test]
fn quit_after_scan() {
    let directory = fixture();
    let cases: &[&[&str]] = &[
        &["-0", "--quit-after-scan", "."],
        &["-0", "--quit-after-scan", "-f", "tree.json"],
        &[
            "-0",
            "--quit-after-scan",
            "-f",
            "tree-extended.json.zst",
            "-e",
        ],
        &["-0", "--quit-after-scan", "-f", "tree.ncdu"],
        &["-0", "--quit-after-scan", "-f", "malformed.json"],
        &["-0", "--quit-after-scan", "missing"],
    ];
    #[cfg(feature = "test-ncdu")]
    for arguments in cases {
        assert_ncdu(directory.path(), arguments);
    }
    assert_snapshot!(table(directory.path(), cases), @"
    $ duit -0 --quit-after-scan .
    exit_code: 0
    ----- stdout -----

    ----- stderr -----
    ^[[J

    $ duit -0 --quit-after-scan -f tree.json
    exit_code: 0
    ----- stdout -----

    ----- stderr -----
    ^[[J

    $ duit -0 --quit-after-scan -f tree-extended.json.zst -e
    exit_code: 0
    ----- stdout -----

    ----- stderr -----
    ^[[J

    $ duit -0 --quit-after-scan -f tree.ncdu
    exit_code: 0
    ----- stdout -----

    ----- stderr -----
    ^[[J

    $ duit -0 --quit-after-scan -f malformed.json
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[JError importing file on line 1:23: expected string.

    $ duit -0 --quit-after-scan missing
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[JError opening directory: No such file or directory.
    ");
}

/// Without `-o` or `-O`, the errors of the import come before the browser opens. Browsing a binary
/// export is not supported yet. The cases pass `-0`, as in [`quit_after_scan`].
#[test]
fn errors() {
    let directory = fixture();
    let cases: &[&[&str]] = &[&["-0", "-f", "missing"], &["-0", "-f", "malformed.json"]];
    #[cfg(feature = "test-ncdu")]
    for arguments in cases {
        assert_ncdu(directory.path(), arguments);
    }
    assert_snapshot!(table(directory.path(), &[cases[0], cases[1], &["-0", "-f", "tree.ncdu"]]), @"
    $ duit -0 -f missing
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[JError reading file 'missing': No such file or directory.

    $ duit -0 -f malformed.json
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[JError importing file on line 1:23: expected string.

    $ duit -0 -f tree.ncdu
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[JBrowsing a binary export is not supported yet.
    ");
}

/// A pseudo-terminal of 80 columns and 24 rows: the side that the test reads and writes, and the
/// terminal side that duit runs on.
struct Pty {
    master: File,
    terminal: File,
}

fn pty() -> Pty {
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).expect("pty is opened");
    grantpt(&master).expect("pty is granted");
    unlockpt(&master).expect("pty is unlocked");
    let name = ptsname(&master, Vec::new()).expect("pty has a name");
    // The test process keeps the controlling terminal it has, if any.
    let terminal = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(OsStr::from_bytes(name.as_bytes()))
        .expect("terminal side is opened");
    let size = Winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    tcsetwinsize(&terminal, size).expect("terminal size is set");
    Pty {
        master: master.into(),
        terminal,
    }
}

/// Run duit with `arguments` in `directory`, in a new session without a controlling terminal, so
/// that it cannot reach the terminal that runs the tests. With `controlling`, its standard error
/// becomes the controlling terminal of the session.
fn spawn(directory: &Path, arguments: &[&str], command: &mut Command, controlling: bool) -> Child {
    command
        .current_dir(directory)
        .arg("--ignore-config")
        .args(arguments);
    #[expect(unsafe_code)]
    // SAFETY: The closure runs in the child between `fork` and `exec`, and calls only `setsid` and
    // `ioctl`, which are async-signal-safe, and allocates nothing.
    unsafe {
        command.pre_exec(move || {
            setsid()?;
            if controlling {
                ioctl_tiocsctty(stderr())?;
            }
            Ok(())
        });
    }
    command.spawn().expect("duit runs")
}

/// Run duit with `arguments` in `directory` on the terminal of `pty`, as its standard input,
/// output, and error.
fn spawn_on(pty: &Pty, directory: &Path, arguments: &[&str]) -> Child {
    let terminal = || Stdio::from(pty.terminal.try_clone().expect("terminal is duplicated"));
    let mut command = Command::new(env!("CARGO_BIN_EXE_duit"));
    command
        .stdin(terminal())
        .stdout(terminal())
        .stderr(terminal());
    spawn(directory, arguments, &mut command, true)
}

/// Send what duit writes to the terminal of `pty` to the returned receiver, until it closes.
fn read_output(pty: &Pty) -> Receiver<Vec<u8>> {
    let mut master = pty.master.try_clone().expect("pty is duplicated");
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut buffer = [0; 4096];
        while let Ok(length @ 1..) = master.read(&mut buffer) {
            if sender.send(buffer[..length].to_vec()).is_err() {
                break;
            }
        }
    });
    receiver
}

/// Append what `receiver` reads to `output` until it holds `needle`.
fn wait_for(receiver: &Receiver<Vec<u8>>, output: &mut Vec<u8>, needle: &[u8]) {
    let deadline = Instant::now() + TIMEOUT;
    while !output.windows(needle.len()).any(|window| window == needle) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let bytes = receiver
            .recv_timeout(remaining)
            .expect("duit writes the text in time");
        output.extend(bytes);
    }
}

/// Wait for `child` to exit, and kill it if it does not.
fn wait_exit(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().expect("child is waited for") {
            return status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
        }
        assert!(Instant::now() <= deadline, "duit exits in time");
        thread::sleep(Duration::from_millis(10));
    }
}

/// Run duit with `arguments` on a terminal, wait until it draws the browser, type `keys`, and
/// return its exit status with everything it wrote to the terminal.
fn browse(directory: &Path, arguments: &[&str], keys: &[u8]) -> (ExitStatus, Vec<u8>) {
    let pty = pty();
    let receiver = read_output(&pty);
    let mut child = spawn_on(&pty, directory, arguments);
    let mut output = Vec::new();
    wait_for(&receiver, &mut output, FOOTER);
    (&pty.master).write_all(keys).expect("keys are typed");
    let status = wait_exit(&mut child);
    wait_for(&receiver, &mut output, LEAVE_ALTERNATE_SCREEN);
    (status, output)
}

/// A scan and an import, plain or compressed, open the browser, and `q` quits with status 0 after
/// the terminal is restored.
#[test]
fn quit() {
    let directory = fixture();
    for arguments in [
        &["."][..],
        &["-f", "tree.json"],
        &["-f", "tree-extended.json.zst"],
    ] {
        let (status, _) = browse(directory.path(), arguments, b"q");
        assert_eq!(status.code(), Some(0), "arguments: {arguments:?}");
    }
}

/// Ctrl-C ends duit with status 1 after the terminal is restored, as ncurses does on `SIGINT`, also
/// in the confirmation of `--confirm-quit`.
#[test]
fn interrupt() {
    let directory = fixture();
    let cases: [(&[&str], &[u8]); 2] = [
        (&["-f", "tree.json"], b"\x03"),
        (&["-f", "tree.json", "--confirm-quit"], b"q\x03"),
    ];
    for (arguments, keys) in cases {
        let (status, _) = browse(directory.path(), arguments, keys);
        assert_eq!(status.code(), Some(1), "arguments: {arguments:?}");
    }
}

/// With an import from standard input, duit draws on and reads keys from `/dev/tty`, its
/// controlling terminal, and writes nothing to standard output.
#[test]
fn import_from_stdin() {
    let directory = fixture();
    let pty = pty();
    let receiver = read_output(&pty);
    let mut command = Command::new(env!("CARGO_BIN_EXE_duit"));
    command
        .stdin(File::open(directory.path().join("tree.json")).expect("export is opened"))
        .stdout(Stdio::piped())
        .stderr(Stdio::from(
            pty.terminal.try_clone().expect("terminal is duplicated"),
        ));
    let mut child = spawn(directory.path(), &["-f", "-"], &mut command, true);
    let mut output = Vec::new();
    wait_for(&receiver, &mut output, FOOTER);
    (&pty.master).write_all(b"q").expect("keys are typed");
    assert_eq!(wait_exit(&mut child).code(), Some(0));
    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .expect("stdout is piped")
        .read_to_end(&mut stdout)
        .expect("stdout is read");
    assert_eq!(stdout, b"");
}

/// Without a controlling terminal, and with standard input that is not a terminal, duit cannot open
/// `/dev/tty` to draw the browser. ncdu writes the message of `strerror`, which differs between
/// systems.
#[test]
fn no_terminal() {
    let directory = fixture();
    let mut command = Command::new(env!("CARGO_BIN_EXE_duit"));
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = spawn(directory.path(), &["-f", "tree.json"], &mut command, false);
    let output = child.wait_with_output().expect("duit exits");
    let rendered = render(&output)
        .replace("Device not configured", "[ENXIO]")
        .replace("No such device or address", "[ENXIO]");
    assert_snapshot!(rendered, @"
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[J^[[JError opening /dev/tty: [ENXIO].
    ");
}
