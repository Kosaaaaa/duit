use std::fmt::Write as _;
use std::time::Duration;

use duit_tui::{Action, BrowserOptions, ColorScheme, Key, ScanScreen, ScanState};
use insta::assert_snapshot;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::render;

/// The default update delay, under 1 s, with which the screen of a scan animates.
const UPDATE_DELAY: Duration = Duration::from_millis(100);

/// A scan that has read 1234 entries of 1 GiB, and reads `/root/sub`.
const RUNNING: ScanState<'static> = ScanState::Running {
    files: 1234,
    bytes: 1 << 30,
    directory: Some(b"/root/sub"),
    last_error: None,
};

/// Draw `state` with `screen` on a terminal of `width` by `height`.
fn draw(
    screen: &mut ScanScreen,
    state: &ScanState<'_>,
    options: &BrowserOptions,
    (width, height): (u16, u16),
) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal opens");
    terminal
        .draw(|frame| screen.draw(frame, state, options))
        .expect("screen draws");
    terminal
}

/// The first screen of a scan, in a box at the center of the terminal: the counters, the
/// directory, the key that aborts, and the first step of the animation, as `drawProgress` in
/// `sink.zig` draws them. Upstream has no test for `drawProgress`.
#[test]
fn running() {
    let mut screen = ScanScreen::new(UPDATE_DELAY);
    let terminal = draw(&mut screen, &RUNNING, &BrowserOptions::default(), (80, 24));
    assert_snapshot!(render(&terminal), @r#"
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "   ┌── Scanning... ──────────────────────────────────────────────────────────┐  "
    "   │                                                                         │  "
    "   │ Total items: 1,234          size:   1.0 GiB                             │  "
    "   │   /root/sub                                                             │  "
    "   │                                                                         │  "
    "   │                                                                         │  "
    "   │                                                                         │  "
    "   │ Sc                                                     Press q to abort │  "
    "   └─────────────────────────────────────────────────────────────────────────┘  "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ......BBBBBBBBBBBBB.............................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ..................................................................B.............
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    "#);
}

/// After a read error, a warning names the path that failed. The directory and the path are
/// converted to UTF-8 and shortened to the box, in the colors of `--color dark`.
#[test]
fn running_error() {
    let directory = [b"/\xff".as_slice(), &[b'd'; 100]].concat();
    let error = [b"/root/".as_slice(), &[b'e'; 100]].concat();
    let state = ScanState::Running {
        files: 5,
        bytes: 20480,
        directory: Some(&directory),
        last_error: Some(&error),
    };
    let options = BrowserOptions {
        color: ColorScheme::Dark,
        ..BrowserOptions::default()
    };
    let mut screen = ScanScreen::new(UPDATE_DELAY);
    let terminal = draw(&mut screen, &state, &options, (80, 9));
    assert_snapshot!(render(&terminal), @r#"
    "   ┌── Scanning... ──────────────────────────────────────────────────────────┐  "
    "   │                                                                         │  "
    "   │ Total items: 5              size:  20.0 KiB                             │  "
    "   │   /\xFFdddddddddddddddddddddddddddd...ddddddddddddddddddddddddddddddddd │  "
    "   │                                                                         │  "
    "   │ Warning: error scanning /root/eeeeeeeeeeeeeeee...eeeeeeeeeeeeeeeeeeeeee │  "
    "   │  some directory sizes may not be correct.                               │  "
    "   │ Sc                                                     Press q to abort │  "
    "   └─────────────────────────────────────────────────────────────────────────┘  "
    ......BBBBBBBBBBBBB.............................................................
    ................................................................................
    ..................B....................BBBBB....................................
    ................................................................................
    ................................................................................
    .....BBBBBBBBB..................................................................
    ................................................................................
    ..................................................................B.............
    ................................................................................
    ......bbbbbbbbbbbbb.............................................................
    ................................................................................
    ..................y....................yyyyy....................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ..................................................................y.............
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    "#);
}

/// The size is shown in a box wider than 48 columns, which is 5 columns narrower than the
/// terminal, and the animation in one wider than 40 with an update delay under 1 s, so not with
/// `-q`. `(waiting)` stands for no directory, and the thousands separator is the locale's.
#[test]
fn widths() {
    let state = ScanState::Running {
        files: 1_234_567,
        bytes: 4096,
        directory: None,
        last_error: None,
    };
    let options = BrowserOptions {
        si: true,
        thousands_separator: ".".to_string(),
        ..BrowserOptions::default()
    };
    let mut rendered = String::new();
    for (width, update_delay) in [
        (45, UPDATE_DELAY),
        (46, UPDATE_DELAY),
        (53, UPDATE_DELAY),
        (54, UPDATE_DELAY),
        (54, Duration::from_secs(1)),
    ] {
        let mut screen = ScanScreen::new(update_delay);
        let terminal = draw(&mut screen, &state, &options, (width, 9));
        let _ = writeln!(rendered, "{width} columns, {update_delay:?}:");
        rendered.push_str(&terminal.backend().to_string());
    }
    assert_snapshot!(rendered, @r#"
    45 columns, 100ms:
    "  ┌── Scanning... ───────────────────────┐   "
    "  │                                      │   "
    "  │ Total items: 1.234.567               │   "
    "  │   (waiting)                          │   "
    "  │                                      │   "
    "  │                                      │   "
    "  │                                      │   "
    "  │                     Press q to abort │   "
    "  └──────────────────────────────────────┘   "
    46 columns, 100ms:
    "   ┌── Scanning... ────────────────────────┐  "
    "   │                                       │  "
    "   │ Total items: 1.234.567                │  "
    "   │   (waiting)                           │  "
    "   │                                       │  "
    "   │                                       │  "
    "   │                                       │  "
    "   │ Sc                   Press q to abort │  "
    "   └───────────────────────────────────────┘  "
    53 columns, 100ms:
    "  ┌── Scanning... ───────────────────────────────┐   "
    "  │                                              │   "
    "  │ Total items: 1.234.567                       │   "
    "  │   (waiting)                                  │   "
    "  │                                              │   "
    "  │                                              │   "
    "  │                                              │   "
    "  │ Sc                          Press q to abort │   "
    "  └──────────────────────────────────────────────┘   "
    54 columns, 100ms:
    "   ┌── Scanning... ────────────────────────────────┐  "
    "   │                                               │  "
    "   │ Total items: 1.234.567      size:   4.1 kB    │  "
    "   │   (waiting)                                   │  "
    "   │                                               │  "
    "   │                                               │  "
    "   │                                               │  "
    "   │ Sc                           Press q to abort │  "
    "   └───────────────────────────────────────────────┘  "
    54 columns, 1s:
    "   ┌── Scanning... ────────────────────────────────┐  "
    "   │                                               │  "
    "   │ Total items: 1.234.567      size:   4.1 kB    │  "
    "   │   (waiting)                                   │  "
    "   │                                               │  "
    "   │                                               │  "
    "   │                                               │  "
    "   │                              Press q to abort │  "
    "   └───────────────────────────────────────────────┘  "
    "#);
}

/// Each draw moves the animation one step: "Scanning..." is written from the left, then erased
/// from the left, in 22 steps.
#[test]
fn animation() {
    let mut screen = ScanScreen::new(UPDATE_DELAY);
    let mut rows = String::new();
    for _ in 0..23 {
        let terminal = draw(&mut screen, &RUNNING, &BrowserOptions::default(), (60, 9));
        let buffer = terminal.backend().buffer();
        let row: String = (0..20).map(|column| buffer[(column, 7)].symbol()).collect();
        let _ = writeln!(rows, "{row:?}");
    }
    assert_snapshot!(rows, @r#"
    "   │ Sc             "
    "   │ Sca            "
    "   │ Scan           "
    "   │ Scann          "
    "   │ Scanni         "
    "   │ Scannin        "
    "   │ Scanning       "
    "   │ Scanning.      "
    "   │ Scanning..     "
    "   │ Scanning...    "
    "   │  canning...    "
    "   │   anning...    "
    "   │    nning...    "
    "   │     ning...    "
    "   │      ing...    "
    "   │       ng...    "
    "   │        g...    "
    "   │         ...    "
    "   │          ..    "
    "   │           .    "
    "   │                "
    "   │ S              "
    "   │ Sc             "
    "#);
}

/// `q` aborts the scan, and with `--confirm-quit` only after `y` or `Y`. Another key, or a
/// second `q`, cancels the confirmation, as `keyInput` in `sink.zig` handles them.
#[test]
fn quit() {
    let mut screen = ScanScreen::new(UPDATE_DELAY);
    let options = BrowserOptions::default();
    assert_eq!(screen.key_input(&RUNNING, Key::Char('x'), &options), None);
    assert_eq!(screen.key_input(&RUNNING, Key::Char('y'), &options), None);
    assert_eq!(
        screen.key_input(&RUNNING, Key::Char('q'), &options),
        Some(Action::Quit)
    );

    let options = BrowserOptions {
        confirm_quit: true,
        ..BrowserOptions::default()
    };
    assert_eq!(screen.key_input(&RUNNING, Key::Char('q'), &options), None);
    assert_eq!(screen.key_input(&RUNNING, Key::Char('q'), &options), None);
    assert_eq!(screen.key_input(&RUNNING, Key::Char('y'), &options), None);
    assert_eq!(screen.key_input(&RUNNING, Key::Char('q'), &options), None);
    assert_eq!(screen.key_input(&RUNNING, Key::Down, &options), None);
    assert_eq!(screen.key_input(&RUNNING, Key::Char('Y'), &options), None);
    assert_eq!(screen.key_input(&RUNNING, Key::Char('q'), &options), None);
    let terminal = draw(&mut screen, &RUNNING, &options, (60, 9));
    assert_snapshot!(terminal.backend(), @r#"
    "   ┌── Scanning... ──────────────────────────────────────┐  "
    "   │                                                     │  "
    "   │ Total items: 1,234          size:   1.0 GiB         │  "
    "   │   /root/sub                                         │  "
    "   │                                                     │  "
    "   │                                                     │  "
    "   │                                                     │  "
    "   │ Sc                               Press y to confirm │  "
    "   └─────────────────────────────────────────────────────┘  "
    "#);
    assert_eq!(
        screen.key_input(&RUNNING, Key::Char('Y'), &options),
        Some(Action::Quit)
    );
}

/// A refresh that cannot open its directory waits for any key, then returns to the browser, as
/// `drawError` and `keyInput` in `sink.zig` do. The path is converted to UTF-8 and shortened. With
/// `--color dark-bg`, the screen around the box is erased to the background of the default style.
#[test]
fn error() {
    let path = [b"/\xff".as_slice(), &[b'a'; 100]].concat();
    let state = ScanState::Error { path: &path };
    let mut screen = ScanScreen::new(UPDATE_DELAY);
    let options = BrowserOptions {
        color: ColorScheme::DarkBackground,
        ..BrowserOptions::default()
    };
    let terminal = draw(&mut screen, &state, &options, (60, 6));
    assert_snapshot!(render(&terminal), @r#"
    "   ┌── Scan error ───────────────────────────────────────┐  "
    "   │                                                     │  "
    "   │ Unable to open directory:                           │  "
    "   │   /\xFFaaaaaaaaaaaaaaaa...aaaaaaaaaaaaaaaaaaaaa     │  "
    "   │                           Press any key to continue │  "
    "   └─────────────────────────────────────────────────────┘  "
    ......BBBBBBBBBBBB..........................................
    ............................................................
    ............................................................
    ............................................................
    ............................................................
    ............................................................
    wwwwwwbbbbbbbbbbbbwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    wwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    wwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    wwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    wwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    wwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    "#);
    assert_eq!(
        screen.key_input(&state, Key::Other, &options),
        Some(Action::Browse)
    );
}

/// While the counts of a refreshed directory are cleared and while the hard links are counted,
/// the screen says so, and every key is ignored. Once the scan is done, nothing is drawn.
#[test]
fn steps() {
    let mut screen = ScanScreen::new(UPDATE_DELAY);
    let options = BrowserOptions::default();
    let mut rendered = String::new();
    for state in [
        ScanState::Zeroing,
        ScanState::CountingHardLinks,
        ScanState::Done,
    ] {
        let terminal = draw(&mut screen, &state, &options, (40, 4));
        let _ = write!(rendered, "{state:?}:\n{}", terminal.backend());
        for key in [Key::Char('q'), Key::Char('y'), Key::Enter] {
            assert_eq!(screen.key_input(&state, key, &options), None, "{state:?}");
        }
    }
    assert_snapshot!(rendered, @r#"
    Zeroing:
    "   ┌── Initializing ─────────────────┐  "
    "   │                                 │  "
    "   │ Clearing directory counts...    │  "
    "   └─────────────────────────────────┘  "
    CountingHardLinks:
    "   ┌── Finalizing ───────────────────┐  "
    "   │                                 │  "
    "   │ Counting hardlinks...           │  "
    "   └─────────────────────────────────┘  "
    Done:
    "                                        "
    "                                        "
    "                                        "
    "                                        "
    "#);
}

/// The box of `drawMessage` in `sink.zig`, which ncdu 2.9.2 does not call.
#[test]
fn message() {
    let mut terminal = Terminal::new(TestBackend::new(40, 4)).expect("terminal opens");
    terminal
        .draw(|frame| {
            ScanScreen::draw_message(frame, "Something went wrong", &BrowserOptions::default());
        })
        .expect("message draws");
    assert_snapshot!(render(&terminal), @r#"
    "   ┌── Scan error ───────────────────┐  "
    "   │                                 │  "
    "   │ Something went wrong            │  "
    "   └─────────────────────────────────┘  "
    ......BBBBBBBBBBBB......................
    ........................................
    ........................................
    ........................................
    "#);
}
