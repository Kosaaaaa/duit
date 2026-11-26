//! Integration tests for duit's terminal UI.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};

mod browser;
mod progress;

/// The text of each line of the screen of `terminal`, quoted, then one line of attributes per
/// line: `R` for reverse video, `B` for bold, `#` for both, and `.` for neither. When a cell has a
/// color, the foreground and the background colors of each line follow, as one [`color_code`] per
/// cell. The version of duit is replaced by `[VERSION]` so that snapshots survive a version bump.
fn render(terminal: &Terminal<TestBackend>) -> String {
    let buffer = terminal.backend().buffer();
    let (width, height) = (buffer.area.width, buffer.area.height);
    let mut screen = terminal
        .backend()
        .to_string()
        .replace(env!("CARGO_PKG_VERSION"), "[VERSION]");
    for row in 0..height {
        for column in 0..width {
            let modifier = buffer[(column, row)].modifier;
            screen.push(
                match (
                    modifier.contains(Modifier::REVERSED),
                    modifier.contains(Modifier::BOLD),
                ) {
                    (true, true) => '#',
                    (true, false) => 'R',
                    (false, true) => 'B',
                    (false, false) => '.',
                },
            );
        }
        screen.push('\n');
    }
    if buffer
        .content
        .iter()
        .any(|cell| cell.fg != Color::Reset || cell.bg != Color::Reset)
    {
        for background in [false, true] {
            for row in 0..height {
                for column in 0..width {
                    let cell = &buffer[(column, row)];
                    screen.push(color_code(if background { cell.bg } else { cell.fg }));
                }
                screen.push('\n');
            }
        }
    }
    screen
}

/// One letter for each of the eight colors of ncurses, and `.` for the default color of the
/// terminal.
fn color_code(color: Color) -> char {
    match color {
        Color::Reset => '.',
        Color::Black => 'k',
        Color::Red => 'r',
        Color::Green => 'g',
        Color::Yellow => 'y',
        Color::Blue => 'b',
        Color::Magenta => 'm',
        Color::Cyan => 'c',
        Color::Gray => 'w',
        Color::DarkGray
        | Color::LightRed
        | Color::LightGreen
        | Color::LightYellow
        | Color::LightBlue
        | Color::LightMagenta
        | Color::LightCyan
        | Color::White
        | Color::Rgb(..)
        | Color::Indexed(_) => '?',
    }
}
