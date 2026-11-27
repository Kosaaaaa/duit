use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
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

const TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) const FOOTER: &[u8] = b"Total disk usage: ";

pub(crate) const LEAVE_ALTERNATE_SCREEN: &[u8] = b"\x1b[?1049l";

pub(crate) const ENTER_ALTERNATE_SCREEN: &[u8] = b"\x1b[?1049h";

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

pub(crate) struct Pty {
    pub(crate) master: File,
    pub(crate) terminal: File,
}

pub(crate) fn pty() -> Pty {
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).expect("pty is opened");
    grantpt(&master).expect("pty is granted");
    unlockpt(&master).expect("pty is unlocked");
    let name = ptsname(&master, Vec::new()).expect("pty has a name");
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

pub(crate) fn spawn_on(pty: &Pty, directory: &Path, arguments: &[&str]) -> Child {
    spawn_command_on(
        pty,
        directory,
        arguments,
        &mut Command::new(env!("CARGO_BIN_EXE_duit")),
    )
}

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

pub(crate) fn read_to_end(receiver: &Receiver<Vec<u8>>, output: &mut Vec<u8>) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok(bytes) => output.extend(bytes),
            Err(error) => {
                assert_eq!(
                    error,
                    RecvTimeoutError::Disconnected,
                    "the terminal is closed in time"
                );
                return;
            }
        }
    }
}

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

fn wait_for_return(receiver: &Receiver<Vec<u8>>, output: &mut Vec<u8>) {
    wait_for(receiver, output, LEAVE_ALTERNATE_SCREEN);
    let restored = output
        .windows(LEAVE_ALTERNATE_SCREEN.len())
        .position(|window| window == LEAVE_ALTERNATE_SCREEN)
        .expect("terminal is restored");
    output.drain(..restored);
    wait_for(receiver, output, FOOTER);
}

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
        (&pty.master).write_all(b"qq").expect("keys are typed");
        assert_eq!(wait_exit(&mut child).code(), Some(0), "{arguments:?}");
    }
}

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
        wait_for(&receiver, &mut output, b"/..");
        (&pty.master).write_all(b"q").expect("keys are typed");
        assert_eq!(wait_exit(&mut child).code(), Some(0), "{export}");
    }
}

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

fn browse_error(directory: &Path, arguments: &[&str], keys: &[u8]) -> String {
    let pty = pty();
    let receiver = read_output(&pty);
    let mut child = spawn_on(&pty, directory, arguments);
    drop(pty.terminal);
    let mut output = Vec::new();
    if !keys.is_empty() {
        wait_for(&receiver, &mut output, FOOTER);
        (&pty.master).write_all(keys).expect("keys are typed");
    }
    assert_eq!(wait_exit(&mut child).code(), Some(1), "{arguments:?}");
    read_to_end(&receiver, &mut output);
    let restored = output
        .windows(LEAVE_ALTERNATE_SCREEN.len())
        .rposition(|window| window == LEAVE_ALTERNATE_SCREEN)
        .expect("terminal is restored");
    text(&output[restored..])
}

#[test]
fn read_errors() {
    let directory = fixture();
    let file = item(b"\x00\x01\x01\x41f");
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

#[test]
fn refresh_error() {
    let directory = TempDir::new().expect("temporary directory is created");
    fs::create_dir(directory.path().join("sub")).expect("directory is created");
    let pty = pty();
    let receiver = read_output(&pty);
    let mut child = spawn_on(&pty, directory.path(), &["-0", "."]);
    let mut output = Vec::new();
    wait_for(&receiver, &mut output, FOOTER);
    (&pty.master).write_all(b"\r").expect("key is typed");
    fs::remove_dir(directory.path().join("sub")).expect("directory is removed");
    (&pty.master).write_all(b"r").expect("key is typed");
    wait_for(&receiver, &mut output, b"directory:");
    (&pty.master).write_all(b"x?").expect("keys are typed");
    wait_for(&receiver, &mut output, b"cursor");
    (&pty.master).write_all(b"qq").expect("keys are typed");
    assert_eq!(wait_exit(&mut child).code(), Some(0));
}

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

#[test]
fn delete() {
    let cases: [(&[&str], &[u8]); 2] = [(&["."], b"dh\r"), (&["--no-confirm-delete", "."], b"d")];
    for (arguments, keys) in cases {
        let directory = TempDir::new().expect("temporary directory is created");
        let file = directory.path().join("file");
        fs::write(&file, b"").expect("file is written");
        let pty = pty();
        let receiver = read_output(&pty);
        let mut child = spawn_on(&pty, directory.path(), arguments);
        let mut output = Vec::new();
        wait_for(&receiver, &mut output, FOOTER);
        (&pty.master).write_all(keys).expect("keys are typed");
        wait_removed(&file);
        (&pty.master).write_all(b"q").expect("key is typed");
        assert_eq!(wait_exit(&mut child).code(), Some(0), "{arguments:?}");
    }
}

fn delete_script(directory: &Path, body: &str) -> PathBuf {
    let path = directory.join("delete");
    fs::write(&path, format!("#!/bin/sh\n{body}")).expect("script is written");
    fs::set_permissions(&path, Permissions::from_mode(0o755)).expect("script is executable");
    path
}

#[test]
fn delete_command() {
    let directory = TempDir::new().expect("temporary directory is created");
    let file = directory.path().join("a \"b\" $c");
    fs::write(&file, b"").expect("file is written");
    let scripts = TempDir::new().expect("temporary directory is created");
    let record = scripts.path().join("record");
    let script = delete_script(
        scripts.path(),
        &format!(
            "printf '%s|%s|%s|%s\\n' \"$1\" \"$NCDU_DELETE_PATH\" \"$NCDU_LEVEL\" \"$(pwd -P)\" > '{record}'\n\
             mv \"$1\" '{trash}'\n",
            record = record.display(),
            trash = scripts.path().display(),
        ),
    );
    let script = script.to_str().expect("path is UTF-8");
    let pty = pty();
    let receiver = read_output(&pty);
    let mut command = Command::new(env!("CARGO_BIN_EXE_duit"));
    command.env_remove("NCDU_LEVEL");
    let mut child = spawn_command_on(
        &pty,
        directory.path(),
        &["--delete-command", script, "."],
        &mut command,
    );
    let mut output = Vec::new();
    wait_for(&receiver, &mut output, FOOTER);
    (&pty.master).write_all(b"dh\r").expect("keys are typed");
    wait_for_return(&receiver, &mut output);
    (&pty.master).write_all(b"q").expect("key is typed");
    assert_eq!(wait_exit(&mut child).code(), Some(0));
    assert!(!file.exists());
    let canonical = fs::canonicalize(directory.path()).expect("directory is resolved");
    let rendered = fs::read_to_string(&record)
        .expect("record is read")
        .replace(canonical.to_str().expect("path is UTF-8"), "[DIR]");
    assert_snapshot!(rendered, @r#"[DIR]/a "b" $c|[DIR]/a "b" $c|1|[DIR]"#);
}

#[test]
fn delete_command_error() {
    let directory = TempDir::new().expect("temporary directory is created");
    let file = directory.path().join("file");
    fs::write(&file, b"").expect("file is written");
    let pty = pty();
    let receiver = read_output(&pty);
    let mut child = spawn_on(&pty, directory.path(), &["--delete-command", "false", "."]);
    let mut output = Vec::new();
    wait_for(&receiver, &mut output, FOOTER);
    (&pty.master).write_all(b"dh\r").expect("keys are typed");
    wait_for(&receiver, &mut output, b"continue.");
    (&pty.master).write_all(b"\r").expect("key is typed");
    wait_for_return(&receiver, &mut output);
    wait_for(&receiver, &mut output, b"file");
    (&pty.master).write_all(b"q").expect("key is typed");
    assert_eq!(wait_exit(&mut child).code(), Some(0));
    assert!(file.exists());
    let entered = output
        .windows(ENTER_ALTERNATE_SCREEN.len())
        .position(|window| window == ENTER_ALTERNATE_SCREEN)
        .expect("terminal is entered again");
    assert_snapshot!(text(&output[..entered]), @"
    ^[[?1049l^[[?25h^M
    Command returned with error code 1.^M
    Press enter to continue.^M
    ^M
    ^[[J
    ");
}

#[test]
fn delete_command_directory() {
    for directory_before in [true, false] {
        let directory = TempDir::new().expect("temporary directory is created");
        let path = directory.path().join("sub");
        if directory_before {
            fs::create_dir(&path).expect("directory is created");
        } else {
            fs::write(&path, b"").expect("file is written");
        }
        let scripts = TempDir::new().expect("temporary directory is created");
        let script = delete_script(
            scripts.path(),
            &format!(
                "[ -d \"$1\" ] || {{ mv \"$1\" '{trash}' && mkdir \"$1\"; }}\n\
                 printf x > \"$1/added-file\"\n",
                trash = scripts.path().display(),
            ),
        );
        let script = script.to_str().expect("path is UTF-8");
        let pty = pty();
        let receiver = read_output(&pty);
        let mut child = spawn_on(&pty, directory.path(), &["--delete-command", script, "."]);
        let mut output = Vec::new();
        wait_for(&receiver, &mut output, FOOTER);
        (&pty.master).write_all(b"dh\r").expect("keys are typed");
        wait_for_return(&receiver, &mut output);
        (&pty.master).write_all(b"\r").expect("key is typed");
        wait_for(&receiver, &mut output, b"added-file");
        (&pty.master).write_all(b"q").expect("key is typed");
        assert_eq!(wait_exit(&mut child).code(), Some(0), "{directory_before}");
    }
}

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
