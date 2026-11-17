use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
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
use crate::{binary_file, binary_frames, block_start, item, reference, render, table, text};

/// How long a test waits for duit to draw or to exit.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Text in the footer of the browser, which duit draws once the tree is loaded.
pub(crate) const FOOTER: &[u8] = b"Total disk usage: ";

/// The sequence that leaves the alternate screen, which duit writes when it restores the terminal.
pub(crate) const LEAVE_ALTERNATE_SCREEN: &[u8] = b"\x1b[?1049l";

/// The sequence that enters the alternate screen, which duit writes when it takes over the
/// terminal.
pub(crate) const ENTER_ALTERNATE_SCREEN: &[u8] = b"\x1b[?1049h";

/// A directory with a copy of the golden files, and a JSON export with a syntax error.
pub(crate) fn fixture() -> TempDir {
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
/// needs no terminal. A binary export is only opened, not read. The cases pass `-0` or `-1`,
/// because the full-screen progress of `-2` would take over the terminal.
/// With `-1`, the tree in memory draws the line that it counts hard links on, even after an error
/// at the root, and erases it only on a terminal.
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
        &["-1", "--quit-after-scan", "."],
        &["-1", "--quit-after-scan", "-f", "tree.json"],
        &["-1", "--quit-after-scan", "-f", "tree.ncdu"],
        &["-1", "--quit-after-scan", "-f", "malformed.json"],
        &["-1", "--quit-after-scan", "missing"],
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

    $ duit -1 --quit-after-scan .
    exit_code: 0
    ----- stdout -----

    ----- stderr -----
    Counting hardlinks...
    ^[[J

    $ duit -1 --quit-after-scan -f tree.json
    exit_code: 0
    ----- stdout -----

    ----- stderr -----
    Counting hardlinks...
    ^[[J

    $ duit -1 --quit-after-scan -f tree.ncdu
    exit_code: 0
    ----- stdout -----

    ----- stderr -----
    ^[[J

    $ duit -1 --quit-after-scan -f malformed.json
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[JError importing file on line 1:23: expected string.

    $ duit -1 --quit-after-scan missing
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    Counting hardlinks...
    ^[[JError opening directory: No such file or directory.
    ");
}

/// Without `-o` or `-O`, the errors of the import come before the browser opens. The cases pass
/// `-0`, as in [`quit_after_scan`].
#[test]
fn errors() {
    let directory = fixture();
    let cases: &[&[&str]] = &[&["-0", "-f", "missing"], &["-0", "-f", "malformed.json"]];
    #[cfg(feature = "test-ncdu")]
    for arguments in cases {
        assert_ncdu(directory.path(), arguments);
    }
    assert_snapshot!(table(directory.path(), cases), @"
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
    ");
}

/// A pseudo-terminal of 80 columns and 24 rows: the side that the test reads and writes, and the
/// terminal side that duit runs on.
pub(crate) struct Pty {
    pub(crate) master: File,
    pub(crate) terminal: File,
}

pub(crate) fn pty() -> Pty {
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
pub(crate) fn spawn(
    directory: &Path,
    arguments: &[&str],
    command: &mut Command,
    controlling: bool,
) -> Child {
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
pub(crate) fn spawn_on(pty: &Pty, directory: &Path, arguments: &[&str]) -> Child {
    spawn_command_on(
        pty,
        directory,
        arguments,
        &mut Command::new(env!("CARGO_BIN_EXE_duit")),
    )
}

/// Run duit with `command`, as [`spawn_on`] does.
fn spawn_command_on(
    pty: &Pty,
    directory: &Path,
    arguments: &[&str],
    command: &mut Command,
) -> Child {
    let terminal = || Stdio::from(pty.terminal.try_clone().expect("terminal is duplicated"));
    command
        .stdin(terminal())
        .stdout(terminal())
        .stderr(terminal());
    spawn(directory, arguments, command, true)
}

/// Send what duit writes to the terminal of `pty` to the returned receiver, until it closes.
pub(crate) fn read_output(pty: &Pty) -> Receiver<Vec<u8>> {
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
pub(crate) fn wait_for(receiver: &Receiver<Vec<u8>>, output: &mut Vec<u8>, needle: &[u8]) {
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
pub(crate) fn wait_exit(child: &mut Child) -> ExitStatus {
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

/// Wait until duit restores the terminal and draws the browser again, and drop what `output` held
/// before it restored the terminal.
fn wait_for_return(receiver: &Receiver<Vec<u8>>, output: &mut Vec<u8>) {
    wait_for(receiver, output, LEAVE_ALTERNATE_SCREEN);
    let restored = output
        .windows(LEAVE_ALTERNATE_SCREEN.len())
        .position(|window| window == LEAVE_ALTERNATE_SCREEN)
        .expect("terminal is restored");
    output.drain(..restored);
    wait_for(receiver, output, FOOTER);
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

/// A scan and an import, plain, compressed, or binary, open the browser, and `q` quits with status
/// 0 after the terminal is restored.
#[test]
fn quit() {
    let directory = fixture();
    for arguments in [
        &["."][..],
        &["-f", "tree.json"],
        &["-f", "tree-extended.json.zst"],
        &["-f", "tree.ncdu"],
    ] {
        let (status, _) = browse(directory.path(), arguments, b"q");
        assert_eq!(status.code(), Some(0), "arguments: {arguments:?}");
    }
}

/// Refreshing, deleting, and the shell are disabled by default for an import, and `-rr` disables
/// the shell for a scan. `b` has no message for reading from a file, so a binary export shows the
/// same one. The needles have no spaces, because duit does not write a space that is on the screen
/// already.
#[test]
fn features_disabled() {
    let directory = fixture();
    let cases: [(&[&str], &[u8], &[u8]); 5] = [
        (&["-f", "tree.json"], b"r", b"--enable-refresh"),
        (&["-f", "tree.json"], b"d", b"--enable-delete"),
        (&["-f", "tree.json"], b"b", b"--enable-shell"),
        (&["-f", "tree.ncdu"], b"b", b"--enable-shell"),
        (&["-rr", "."], b"b", b"--enable-shell"),
    ];
    for (arguments, key, needle) in cases {
        let pty = pty();
        let receiver = read_output(&pty);
        let mut child = spawn_on(&pty, directory.path(), arguments);
        let mut output = Vec::new();
        wait_for(&receiver, &mut output, FOOTER);
        (&pty.master).write_all(key).expect("key is typed");
        wait_for(&receiver, &mut output, needle);
        // The first `q` closes the message.
        (&pty.master).write_all(b"qq").expect("keys are typed");
        assert_eq!(wait_exit(&mut child).code(), Some(0), "{arguments:?}");
    }
}

/// Write an executable script in `directory` that appends `name`, its working directory, and its
/// `NCDU_LEVEL` to `record`, and exits with status 3. It stands in for a shell.
fn shell_script(directory: &Path, name: &str, record: &Path) -> PathBuf {
    let path = directory.join(name);
    let script = format!(
        "#!/bin/sh\nprintf '%s %s %s\\n' {name} \"$(pwd -P)\" \"${{NCDU_LEVEL-unset}}\" >> '{}'\nexit 3\n",
        record.display()
    );
    fs::write(&path, script).expect("script is written");
    fs::set_permissions(&path, Permissions::from_mode(0o755)).expect("script is executable");
    path
}

/// `b` runs `NCDU_SHELL`, or `SHELL` without it, in the open directory, with `NCDU_LEVEL` one
/// higher than duit's: 1 when it is unset, at most 9, and from the first character only, so 1 for
/// a letter and 2 for 12. The browser comes back once the shell exits, without a message for its
/// status 3, and `q` quits.
#[test]
fn shell() {
    let directory = TempDir::new().expect("temporary directory is created");
    fs::create_dir(directory.path().join("sub")).expect("directory is created");
    let scripts = TempDir::new().expect("temporary directory is created");
    let record = scripts.path().join("record");
    let ncdu_shell = shell_script(scripts.path(), "ncdu-shell", &record);
    let shell = shell_script(scripts.path(), "shell", &record);
    let cases: [(Option<&Path>, Option<&str>); 5] = [
        (Some(&ncdu_shell), None),
        (Some(&ncdu_shell), Some("3")),
        (Some(&ncdu_shell), Some("9")),
        (Some(&ncdu_shell), Some("x")),
        (None, Some("12")),
    ];
    for (ncdu_shell, level) in cases {
        let pty = pty();
        let receiver = read_output(&pty);
        let mut command = Command::new(env!("CARGO_BIN_EXE_duit"));
        command
            .env("SHELL", &shell)
            .env_remove("NCDU_SHELL")
            .env_remove("NCDU_LEVEL");
        if let Some(ncdu_shell) = ncdu_shell {
            command.env("NCDU_SHELL", ncdu_shell);
        }
        if let Some(level) = level {
            command.env("NCDU_LEVEL", level);
        }
        let mut child = spawn_command_on(&pty, directory.path(), &["."], &mut command);
        let mut output = Vec::new();
        wait_for(&receiver, &mut output, FOOTER);
        // `/sub` is empty, so no key is dropped while it is listed.
        (&pty.master).write_all(b"\rb").expect("keys are typed");
        wait_for_return(&receiver, &mut output);
        (&pty.master).write_all(b"q").expect("key is typed");
        assert_eq!(wait_exit(&mut child).code(), Some(0), "{level:?}");
    }
    let canonical = fs::canonicalize(directory.path()).expect("directory is resolved");
    let rendered = fs::read_to_string(&record)
        .expect("record is read")
        .replace(canonical.to_str().expect("path is UTF-8"), "[DIR]");
    assert_snapshot!(rendered, @"
    ncdu-shell [DIR]/sub 1
    ncdu-shell [DIR]/sub 4
    ncdu-shell [DIR]/sub 9
    ncdu-shell [DIR]/sub 1
    shell [DIR]/sub 2
    ");
}

/// A shell that cannot run, or that a signal ends, leaves a message on the restored terminal, as
/// in ncdu, and the browser comes back after Enter. The clear before the browser is ncdu's too.
#[test]
fn shell_errors() {
    let directory = fixture();
    let scripts = TempDir::new().expect("temporary directory is created");
    let shell = shell_script(scripts.path(), "shell", &scripts.path().join("record"));
    let signal = scripts.path().join("signal");
    fs::write(&signal, "#!/bin/sh\nkill -TERM $$\n").expect("script is written");
    fs::set_permissions(&signal, Permissions::from_mode(0o755)).expect("script is executable");
    let mut rendered = String::new();
    for ncdu_shell in [Path::new("/nonexistent"), &signal] {
        let pty = pty();
        let receiver = read_output(&pty);
        let mut command = Command::new(env!("CARGO_BIN_EXE_duit"));
        command.env("SHELL", &shell).env("NCDU_SHELL", ncdu_shell);
        let mut child = spawn_command_on(&pty, directory.path(), &["."], &mut command);
        let mut output = Vec::new();
        wait_for(&receiver, &mut output, FOOTER);
        (&pty.master).write_all(b"b").expect("key is typed");
        wait_for(&receiver, &mut output, b"continue.");
        (&pty.master).write_all(b"\r").expect("key is typed");
        wait_for_return(&receiver, &mut output);
        (&pty.master).write_all(b"q").expect("key is typed");
        assert_eq!(wait_exit(&mut child).code(), Some(0), "{ncdu_shell:?}");
        let entered = output
            .windows(ENTER_ALTERNATE_SCREEN.len())
            .position(|window| window == ENTER_ALTERNATE_SCREEN)
            .expect("terminal is entered again");
        let _ = writeln!(rendered, "{}", text(&output[..entered]));
    }
    assert_snapshot!(rendered, @"
    ^[[?1049l^[[?25hError running command: No such file or directory^M
    ^M
    Press enter to continue.^M
    ^M
    ^[[J
    ^[[?1049l^[[?25h^M
    Command returned with signal code 15.^M
    Press enter to continue.^M
    ^M
    ^[[J
    ");
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

/// Ctrl-\ ends duit with `SIGQUIT` after the terminal is restored.
#[test]
fn quit_signal() {
    let directory = fixture();
    let (status, _) = browse(directory.path(), &["-f", "tree.json"], b"\x1c");
    assert_eq!(status.signal(), Some(libc::SIGQUIT));
}

/// Ctrl-Z restores the terminal before it sends `SIGTSTP`, and duit takes the terminal over again
/// and draws the browser once it continues. duit runs in an orphaned process group, so the kernel
/// discards the signal instead of stopping it.
#[test]
fn suspend() {
    let directory = fixture();
    let pty = pty();
    let receiver = read_output(&pty);
    let mut child = spawn_on(&pty, directory.path(), &["-f", "tree.json"]);
    let mut output = Vec::new();
    wait_for(&receiver, &mut output, FOOTER);
    (&pty.master).write_all(b"\x1a").expect("key is typed");
    wait_for_return(&receiver, &mut output);
    (&pty.master).write_all(b"q").expect("key is typed");
    assert_eq!(wait_exit(&mut child).code(), Some(0));
}

/// An ESC that the terminal sends together with the next key, as for Alt+y, is two keys, as ncurses
/// reads them: the ESC closes the confirmation of `--confirm-quit`, and the `y` does not quit.
#[test]
fn escape_then_key() {
    let directory = fixture();
    let (status, _) = browse(
        directory.path(),
        &["-f", "tree.json", "--confirm-quit"],
        b"q\x1by\x03",
    );
    assert_eq!(status.code(), Some(1));
}

/// Keys that are typed while a directory is listed are dropped, as ncdu drops them once `loadDir`
/// has listed 2 entries: the `q` after Enter on `/src` does not quit.
#[test]
fn keys_dropped_while_loading() {
    let directory = fixture();
    for export in ["tree.json", "tree.ncdu"] {
        let pty = pty();
        let receiver = read_output(&pty);
        let mut child = spawn_on(&pty, directory.path(), &["-f", export]);
        let mut output = Vec::new();
        wait_for(&receiver, &mut output, FOOTER);
        (&pty.master).write_all(b"\rq").expect("keys are typed");
        // The row of the parent directory of `/src`.
        wait_for(&receiver, &mut output, b"/..");
        (&pty.master).write_all(b"q").expect("keys are typed");
        assert_eq!(wait_exit(&mut child).code(), Some(0), "{export}");
    }
}

/// With an import from standard input, duit draws on and reads keys from `/dev/tty`, its
/// controlling terminal, and writes nothing to standard output. A binary export is read from
/// standard input too when it is a file, which can seek.
#[test]
fn import_from_stdin() {
    let directory = fixture();
    for export in ["tree.json", "tree.ncdu"] {
        let pty = pty();
        let receiver = read_output(&pty);
        let mut command = Command::new(env!("CARGO_BIN_EXE_duit"));
        command
            .stdin(File::open(directory.path().join(export)).expect("export is opened"))
            .stdout(Stdio::piped())
            .stderr(Stdio::from(
                pty.terminal.try_clone().expect("terminal is duplicated"),
            ));
        let mut child = spawn(directory.path(), &["-f", "-"], &mut command, true);
        let mut output = Vec::new();
        wait_for(&receiver, &mut output, FOOTER);
        (&pty.master).write_all(b"q").expect("keys are typed");
        assert_eq!(wait_exit(&mut child).code(), Some(0), "{export}");
        let mut stdout = Vec::new();
        child
            .stdout
            .take()
            .expect("stdout is piped")
            .read_to_end(&mut stdout)
            .expect("stdout is read");
        assert_eq!(stdout, b"", "{export}");
    }
}

/// Without a controlling terminal, and with standard input that is not a terminal, duit cannot open
/// `/dev/tty` to draw the progress of `-2`, or the browser of a binary export, which is read once
/// the terminal is open. ncdu writes the message of `strerror`, which differs between systems.
/// `--quit-after-scan` lets a scan run without a terminal on standard input, and its progress
/// fails to open `/dev/tty` too.
#[test]
fn no_terminal() {
    let directory = fixture();
    let mut rendered = String::new();
    for arguments in [
        &["-f", "tree.json"][..],
        &["-f", "tree.ncdu"],
        &["--quit-after-scan", "."],
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_duit"));
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = spawn(directory.path(), arguments, &mut command, false);
        let output = child.wait_with_output().expect("duit exits");
        let _ = writeln!(
            rendered,
            "$ duit {}\n{}",
            arguments.join(" "),
            render(&output)
        );
    }
    let rendered = rendered
        .replace("Device not configured", "[ENXIO]")
        .replace("No such device or address", "[ENXIO]");
    assert_snapshot!(rendered, @"
    $ duit -f tree.json
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[J^[[JError opening /dev/tty: [ENXIO].

    $ duit -f tree.ncdu
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[J^[[JError opening /dev/tty: [ENXIO].

    $ duit --quit-after-scan .
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    ^[[J^[[JError opening /dev/tty: [ENXIO].
    ");
}

/// Run duit with `arguments` on a terminal, type `keys` once it draws the browser unless there are
/// none, and return what it wrote after it restored the terminal, which it exits with status 1.
fn browse_error(directory: &Path, arguments: &[&str], keys: &[u8]) -> String {
    let pty = pty();
    let receiver = read_output(&pty);
    let mut child = spawn_on(&pty, directory, arguments);
    let mut output = Vec::new();
    if !keys.is_empty() {
        wait_for(&receiver, &mut output, FOOTER);
        (&pty.master).write_all(keys).expect("keys are typed");
    }
    assert_eq!(wait_exit(&mut child).code(), Some(1), "{arguments:?}");
    // Everything that duit wrote is on its way once it exited.
    while let Ok(bytes) = receiver.recv_timeout(Duration::from_millis(500)) {
        output.extend(bytes);
    }
    let restored = output
        .windows(LEAVE_ALTERNATE_SCREEN.len())
        .rposition(|window| window == LEAVE_ALTERNATE_SCREEN)
        .expect("terminal is restored");
    text(&output[restored..])
}

/// A binary export is read once the terminal is open, so an error in it stops duit there, as it
/// stops ncdu, with the message once the terminal is restored: a root that is not a directory, and
/// a block that is read only when a directory is opened. Entries that refer to each other in a
/// cycle are an error too, unlike in ncdu, which reads them forever.
#[test]
fn read_errors() {
    let directory = fixture();
    let file = item(b"\x00\x01\x01\x41f");
    // Block 1 holds the root and its subdirectory, whose entry is in block 0, which is not a zstd
    // frame.
    let subdirectory = item(&[&b"\x00\x00\x01\x43sub"[..], &reference(0x0c, 0)].concat());
    let root = item(&[&b"\x00\x00\x01\x45/root"[..], &reference(0x0c, 1)].concat());
    let frame =
        zstd::bulk::compress(&[&subdirectory[..], &root].concat(), 3).expect("block is compressed");
    let root_reference = block_start(1) + u64::try_from(subdirectory.len()).expect("offset fits");
    let cycle = item(&[&b"\x00\x01\x01\x41f"[..], &reference(0x02, 0)].concat());
    let cycle_root = item(&[&b"\x00\x00\x01\x45/root"[..], &reference(0x0c, 0)].concat());
    let cases: [(&str, Vec<u8>, &[u8]); 3] = [
        ("file.ncdu", binary_file(&[&file], 0), b""),
        (
            "block.ncdu",
            binary_frames(&[b"not zstd", &frame], root_reference),
            b"\r",
        ),
        (
            "cycle.ncdu",
            binary_file(&[&cycle, &cycle_root], block_start(1)),
            b"",
        ),
    ];
    let mut rendered = String::new();
    for (name, export, keys) in cases {
        fs::write(directory.path().join(name), export).expect("export is written");
        let output = browse_error(directory.path(), &["-f", name], keys);
        let _ = writeln!(rendered, "$ duit -f {name}\n{output}");
    }
    assert_snapshot!(rendered, @"
    $ duit -f file.ncdu
    ^[[?1049l^[[?25h^[[JInvalid import^M

    $ duit -f block.ncdu
    ^[[?1049l^[[?25h^[[?25h^[[JError reading item 0 from file^M

    $ duit -f cycle.ncdu
    ^[[?1049l^[[?25h^[[JError reading item 0 from file^M
    ");
}

/// `r` scans the open directory again and lists what changed on disk since the scan, also with
/// `-0`, which a refresh ignores. The names have no spaces, because duit does not write a space
/// that is on the screen already.
#[test]
fn refresh() {
    for arguments in [&["."][..], &["-0", "."]] {
        let directory = TempDir::new().expect("temporary directory is created");
        fs::write(directory.path().join("gone"), [0; 5000]).expect("file is written");
        let pty = pty();
        let receiver = read_output(&pty);
        let mut child = spawn_on(&pty, directory.path(), arguments);
        let mut output = Vec::new();
        wait_for(&receiver, &mut output, FOOTER);
        fs::remove_file(directory.path().join("gone")).expect("file is removed");
        fs::write(directory.path().join("added-file"), [0; 7000]).expect("file is written");
        fs::create_dir(directory.path().join("added-directory")).expect("directory is created");
        (&pty.master).write_all(b"r").expect("key is typed");
        wait_for(&receiver, &mut output, b"added-file");
        wait_for(&receiver, &mut output, b"/added-directory");
        (&pty.master).write_all(b"q").expect("key is typed");
        assert_eq!(wait_exit(&mut child).code(), Some(0), "{arguments:?}");
    }
}

/// A refresh of a directory that is gone shows the error screen of `-2`, also with `-0`, and any
/// key goes back to the browser, where `?` opens the help.
#[test]
fn refresh_error() {
    let directory = TempDir::new().expect("temporary directory is created");
    fs::create_dir(directory.path().join("sub")).expect("directory is created");
    let pty = pty();
    let receiver = read_output(&pty);
    let mut child = spawn_on(&pty, directory.path(), &["-0", "."]);
    let mut output = Vec::new();
    wait_for(&receiver, &mut output, FOOTER);
    // `/sub` is empty, so no key is dropped while it is listed. It is opened from memory, and
    // removed before `r` is typed.
    (&pty.master).write_all(b"\r").expect("key is typed");
    fs::remove_dir(directory.path().join("sub")).expect("directory is removed");
    (&pty.master).write_all(b"r").expect("key is typed");
    wait_for(&receiver, &mut output, b"directory:");
    (&pty.master).write_all(b"x?").expect("keys are typed");
    wait_for(&receiver, &mut output, b"cursor");
    (&pty.master).write_all(b"qq").expect("keys are typed");
    assert_eq!(wait_exit(&mut child).code(), Some(0));
}

/// Wait until `path` no longer exists.
fn wait_removed(path: &Path) {
    let deadline = Instant::now() + TIMEOUT;
    while path.exists() {
        assert!(
            Instant::now() <= deadline,
            "{} is deleted in time",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

/// `d` deletes the entry under the cursor once "yes" is selected and confirmed, and at once with
/// `--no-confirm-delete`. With `--delete-command`, whose command duit does not run yet, `d` does
/// nothing rather than delete the entry itself.
#[test]
fn delete() {
    let cases: [(&[&str], &[u8], bool); 3] = [
        (&["."], b"dh\r", true),
        (&["--no-confirm-delete", "."], b"d", true),
        (&["--delete-command", "true", "."], b"dh\r", false),
    ];
    for (arguments, keys, deleted) in cases {
        let directory = TempDir::new().expect("temporary directory is created");
        let file = directory.path().join("file");
        fs::write(&file, b"").expect("file is written");
        let pty = pty();
        let receiver = read_output(&pty);
        let mut child = spawn_on(&pty, directory.path(), arguments);
        let mut output = Vec::new();
        wait_for(&receiver, &mut output, FOOTER);
        (&pty.master).write_all(keys).expect("keys are typed");
        if deleted {
            wait_removed(&file);
        }
        (&pty.master).write_all(b"q").expect("key is typed");
        assert_eq!(wait_exit(&mut child).code(), Some(0), "{arguments:?}");
        assert_eq!(file.exists(), !deleted, "{arguments:?}");
    }
}

/// A directory that is read-only until the value is dropped, so that the entries in it cannot be
/// deleted and the temporary directory that holds it can.
struct ReadOnly(PathBuf);

impl ReadOnly {
    fn new(path: PathBuf) -> Self {
        fs::set_permissions(&path, Permissions::from_mode(0o555)).expect("directory is read-only");
        Self(path)
    }
}

impl Drop for ReadOnly {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, Permissions::from_mode(0o755));
    }
}

/// Ctrl-C in the error window of a deletion ends duit with status 1.
#[test]
fn delete_interrupt() {
    let directory = TempDir::new().expect("temporary directory is created");
    let path = directory.path().join("ro");
    fs::create_dir(&path).expect("directory is created");
    fs::write(path.join("file"), b"").expect("file is written");
    let _read_only = ReadOnly::new(path);
    let pty = pty();
    let receiver = read_output(&pty);
    let mut child = spawn_on(&pty, directory.path(), &["."]);
    let mut output = Vec::new();
    wait_for(&receiver, &mut output, FOOTER);
    (&pty.master).write_all(b"dh\r").expect("keys are typed");
    wait_for(&receiver, &mut output, b"deleting");
    (&pty.master).write_all(b"\x03").expect("key is typed");
    assert_eq!(wait_exit(&mut child).code(), Some(1));
}
