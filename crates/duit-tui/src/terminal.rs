//! The terminal that `ui.zig` sets up with ncurses, over crossterm: `init`, `deinit`, and
//! `getch`.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::os::fd::AsFd;
use std::panic;
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
        enable_raw_mode().map_err(InitError::Terminal)?;
        let previous_hook = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            restore(&panic_output);
            previous_hook(info);
        }));
        let mut output = BufWriter::new(output);
        // The alternate screen starts empty. `Terminal::clear` is not used, because it asks the
        // terminal for the cursor position and waits for the answer.
        let terminal = execute!(output, EnterAlternateScreen)
            .and_then(|()| Terminal::new(CrosstermBackend::new(output)));
        match terminal {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                let _ = disable_raw_mode();
                Err(InitError::Terminal(error))
            }
        }
    }

    /// Draw a frame with `draw`, after a resize at the new size.
    pub fn draw(&mut self, draw: impl FnOnce(&mut Frame<'_>)) {
        // ncdu ignores the errors of ncurses' `refresh`.
        let _ = self.terminal.draw(draw);
    }

    /// Read the next input, `getch` in `ui.zig`. Without `block`, return `None` when no input is
    /// waiting. Events that are no input for ncurses, such as a focus change, are skipped.
    pub fn getch(&self, block: bool) -> io::Result<Option<Input>> {
        loop {
            // crossterm 0.29 with `use-dev-tty` does not look at the events that it parsed already
            // when it polls with a timeout of zero, so a batch of keys would end after its first.
            // shortcut: waits up to 1 ms when no input is waiting, drop it once crossterm checks
            // its buffer on a zero timeout.
            if !block && !event::poll(Duration::from_millis(1))? {
                return Ok(None);
            }
            let input = match event::read()? {
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
