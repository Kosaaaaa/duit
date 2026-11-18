use std::fmt::Write as _;

use crate::ui::{FmtSize, shorten, to_utf8};

/// What a scan or an import is doing, as `global.state` in `sink.zig`.
#[derive(Debug, Clone, Copy)]
pub enum ScanState<'a> {
    /// The entries are being read, with the counters of a [`duit_model::Progress`].
    Running {
        files: u64,
        bytes: u64,
        directory: Option<&'a [u8]>,
    },
    /// The tree in memory adds the sizes of its hard links.
    CountingHardLinks,
    /// The scan or the import is done.
    Done,
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
            ScanState::Done => {}
        }
        output.into_bytes()
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
            },
            ScanState::Running {
                files: 1234,
                bytes: 1 << 30,
                directory: Some(b"/root/sub"),
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
            },
            ScanState::Running {
                files: 2,
                bytes: 0,
                directory: None,
            },
        ];
        assert_snapshot!(draws(&mut Console::new(false), &states, false), @r"
        0 files /   0.0   B\n  #1: /\xFFaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa...aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n
        2 files /   0.0   B\n  #1: (waiting)\n
        ");
    }
}
