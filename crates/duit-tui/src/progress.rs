use std::fmt::Write as _;
use std::time::Duration;

use ratatui::Frame;
use ratatui::buffer::Buffer;

use crate::browser::BrowserOptions;
use crate::terminal::Key;
use crate::ui::{FmtSize, Style, Window, shorten, to_utf8};

/// What a scan or an import is doing, as `global.state` in `sink.zig`.
#[derive(Debug, Clone, Copy)]
pub enum ScanState<'a> {
    /// The entries are being read, with the counters of a [`duit_model::Progress`].
    Running {
        files: u64,
        bytes: u64,
        directory: Option<&'a [u8]>,
        last_error: Option<&'a [u8]>,
    },
    /// A refresh clears the counts of the directory that it scans again, `zeroing`.
    Zeroing,
    /// The tree in memory adds the sizes of its hard links.
    CountingHardLinks,
    /// A refresh could not open the directory at `path`, and waits for a key, `err`.
    Error { path: &'a [u8] },
    /// The scan or the import is done.
    Done,
}

/// What the main loop does after a key on the scan screen, from [`ScanScreen::key_input`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanKey {
    /// Abort the scan and quit.
    Quit,
    /// Close the error of a refresh and keep browsing.
    Close,
}

/// The progress lines of `-1` on standard error, `drawConsole` in `sink.zig`.
#[derive(Debug)]
pub struct Console {
    /// Whether standard error takes ANSI escapes, so that each draw can erase the lines before it.
    ansi: bool,
    /// The number of lines that the last draw wrote.
    lines: usize,
}

impl Console {
    /// Progress lines for a standard error that takes ANSI escapes when `ansi` is set.
    pub fn new(ansi: bool) -> Self {
        Self { ansi, lines: 0 }
    }

    /// The bytes that erase the lines of the last draw and write those of `state`, with sizes in
    /// SI units if `si` is set.
    pub fn draw(&mut self, state: &ScanState<'_>, si: bool) -> Vec<u8> {
        let mut output = String::new();
        if self.ansi {
            output.push_str(&"\x1b[1F\x1b[2K".repeat(self.lines));
            self.lines = 0;
        }
        match *state {
            ScanState::Running {
                files,
                bytes,
                directory,
                last_error: _,
            } => {
                let size = FmtSize::new(bytes, si);
                let directory = to_utf8(directory.unwrap_or(b"(waiting)"));
                let _ = write!(
                    output,
                    "{files} files / {}{}\n  #1: {}\n",
                    size.number,
                    size.unit,
                    shorten(&directory, 73)
                );
                self.lines += 2;
            }
            ScanState::CountingHardLinks => {
                output.push_str("Counting hardlinks...\n");
                self.lines += 1;
            }
            ScanState::Zeroing | ScanState::Error { .. } | ScanState::Done => {}
        }
        output.into_bytes()
    }
}

/// The full-screen progress of `-2`, the `.full` branch of `draw` in `sink.zig` with
/// `drawProgress` and `drawError`, and `keyInput`.
#[derive(Debug)]
pub struct ScanScreen {
    /// Whether the update delay is under 1 s, so that the screen animates.
    animate: bool,
    /// The step of the animation, `animation_pos` in `drawProgress`.
    animation: usize,
    /// `q` was pressed with `--confirm-quit`, `need_confirm_quit` in `sink.zig`.
    confirming_quit: bool,
}

impl ScanScreen {
    /// A progress screen that animates when `update_delay` is under 1 s.
    pub fn new(update_delay: Duration) -> Self {
        Self {
            animate: update_delay < Duration::from_secs(1),
            animation: 0,
            confirming_quit: false,
        }
    }

    /// Draw `state` on all of `frame`, which is erased first.
    pub fn draw(&mut self, frame: &mut Frame<'_>, state: &ScanState<'_>, options: &BrowserOptions) {
        let width = frame.area().width.saturating_sub(5);
        let buffer = frame.buffer_mut();
        let scheme = options.color;
        // ncurses erases the screen to the background of the `default` style before each frame.
        buffer.set_style(buffer.area, scheme.style(Style::Default));
        match *state {
            ScanState::Running {
                files,
                bytes,
                directory,
                last_error,
            } => self.draw_progress(buffer, files, bytes, directory, last_error, options),
            ScanState::Zeroing => {
                let window = Window::new(buffer, scheme, 4, width, "Initializing");
                window.text(buffer, 2, 2, "Clearing directory counts...", Style::Default);
            }
            ScanState::CountingHardLinks => {
                let window = Window::new(buffer, scheme, 4, width, "Finalizing");
                window.text(buffer, 2, 2, "Counting hardlinks... ", Style::Default);
            }
            ScanState::Error { path } => {
                let window = Window::new(buffer, scheme, 6, width, "Scan error");
                window.text(buffer, 2, 2, "Unable to open directory:", Style::Default);
                let path = to_utf8(path);
                let path = shorten(&path, width.saturating_sub(10).into());
                window.text(buffer, 3, 4, &path, Style::Default);
                let x = width.saturating_sub(27);
                window.text(buffer, 4, x, "Press any key to continue", Style::Default);
            }
            ScanState::Done => {}
        }
    }

    /// The counters, the directory, the last error, the key that aborts, and the animation,
    /// `drawProgress` in `sink.zig` with one thread.
    fn draw_progress(
        &mut self,
        buffer: &mut Buffer,
        files: u64,
        bytes: u64,
        directory: Option<&[u8]>,
        last_error: Option<&[u8]>,
        options: &BrowserOptions,
    ) {
        let width = buffer.area.width.saturating_sub(5);
        let window = Window::new(buffer, options.color, 9, width, "Scanning...");
        let x = window.text(buffer, 2, 2, "Total items: ", Style::Default);
        window.number(buffer, 2, x, files, &options.thousands_separator);
        if width > 48 {
            let x = window.text(buffer, 2, 30, "size: ", Style::Default);
            window.size(buffer, 2, x, bytes, options.si);
        }
        let directory = to_utf8(directory.unwrap_or(b"(waiting)"));
        let directory = shorten(&directory, width.saturating_sub(6).into());
        window.text(buffer, 3, 4, &directory, Style::Default);
        if let Some(path) = last_error {
            let x = window.text(buffer, 5, 2, "Warning: ", Style::Bold);
            let x = window.text(buffer, 5, x, "error scanning ", Style::Default);
            let path = to_utf8(path);
            let path = shorten(&path, width.saturating_sub(28).into());
            window.text(buffer, 5, x, &path, Style::Default);
            window.text(
                buffer,
                6,
                3,
                "some directory sizes may not be correct.",
                Style::Default,
            );
        }
        let (x, key, action) = if self.confirming_quit {
            (width.saturating_sub(20), "y", " to confirm")
        } else {
            (width.saturating_sub(18), "q", " to abort")
        };
        let x = window.text(buffer, 7, x, "Press ", Style::Default);
        let x = window.text(buffer, 7, x, key, Style::Key);
        window.text(buffer, 7, x, action, Style::Default);
        if self.animate && width > 40 {
            const TEXT: &str = "Scanning...";
            self.animation += 1;
            if self.animation >= TEXT.len() * 2 {
                self.animation = 0;
            }
            // The text is written from the left, then erased from the left.
            let (start, end) = if self.animation < TEXT.len() {
                (0, self.animation + 1)
            } else {
                (self.animation - TEXT.len() + 1, TEXT.len())
            };
            if let Some(text) = TEXT.get(start..end) {
                let column = u16::try_from(start).map_or(u16::MAX, |start| start + 2);
                window.text(buffer, 7, column, text, Style::Default);
            }
        }
    }

    /// Handle `key` in `state`, `keyInput` in `sink.zig`. Returns [`ScanKey::Quit`] once the scan is
    /// aborted, and [`ScanKey::Close`] once the error of a refresh is closed.
    pub fn key_input(
        &mut self,
        state: &ScanState<'_>,
        key: Key,
        options: &BrowserOptions,
    ) -> Option<ScanKey> {
        match state {
            ScanState::Running { .. } => match key {
                Key::Char('q') if options.confirm_quit => {
                    self.confirming_quit = !self.confirming_quit;
                    None
                }
                Key::Char('q') => Some(ScanKey::Quit),
                Key::Char('y' | 'Y') if self.confirming_quit => Some(ScanKey::Quit),
                Key::Char('y' | 'Y') => None,
                Key::Char(_)
                | Key::Enter
                | Key::Backspace
                | Key::Up
                | Key::Down
                | Key::Left
                | Key::Right
                | Key::Home
                | Key::End
                | Key::PageUp
                | Key::PageDown
                | Key::Other => {
                    self.confirming_quit = false;
                    None
                }
            },
            ScanState::Error { .. } => Some(ScanKey::Close),
            ScanState::Zeroing | ScanState::CountingHardLinks { .. } | ScanState::Done => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use insta::assert_snapshot;

    use super::{Console, ScanState};

    /// The bytes that `console` writes for each of `states`, one line each, with ESC as `^[` and
    /// newlines as `\n`.
    fn draws(console: &mut Console, states: &[ScanState<'_>], si: bool) -> String {
        let mut output = String::new();
        for state in states {
            let bytes = console.draw(state, si);
            output.push_str(
                &String::from_utf8(bytes)
                    .expect("progress is UTF-8")
                    .replace('\x1b', "^[")
                    .replace('\n', "\\n"),
            );
            output.push('\n');
        }
        output
    }

    /// Each draw erases the lines of the one before it, when the terminal takes ANSI escapes, as
    /// `drawConsole` in `sink.zig` does. Once the scan is done, the lines are erased.
    #[test]
    fn erase() {
        let states = [
            ScanState::Running {
                files: 1,
                bytes: 4096,
                directory: Some(b"/root"),
                last_error: None,
            },
            ScanState::Running {
                files: 1234,
                bytes: 1 << 30,
                directory: Some(b"/root/sub"),
                last_error: None,
            },
            ScanState::CountingHardLinks,
            ScanState::Done,
        ];
        assert_snapshot!(draws(&mut Console::new(true), &states, false), @r"
        1 files /   4.0 KiB\n  #1: /root\n
        ^[[1F^[[2K^[[1F^[[2K1234 files /   1.0 GiB\n  #1: /root/sub\n
        ^[[1F^[[2K^[[1F^[[2KCounting hardlinks...\n
        ^[[1F^[[2K
        ");
        assert_snapshot!(draws(&mut Console::new(false), &states, true), @r"
        1 files /   4.1 kB\n  #1: /root\n
        1234 files /   1.1 GB\n  #1: /root/sub\n
        Counting hardlinks...\n
        ");
    }

    /// The directory is converted to UTF-8 and shortened to 73 columns, and `(waiting)` stands
    /// for none.
    #[test]
    fn directory() {
        let long = [b"/\xff".as_slice(), &[b'a'; 100]].concat();
        let states = [
            ScanState::Running {
                files: 0,
                bytes: 0,
                directory: Some(&long),
                last_error: None,
            },
            ScanState::Running {
                files: 2,
                bytes: 0,
                directory: None,
                last_error: None,
            },
        ];
        assert_snapshot!(draws(&mut Console::new(false), &states, false), @r"
        0 files /   0.0   B\n  #1: /\xFFaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa...aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n
        2 files /   0.0   B\n  #1: (waiting)\n
        ");
    }
}
