//! The terminal that `ui.zig` sets up with ncurses, over crossterm: `init`, `deinit`, `getch`,
//! and `runCmd`.

use std::env;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufWriter, Write};
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::ExitStatusExt;
use std::panic;
use std::process::{self, Command, ExitStatus};
use std::time::Duration;

use crossterm::cursor::Show;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::{Frame, Terminal};

/// Why [`Ui::init`] failed.
#[derive(Debug)]
pub enum InitError {
    /// `/dev/tty` could not be opened.
    OpenTty(io::Error),
    /// The terminal could not be set up.
    Terminal(io::Error),
}

/// An input event, as `getch` in `ui.zig` reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Key(Key),
    /// The terminal changed its size, so the screen needs a redraw.
    Resize,
    /// Ctrl-C. ncurses keeps the terminal from turning it into a key, so it raises `SIGINT`, and
    /// the signal handler of ncurses restores the terminal and exits with status 1. The raw mode
    /// of crossterm reports it as a key instead.
    Interrupt,
}

/// A key press, as the browser reads it from ncurses `getch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A character key.
    Char(char),
    /// The Enter key, which ncurses reports as `\n`.
    Enter,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    /// A key that has no binding, such as a function key. It still closes a window that any key
    /// closes.
    Other,
}

/// The terminal while the full-screen interface owns it.
pub struct Ui {
    terminal: Terminal<CrosstermBackend<BufWriter<File>>>,
    /// The input after an ESC that crossterm read together with it, which [`Ui::getch`] returns
    /// next.
    pending: Option<Input>,
    /// The terminal that `terminal` draws on, to take over again after [`Ui::run_command`].
    output: File,
}

impl Ui {
    /// Take over the terminal, `init` in `ui.zig`. With `open_tty`, draw on `/dev/tty`, as ncdu
    /// does when standard input is not a terminal, and otherwise on standard output. crossterm
    /// reads the keys from standard input if it is a terminal, and otherwise from `/dev/tty`.
    ///
    /// The release profile aborts on a panic, so no `Drop` restores the terminal: a panic hook
    /// does, before the panic message is printed.
    pub fn init(open_tty: bool) -> Result<Self, InitError> {
        let output = if open_tty {
            OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")
                .map_err(InitError::OpenTty)?
        } else {
            File::from(
                io::stdout()
                    .as_fd()
                    .try_clone_to_owned()
                    .map_err(InitError::Terminal)?,
            )
        };
        let panic_output = output.try_clone().map_err(InitError::Terminal)?;
        let previous_hook = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            restore(&panic_output);
            previous_hook(info);
        }));
        let terminal = enter(&output)?;
        Ok(Self {
            terminal,
            output,
            pending: None,
        })
    }

    /// Give the terminal to `command` until it exits, and take it over again, `runCmd` in
    /// `ui.zig`. The command gets an `NCDU_LEVEL` one higher than duit's. A command that cannot
    /// run, or that a signal ends, leaves a message, and with `report_error` also one that exits
    /// with a status other than 0. After a message, Enter on standard input continues.
    pub fn run_command(
        &mut self,
        command: &mut Command,
        report_error: bool,
    ) -> Result<(), InitError> {
        restore(self.terminal.backend_mut());
        let level = env::var_os("NCDU_LEVEL").unwrap_or_default();
        command.env("NCDU_LEVEL", next_level(level.as_bytes()));
        // ncdu prints with `std.debug.print`, which ignores errors.
        let message = match command.status() {
            Ok(status) => exit_message(status, report_error),
            Err(error) => Some(format!(
                "Error running command: {}\n\nPress enter to continue.\n",
                spawn_error(&error)
            )),
        };
        if let Some(message) = message {
            let _ = io::stderr().write_all(message.as_bytes());
            let _ = io::stdin().lock().read_until(b'\n', &mut Vec::new());
        }
        // The clear that `init` writes before it takes over the terminal, for a line that `-1`
        // leaves behind.
        let _ = io::stderr().write_all(b"\x1b[J");
        // A new `Terminal` draws the whole screen, which the alternate screen lost.
        self.terminal = enter(&self.output)?;
        Ok(())
    }

    /// Draw a frame with `draw`, after a resize at the new size.
    pub fn draw(&mut self, draw: impl FnOnce(&mut Frame<'_>)) {
        // ncdu ignores the errors of ncurses' `refresh`.
        let _ = self.terminal.draw(draw);
    }

    /// Read the next input, `getch` in `ui.zig`. Without `block`, return `None` when no input is
    /// waiting. Events that are no input for ncurses, such as a focus change, are skipped.
    pub fn getch(&mut self, block: bool) -> io::Result<Option<Input>> {
        if let Some(input) = self.pending.take() {
            return Ok(Some(input));
        }
        loop {
            // crossterm 0.29 with `use-dev-tty` does not look at the events that it parsed already
            // when it polls with a timeout of zero, so a batch of keys would end after its first.
            // shortcut: waits up to 1 ms when no input is waiting, drop it once crossterm checks
            // its buffer on a zero timeout.
            if !block && !event::poll(Duration::from_millis(1))? {
                return Ok(None);
            }
            let input = match event::read()? {
                // crossterm reads an ESC and the key after it in the same read as that key with
                // ALT, and ncurses as two keys.
                // shortcut: also splits a key whose ALT comes from a CSI modifier, such as
                // `\x1b[1;3A`, which ncurses reads as one key; tell them apart by `KeyCode` if
                // such a key ever needs a binding.
                Event::Key(event)
                    if event.kind != KeyEventKind::Release
                        && event.modifiers.contains(KeyModifiers::ALT) =>
                {
                    self.pending = input_of(KeyEvent {
                        modifiers: event.modifiers.difference(KeyModifiers::ALT),
                        ..event
                    });
                    Some(Input::Key(Key::Char('\x1b')))
                }
                Event::Key(event) => input_of(event),
                Event::Resize(..) => Some(Input::Resize),
                Event::FocusGained | Event::FocusLost | Event::Mouse(_) | Event::Paste(_) => None,
            };
            if input.is_some() {
                return Ok(input);
            }
        }
    }

    /// Give the terminal back, `deinit` in `ui.zig`.
    pub fn deinit(mut self) {
        restore(self.terminal.backend_mut());
    }

    /// Give the terminal back and exit with status 1, as on Ctrl-C.
    pub fn interrupt(&mut self) -> ! {
        restore(self.terminal.backend_mut());
        // The SIGINT handler of ncurses restores the terminal and exits at once.
        process::exit(1)
    }
}

/// Enter raw mode and the alternate screen on `output`, and return the terminal that draws there.
fn enter(output: &File) -> Result<Terminal<CrosstermBackend<BufWriter<File>>>, InitError> {
    let output = output.try_clone().map_err(InitError::Terminal)?;
    enable_raw_mode().map_err(InitError::Terminal)?;
    let mut output = BufWriter::new(output);
    // The alternate screen starts empty. `Terminal::clear` is not used, because it asks the
    // terminal for the cursor position and waits for the answer.
    execute!(output, EnterAlternateScreen)
        .and_then(|()| Terminal::new(CrosstermBackend::new(output)))
        .map_err(|error| {
            let _ = disable_raw_mode();
            InitError::Terminal(error)
        })
}

/// The `NCDU_LEVEL` of a command that ncdu runs, from its own `level`: one higher, and 1 when it
/// is empty. Only the first character counts, so the level stops at 9, and anything that does
/// not start with a digit gives 1.
fn next_level(level: &[u8]) -> String {
    match level.first() {
        Some(&digit @ b'0'..=b'8') => char::from(digit + 1).to_string(),
        Some(b'9') => "9".to_string(),
        Some(_) | None => "1".to_string(),
    }
}

/// The message for a command that exited with `status`, if it needs one: a command that a signal
/// ended, and with `report_error`, one that exited with a status other than 0.
fn exit_message(status: ExitStatus, report_error: bool) -> Option<String> {
    let (kind, value) = match (status.code(), status.signal()) {
        (Some(0), _) => return None,
        (Some(_), _) if !report_error => return None,
        (Some(code), _) => ("error", code),
        (None, Some(signal)) => ("signal", signal),
        // `waitpid` without `WUNTRACED` reports no stopped child, so this is not reached.
        (None, None) => ("unknown", status.into_raw()),
    };
    Some(format!(
        "\nCommand returned with {kind} code {value}.\nPress enter to continue.\n"
    ))
}

/// The description of an error that keeps a command from running, as Zig 0.15 maps the errors of
/// `pipe2`, `fork`, `chdir`, and `execve` for `std.process.Child`, and `ui.errorString` prints
/// them. An `ELOOP` of `chdir` reads as one of `execve`, which Rust does not tell apart.
fn spawn_error(error: &io::Error) -> &'static str {
    match error.raw_os_error() {
        Some(libc::EACCES) => "Access denied",
        Some(libc::EPERM) => "PermissionDenied",
        Some(libc::E2BIG | libc::ENOMEM | libc::EAGAIN) => "Out of memory",
        Some(libc::EMFILE) => "Process file descriptor limit exceeded",
        Some(libc::ENFILE) => "System file descriptor limit exceeded",
        Some(libc::ENAMETOOLONG) => "Filename too long",
        Some(libc::EINVAL | libc::ENOEXEC) => "Invalid executable",
        #[cfg(target_vendor = "apple")]
        Some(libc::EBADEXEC | libc::EBADARCH) => "Invalid executable",
        #[cfg(target_os = "linux")]
        Some(libc::ELIBBAD) => "Invalid executable",
        Some(libc::EIO | libc::ELOOP) => "I/O error",
        Some(libc::EISDIR) => "Is a directory",
        Some(libc::ENOENT) => "No such file or directory",
        Some(libc::ENOTDIR) => "Not a directory",
        Some(libc::ETXTBSY) => "File is busy",
        _ => "Unexpected",
    }
}

/// Leave the alternate screen, show the cursor, and leave raw mode, ignoring errors.
fn restore(mut output: impl Write) {
    let _ = execute!(output, LeaveAlternateScreen, Show);
    let _ = disable_raw_mode();
}

/// The input of a key event, as ncurses reads the bytes that the terminal sends for it.
fn input_of(event: KeyEvent) -> Option<Input> {
    if event.kind == KeyEventKind::Release {
        return None;
    }
    let control = event.modifiers.contains(KeyModifiers::CONTROL);
    let key = match event.code {
        KeyCode::Char('c') if control => return Some(Input::Interrupt),
        // ncurses reads ^H as `KEY_BACKSPACE` under `TERM=xterm`, whose terminfo entry gives it
        // as the backspace key.
        KeyCode::Char('h') if control => Key::Backspace,
        // ^J is `\n`, which ncdu reads as Enter.
        KeyCode::Char('j') if control => Key::Enter,
        // ^@ is NUL, which ncdu reads as no key.
        KeyCode::Char(' ') if control => return None,
        KeyCode::Null => return None,
        KeyCode::Char(_) if control => Key::Other,
        KeyCode::Char(character) => Key::Char(character),
        KeyCode::Enter => Key::Enter,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Tab => Key::Char('\t'),
        KeyCode::Esc => Key::Char('\x1b'),
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::BackTab
        | KeyCode::Delete
        | KeyCode::Insert
        | KeyCode::F(_)
        | KeyCode::CapsLock
        | KeyCode::ScrollLock
        | KeyCode::NumLock
        | KeyCode::PrintScreen
        | KeyCode::Pause
        | KeyCode::Menu
        | KeyCode::KeypadBegin
        | KeyCode::Media(_)
        | KeyCode::Modifier(_) => Key::Other,
    };
    Some(Input::Key(key))
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    use super::{exit_message, next_level};

    /// `NCDU_LEVEL` counts from the first character only, up to 9. Upstream has no test for
    /// `runCmd`.
    #[test]
    fn next_levels() {
        assert_eq!(next_level(b""), "1");
        assert_eq!(next_level(b"0"), "1");
        assert_eq!(next_level(b"3"), "4");
        assert_eq!(next_level(b"8"), "9");
        assert_eq!(next_level(b"9"), "9");
        assert_eq!(next_level(b"12"), "2");
        assert_eq!(next_level(b"9x"), "9");
        assert_eq!(next_level(b"x"), "1");
        assert_eq!(next_level(b"\xff"), "1");
    }

    /// A command that a signal ended is always reported, and one that exited with a status other
    /// than 0 only with `report_error`, as `--delete-command` runs it.
    #[test]
    fn exit_messages() {
        let exited = |code| ExitStatus::from_raw(code << 8);
        let message = |kind, value| {
            format!("\nCommand returned with {kind} code {value}.\nPress enter to continue.\n")
        };
        assert_eq!(exit_message(exited(0), false), None);
        assert_eq!(exit_message(exited(0), true), None);
        assert_eq!(exit_message(exited(3), false), None);
        assert_eq!(exit_message(exited(3), true), Some(message("error", 3)));
        assert_eq!(
            exit_message(ExitStatus::from_raw(15), false),
            Some(message("signal", 15))
        );
        assert_eq!(
            exit_message(ExitStatus::from_raw(9), true),
            Some(message("signal", 9))
        );
    }
}
