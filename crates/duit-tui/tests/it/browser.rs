use std::fmt::Write as _;

use duit_format::import;
use duit_model::{DirectoryId, EntryKind, MemorySink, Tree};
use duit_tui::{Browser, BrowserOptions, GraphStyle, Key, SharedColumn, SortColumn, SortOrder};
use insta::assert_snapshot;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Modifier;

/// The JSON export that `crates/duit/tests/fixtures/generate.sh` converts with ncdu. It has every
/// kind of entry.
const SOURCE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../duit/tests/fixtures/source.json"
));

/// A tree with hidden entries: the largest file, a backup file, a directory that shares a hard
/// link with the root, and excluded entries. The other entries include a long name.
const HIDDEN: &str = r#"[1,2,{"progname":"ncdu","progver":"2.9.2"},
[{"name":"/data","asize":4096,"dsize":4096},
{"name":".big","asize":40960,"dsize":40960},
{"name":"backup~","asize":8192,"dsize":8192},
{"name":"notes.txt","asize":10240,"dsize":10240},
{"name":"a-very-long-file-name.txt","asize":2048,"dsize":4096},
[{"name":"photos","asize":4096,"dsize":4096},{"name":"a.jpg","asize":20480,"dsize":20480}],
[{"name":".config","asize":4096,"dsize":4096},{"name":"settings","asize":512,"dsize":4096,"ino":7,"hlnkc":true,"nlink":2}],
{"name":"settings-link","asize":512,"dsize":4096,"ino":7,"hlnkc":true,"nlink":2},
{"name":"node_modules","excluded":"pattern"},
{"name":"mnt","excluded":"otherfs"},
{"name":"proc","excluded":"kernfs"}]]"#;

/// Files whose sizes fill the bars of the graph in steps of an eighth.
const GRAPH: &str = r#"[1,2,{"progname":"ncdu","progver":"2.9.2"},
[{"name":"/graph","asize":0,"dsize":0},
{"name":"a","asize":45056,"dsize":45056},
{"name":"b","asize":30720,"dsize":30720},
{"name":"c","asize":12288,"dsize":12288},
{"name":"d","asize":4096,"dsize":4096},
{"name":"e","asize":512,"dsize":512},
{"name":"f","asize":0,"dsize":0}]]"#;

/// Entries with extended information, at 10:00 UTC on different days so that the dates are the
/// same in most time zones. The directory `old` is older than its file, and `nomtime` has extended
/// information without an mtime.
const MTIME: &str = r#"[1,2,{"progname":"ncdu","progver":"2.9.2"},
[{"name":"/mtime","asize":4096,"dsize":4096,"mtime":1704103200},
[{"name":"old","asize":4096,"dsize":4096,"mtime":1704189600},{"name":"new.txt","asize":100,"dsize":4096,"mtime":1704362400}],
{"name":"early.txt","asize":100,"dsize":4096,"mtime":1704276000},
{"name":"latest.txt","asize":100,"dsize":4096,"mtime":1704535200},
{"name":"nomtime","asize":100,"dsize":4096,"uid":1000},
{"name":"plain","asize":100,"dsize":4096}]]"#;

/// Names that sort differently in natural and in byte order, a directory, and two excluded
/// entries, of which only the one on another filesystem is shown as a directory.
const NAMES: &str = r#"[1,2,{"progname":"ncdu","progver":"2.9.2"},
[{"name":"/names","asize":0,"dsize":0},
{"name":"file10","asize":10,"dsize":4096},
{"name":"file9","asize":10,"dsize":4096},
{"name":"File2","asize":10,"dsize":4096},
[{"name":"dir","asize":10,"dsize":4096}],
{"name":"zfs","excluded":"otherfs"},
{"name":"apattern","excluded":"pattern"}]]"#;

/// Eleven hard links to one inode, three of them in a subdirectory, one with a long name.
const LINKS: &str = r#"[1,2,{"progname":"ncdu","progver":"2.9.2"},
[{"name":"/links","asize":4096,"dsize":4096},
{"name":"a","asize":100,"dsize":4096,"ino":9,"hlnkc":true,"nlink":11},
{"name":"b","asize":100,"dsize":4096,"ino":9,"hlnkc":true,"nlink":11},
{"name":"c","asize":100,"dsize":4096,"ino":9,"hlnkc":true,"nlink":11},
{"name":"d","asize":100,"dsize":4096,"ino":9,"hlnkc":true,"nlink":11},
{"name":"e","asize":100,"dsize":4096,"ino":9,"hlnkc":true,"nlink":11},
{"name":"f","asize":100,"dsize":4096,"ino":9,"hlnkc":true,"nlink":11},
{"name":"g","asize":100,"dsize":4096,"ino":9,"hlnkc":true,"nlink":11},
{"name":"h","asize":100,"dsize":4096,"ino":9,"hlnkc":true,"nlink":11},
[{"name":"sub","asize":4096,"dsize":4096},
{"name":"y","asize":100,"dsize":4096,"ino":9,"hlnkc":true,"nlink":11},
{"name":"a-file-name-that-is-long-enough-to-be-shortened-in-the-window","asize":100,"dsize":4096,"ino":9,"hlnkc":true,"nlink":11},
{"name":"x","asize":100,"dsize":4096,"ino":9,"hlnkc":true,"nlink":11}]]]"#;

/// Import the JSON export `input` and return the tree.
fn tree_of(input: &[u8], extended: bool) -> Tree {
    let mut sink = MemorySink::new(extended);
    import(&mut sink, input, &[]).expect("input is imported");
    sink.finish()
}

/// A JSON export whose root holds a directory with each number of files in `counts`, and one
/// file. Every entry is empty.
fn item_counts(counts: &[(&str, usize)]) -> String {
    let mut export =
        String::from(r#"[1,2,{"progname":"ncdu","progver":"2.9.2"},[{"name":"/items"},"#);
    for (name, count) in counts {
        let _ = write!(export, r#"[{{"name":"{name}"}}"#);
        for index in 0..*count {
            let _ = write!(export, r#",{{"name":"{index}"}}"#);
        }
        export.push_str("],");
    }
    export.push_str(r#"{"name":"file"}]]"#);
    export
}

/// The root of `tree`.
fn root(tree: &Tree) -> DirectoryId {
    tree.root().expect("tree has a root")
}

/// The directory `name` of the root of `tree`.
fn subdirectory(tree: &Tree, name: &[u8]) -> DirectoryId {
    tree.entries(root(tree))
        .find(|&entry| tree.name(entry) == name)
        .and_then(|entry| match tree.entry(entry).kind {
            EntryKind::Directory(directory) => Some(directory),
            EntryKind::HardLink(_)
            | EntryKind::Regular
            | EntryKind::NonRegular
            | EntryKind::Special(_) => None,
        })
        .expect("directory exists")
}

/// The options of an import, which ncdu marks in the header.
fn imported() -> BrowserOptions {
    BrowserOptions {
        imported: true,
        ..BrowserOptions::default()
    }
}

/// Open `directory` of `tree` on a terminal of `width` by `height`, and draw it again after each
/// of `keys`, as ncdu does between key presses. Returns the text of each line of the last screen,
/// quoted, then one line of attributes per line: `R` for reverse video, `B` for bold, `#` for
/// both, and `.` for neither.
fn screen(
    tree: &Tree,
    directory: DirectoryId,
    options: &BrowserOptions,
    keys: &[Key],
    (width, height): (u16, u16),
) -> String {
    let mut options = options.clone();
    let mut browser = Browser::new(tree, directory, &options);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal opens");
    terminal
        .draw(|frame| browser.draw(frame, tree, &options))
        .expect("browser draws");
    for &key in keys {
        browser.key_input(tree, &mut options, key, height);
        terminal
            .draw(|frame| browser.draw(frame, tree, &options))
            .expect("browser draws");
    }
    let buffer = terminal.backend().buffer();
    let mut screen = terminal.backend().to_string();
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
    screen
}

/// Replace the time of day and the offset of each timestamp in `screen`, which depend on the time
/// zone, and keep the date.
fn redact_times(screen: &str) -> String {
    const MASK: &[u8] = b"dddd-dd-dd dd:dd:dd +dddd";
    let mut screen = screen.as_bytes().to_vec();
    let mut start = 0;
    while start + MASK.len() <= screen.len() {
        let matches = screen[start..start + MASK.len()]
            .iter()
            .zip(MASK)
            .all(|(&byte, &mask)| match mask {
                b'd' => byte.is_ascii_digit(),
                b'+' => byte == b'+' || byte == b'-',
                _ => byte == mask,
            });
        if matches {
            screen[start + 11..start + MASK.len()].copy_from_slice(b"HH:MM:SS +ZZZZ");
            start += MASK.len();
        } else {
            start += 1;
        }
    }
    String::from_utf8(screen).expect("screen is UTF-8")
}

/// The default columns, sorted by disk usage: a flag for every kind of entry, the shared size of
/// the directory with a hard link outside of it, and the graph. Names are escaped and shortened.
#[test]
fn default_columns() {
    let tree = tree_of(SOURCE, false);
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &[], (80, 18)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 KiB S   4.0 KiB [###########] /src                                      "
    "!   4.0 KiB             [####       ] /unreadable                               "
    "e   4.0 KiB             [####       ] /empty                                    "
    "    4.0 KiB             [####       ]  README.md                                "
    "H   4.0 KiB             [####       ]  also-linked                              "
    "    0.0   B             [           ]  sparse                                   "
    "    0.0   B             [           ]  raw \xFF\xFE                             "
    "    0.0   B             [           ]  quote " backslash \...\x09 control \x01 é"
    "@   0.0   B             [           ]  socket                                   "
    "^   0.0   B             [           ] /proc                                     "
    "<   0.0   B             [           ]  node_modules                             "
    ">   0.0   B             [           ] /mnt                                      "
    "    0.0   B             [           ]  broken                                   "
    "                                                                                "
    "                                                                                "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
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
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// `--shared-column=unique` shows the size that only the directory holds, and the size of every
/// other entry as unique.
#[test]
fn shared_column_unique() {
    let tree = tree_of(SOURCE, false);
    let options = BrowserOptions {
        shared_column: SharedColumn::Unique,
        ..imported()
    };
    assert_snapshot!(screen(&tree, root(&tree), &options, &[], (80, 18)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 KiB U  12.0 KiB [###########] /src                                      "
    "!   4.0 KiB U   4.0 KiB [####       ] /unreadable                               "
    "e   4.0 KiB U   4.0 KiB [####       ] /empty                                    "
    "    4.0 KiB U   4.0 KiB [####       ]  README.md                                "
    "H   4.0 KiB U   4.0 KiB [####       ]  also-linked                              "
    "    0.0   B             [           ]  sparse                                   "
    "    0.0   B             [           ]  raw \xFF\xFE                             "
    "    0.0   B             [           ]  quote " backslash \...\x09 control \x01 é"
    "@   0.0   B             [           ]  socket                                   "
    "^   0.0   B             [           ] /proc                                     "
    "<   0.0   B             [           ]  node_modules                             "
    ">   0.0   B             [           ] /mnt                                      "
    "    0.0   B             [           ]  broken                                   "
    "                                                                                "
    "                                                                                "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
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
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// `--shared-column=off` drops the shared column.
#[test]
fn shared_column_off() {
    let tree = tree_of(SOURCE, false);
    let options = BrowserOptions {
        shared_column: SharedColumn::Off,
        ..imported()
    };
    assert_snapshot!(screen(&tree, root(&tree), &options, &[], (80, 18)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 KiB [###########] /src                                                  "
    "!   4.0 KiB [####       ] /unreadable                                           "
    "e   4.0 KiB [####       ] /empty                                                "
    "    4.0 KiB [####       ]  README.md                                            "
    "H   4.0 KiB [####       ]  also-linked                                          "
    "    0.0   B [           ]  sparse                                               "
    "    0.0   B [           ]  raw \xFF\xFE                                         "
    "    0.0   B [           ]  quote " backslash \ tab \x09 control \x01 é          "
    "@   0.0   B [           ]  socket                                               "
    "^   0.0   B [           ] /proc                                                 "
    "<   0.0   B [           ]  node_modules                                         "
    ">   0.0   B [           ] /mnt                                                  "
    "    0.0   B [           ]  broken                                               "
    "                                                                                "
    "                                                                                "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
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
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// `--si --apparent-size` shows apparent sizes in powers of 1000 in a narrower column, and the
/// footer marks the apparent size. The order stays by disk usage.
#[test]
fn si_apparent_size() {
    let tree = tree_of(SOURCE, false);
    let options = BrowserOptions {
        si: true,
        show_blocks: false,
        ..imported()
    };
    assert_snapshot!(screen(&tree, root(&tree), &options, &[], (80, 18)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    "    9.4 kB S 300.0  B [           ] /src                                        "
    "!   4.1 kB            [           ] /unreadable                                 "
    "e   4.1 kB            [           ] /empty                                      "
    "    1.2 kB            [           ]  README.md                                  "
    "H 300.0  B            [           ]  also-linked                                "
    "    1.0 MB            [###########]  sparse                                     "
    "    2.0  B            [           ]  raw \xFF\xFE                               "
    "    1.0  B            [           ]  quote " backslash \ tab \x09 control \x01 é"
    "@   0.0  B            [           ]  socket                                     "
    "^   0.0  B            [           ] /proc                                       "
    "<   0.0  B            [           ]  node_modules                               "
    ">   0.0  B            [           ] /mnt                                        "
    "    0.0  B            [           ]  broken                                     "
    "                                                                                "
    "                                                                                "
    " Total disk usage:  32.8 kB  *Apparent size:   1.1 MB   Items: 15               "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
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
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRR##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// Hidden entries are shown by default. The directory that shares a hard link widens the size
/// column, and the readonly mode is marked in the header.
#[test]
fn hidden_shown() {
    let tree = tree_of(HIDDEN.as_bytes(), false);
    let options = BrowserOptions {
        can_delete: false,
        ..BrowserOptions::default()
    };
    assert_snapshot!(screen(&tree, root(&tree), &options, &[], (80, 14)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [readonly]"
    "--- /data ----------------------------------------------------------------------"
    "   40.0 KiB             [###########]  .big                                     "
    "   24.0 KiB             [######     ] /photos                                   "
    "   10.0 KiB             [##         ]  notes.txt                                "
    "    8.0 KiB             [##         ]  backup~                                  "
    "    8.0 KiB S   4.0 KiB [##         ] /.config                                  "
    "    4.0 KiB             [#          ]  a-very-long-file-name.txt                "
    "H   4.0 KiB             [#          ]  settings-link                            "
    "^   0.0   B             [           ] /proc                                     "
    "<   0.0   B             [           ]  node_modules                             "
    ">   0.0   B             [           ] /mnt                                      "
    "                                                                                "
    "*Total disk usage:  98.0 KiB   Apparent size:  92.5 KiB   Items: 12             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
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
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// `--hide-hidden` hides names that start with `.` or end with `~`, and excluded entries. The
/// hidden directory with a shared size no longer widens the size column, but the hidden file
/// still scales the graph.
#[test]
fn hidden_hidden() {
    let tree = tree_of(HIDDEN.as_bytes(), false);
    let options = BrowserOptions {
        show_hidden: false,
        ..BrowserOptions::default()
    };
    assert_snapshot!(screen(&tree, root(&tree), &options, &[], (80, 9)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help                   "
    "--- /data ----------------------------------------------------------------------"
    "   24.0 KiB [######     ] /photos                                               "
    "   10.0 KiB [##         ]  notes.txt                                            "
    "    4.0 KiB [#          ]  a-very-long-file-name.txt                            "
    "H   4.0 KiB [#          ]  settings-link                                        "
    "                                                                                "
    "                                                                                "
    "*Total disk usage:  98.0 KiB   Apparent size:  92.5 KiB   Items: 12             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// `--show-percent --graph-style=eighth-block` shows the share of the directory and bars in
/// eighths.
#[test]
fn graph_eighth_block_percent() {
    let tree = tree_of(GRAPH.as_bytes(), false);
    let options = BrowserOptions {
        show_percent: true,
        graph_style: GraphStyle::EighthBlock,
        ..imported()
    };
    assert_snapshot!(screen(&tree, root(&tree), &options, &[], (80, 9)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /graph ---------------------------------------------------------------------"
    "   44.0 KiB [ 48.6% ███████████]  a                                             "
    "   30.0 KiB [ 33.1% ███████▌   ]  b                                             "
    "   12.0 KiB [ 13.3% ███        ]  c                                             "
    "    4.0 KiB [  4.4% █          ]  d                                             "
    "  512.0   B [  0.6% ▏          ]  e                                             "
    "    0.0   B [  0.0%            ]  f                                             "
    "*Total disk usage:  90.5 KiB   Apparent size:  90.5 KiB   Items: 6              "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// `--graph-style=half-block` fills bars in halves.
#[test]
fn graph_half_block() {
    let tree = tree_of(GRAPH.as_bytes(), false);
    let options = BrowserOptions {
        graph_style: GraphStyle::HalfBlock,
        ..imported()
    };
    assert_snapshot!(screen(&tree, root(&tree), &options, &[], (80, 9)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /graph ---------------------------------------------------------------------"
    "   44.0 KiB [███████████]  a                                                    "
    "   30.0 KiB [███████▌   ]  b                                                    "
    "   12.0 KiB [███        ]  c                                                    "
    "    4.0 KiB [█          ]  d                                                    "
    "  512.0   B [           ]  e                                                    "
    "    0.0   B [           ]  f                                                    "
    "*Total disk usage:  90.5 KiB   Apparent size:  90.5 KiB   Items: 6              "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// `--show-percent --hide-graph` shows the percentage alone.
#[test]
fn percent_without_graph() {
    let tree = tree_of(GRAPH.as_bytes(), false);
    let options = BrowserOptions {
        show_percent: true,
        show_graph: false,
        ..imported()
    };
    assert_snapshot!(screen(&tree, root(&tree), &options, &[], (80, 9)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /graph ---------------------------------------------------------------------"
    "   44.0 KiB [ 48.6%]  a                                                         "
    "   30.0 KiB [ 33.1%]  b                                                         "
    "   12.0 KiB [ 13.3%]  c                                                         "
    "    4.0 KiB [  4.4%]  d                                                         "
    "  512.0   B [  0.6%]  e                                                         "
    "    0.0   B [  0.0%]  f                                                         "
    "*Total disk usage:  90.5 KiB   Apparent size:  90.5 KiB   Items: 6              "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// `--show-itemcount --sort=itemcount` shows the number of items of each directory, with a
/// thousands separator from 1000 on, and nothing for a file.
#[test]
fn item_count() {
    let tree = tree_of(
        item_counts(&[("few", 3), ("thousands", 1234), ("many", 12345)]).as_bytes(),
        false,
    );
    let options = BrowserOptions {
        show_item_count: true,
        sort_column: SortColumn::Items,
        ..imported()
    };
    assert_snapshot!(screen(&tree, root(&tree), &options, &[], (80, 7)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /items ---------------------------------------------------------------------"
    "    0.0   B [           ] 12,345 /many                                          "
    "    0.0   B [           ]  1,234 /thousands                                     "
    "    0.0   B [           ]      3 /few                                           "
    "    0.0   B [           ]         file                                          "
    "*Total disk usage:   0.0   B   Apparent size:   0.0   B   Items: 13,586         "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// `--show-mtime --sort=mtime` shows the latest mtime of each entry, puts the entries without
/// extended information last, and `no mtime` where the mtime is unknown.
#[test]
fn mtime() {
    let tree = tree_of(MTIME.as_bytes(), true);
    let options = BrowserOptions {
        show_mtime: true,
        sort_column: SortColumn::Mtime,
        sort_order: SortOrder::Ascending,
        ..imported()
    };
    assert_snapshot!(redact_times(&screen(&tree, root(&tree), &options, &[], (80, 8))), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /mtime ---------------------------------------------------------------------"
    "    4.0 KiB [########   ]                   no mtime  nomtime                   "
    "    4.0 KiB [########   ]  2024-01-03 HH:MM:SS +ZZZZ  early.txt                 "
    "    8.0 KiB [###########]  2024-01-04 HH:MM:SS +ZZZZ /old                       "
    "    4.0 KiB [########   ]  2024-01-06 HH:MM:SS +ZZZZ  latest.txt                "
    "    4.0 KiB [########   ]                   no mtime  plain                     "
    "*Total disk usage:  28.0 KiB   Apparent size:   8.5 KiB   Items: 6              "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// The row of the parent directory shows the mtime of the directory that is open.
#[test]
fn mtime_parent_row() {
    let tree = tree_of(MTIME.as_bytes(), true);
    let options = BrowserOptions {
        show_mtime: true,
        ..imported()
    };
    let old = subdirectory(&tree, b"old");
    assert_snapshot!(redact_times(&screen(&tree, old, &options, &[], (80, 5))), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /mtime/old -----------------------------------------------------------------"
    "                           2024-01-04 HH:MM:SS +ZZZZ /..                        "
    "    4.0 KiB [###########]  2024-01-04 HH:MM:SS +ZZZZ  new.txt                   "
    "*Total disk usage:   8.0 KiB   Apparent size:   4.1 KiB   Items: 1              "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// `--sort=name --disable-natsort --group-directories-first` puts directories and directories on
/// another filesystem first, then sorts names byte by byte.
#[test]
fn sort_name_bytes_directories_first() {
    let tree = tree_of(NAMES.as_bytes(), false);
    let options = BrowserOptions {
        sort_column: SortColumn::Name,
        sort_order: SortOrder::Ascending,
        sort_natural: false,
        sort_directories_first: true,
        ..imported()
    };
    assert_snapshot!(screen(&tree, root(&tree), &options, &[], (80, 9)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /names ---------------------------------------------------------------------"
    "e   4.0 KiB [###########] /dir                                                  "
    ">   0.0   B [           ] /zfs                                                  "
    "    4.0 KiB [###########]  File2                                                "
    "<   0.0   B [           ]  apattern                                             "
    "    4.0 KiB [###########]  file10                                               "
    "    4.0 KiB [###########]  file9                                                "
    "*Total disk usage:  16.0 KiB   Apparent size:  40.0   B   Items: 6              "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// `--sort=name-desc` sorts names in reverse natural order, with directories among the files.
#[test]
fn sort_name_natural_descending() {
    let tree = tree_of(NAMES.as_bytes(), false);
    let options = BrowserOptions {
        sort_column: SortColumn::Name,
        sort_order: SortOrder::Descending,
        ..imported()
    };
    assert_snapshot!(screen(&tree, root(&tree), &options, &[], (80, 9)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /names ---------------------------------------------------------------------"
    ">   0.0   B [           ] /zfs                                                  "
    "    4.0 KiB [###########]  file10                                               "
    "    4.0 KiB [###########]  file9                                                "
    "e   4.0 KiB [###########] /dir                                                  "
    "<   0.0   B [           ]  apattern                                             "
    "    4.0 KiB [###########]  File2                                                "
    "*Total disk usage:  16.0 KiB   Apparent size:  40.0   B   Items: 6              "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// A subdirectory starts with the row of its parent, and its path joins the names. The cursor row
/// is in reverse video.
#[test]
fn subdirectory_cursor() {
    let tree = tree_of(SOURCE, false);
    let src = subdirectory(&tree, b"src");
    assert_snapshot!(screen(&tree, src, &imported(), &[Key::Down], (80, 6)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project/src -----------------------------------------------------"
    "                          /..                                                   "
    "    8.0 KiB [###########]  main.rs                                              "
    "H   4.0 KiB [########   ]  linked                                               "
    "*Total disk usage:  16.0 KiB   Apparent size:   9.2 KiB   Items: 2              "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// The listing scrolls to keep the cursor on the screen.
#[test]
fn scroll_to_cursor() {
    let tree = tree_of(SOURCE, false);
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &[Key::Down; 9], (80, 8)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    "    0.0   B             [           ]  sparse                                   "
    "    0.0   B             [           ]  raw \xFF\xFE                             "
    "    0.0   B             [           ]  quote " backslash \...\x09 control \x01 é"
    "@   0.0   B             [           ]  socket                                   "
    "^   0.0   B             [           ] /proc                                     "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// On a narrow terminal, the graph and the mtime need more columns than are left and are not
/// shown, the item count fits, and names are shortened.
#[test]
fn narrow_terminal() {
    let tree = tree_of(HIDDEN.as_bytes(), true);
    let options = BrowserOptions {
        show_hidden: false,
        show_item_count: true,
        show_mtime: true,
        ..imported()
    };
    assert_snapshot!(screen(&tree, root(&tree), &options, &[], (31, 9)), @r#"
    "duit 0.0.1 ~ Use the [imported]"
    "--- /data ---------------------"
    "   24.0 KiB      1 /photos     "
    "   10.0 KiB         notes.txt  "
    "    4.0 KiB         a-ve....txt"
    "H   4.0 KiB         sett...link"
    "                               "
    "                               "
    "*Total disk usage:  98.0 KiB   "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ...............................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ...............................
    ...............................
    ...............................
    ...............................
    ...............................
    R##################RRRRRRRRRRRR
    "#);
}

/// Page down moves the cursor by the rows of the listing, and stops at the last row.
#[test]
fn page_down_to_end() {
    let tree = tree_of(SOURCE, false);
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &[Key::PageDown; 3], (80, 8)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    "@   0.0   B             [           ]  socket                                   "
    "^   0.0   B             [           ] /proc                                     "
    "<   0.0   B             [           ]  node_modules                             "
    ">   0.0   B             [           ] /mnt                                      "
    "    0.0   B             [           ]  broken                                   "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// End moves the cursor to the last row, down stops there, and page up and up move it back.
#[test]
fn end_page_up() {
    let tree = tree_of(SOURCE, false);
    let keys = [Key::End, Key::Down, Key::PageUp, Key::Up];
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &keys, (80, 8)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    "    0.0   B             [           ]  raw \xFF\xFE                             "
    "    0.0   B             [           ]  quote " backslash \...\x09 control \x01 é"
    "@   0.0   B             [           ]  socket                                   "
    "^   0.0   B             [           ] /proc                                     "
    "<   0.0   B             [           ]  node_modules                             "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// Home moves the cursor to the first row, and up stops there.
#[test]
fn home() {
    let tree = tree_of(SOURCE, false);
    let mut keys = vec![Key::Char('j'); 9];
    keys.extend([Key::Home, Key::Char('k')]);
    assert_eq!(
        screen(&tree, root(&tree), &imported(), &keys, (80, 8)),
        screen(&tree, root(&tree), &imported(), &[], (80, 8))
    );
}

/// Going back to a directory restores both the cursor and the scroll position it had, so the
/// cursor stays on the first row of the listing.
#[test]
fn enter_parent_restores_view() {
    let tree = tree_of(SOURCE, false);
    let keys = [
        Key::Char('j'),
        Key::Char('j'),
        Key::Char('k'),
        Key::Char('l'),
        Key::Char('h'),
    ];
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &keys, (80, 5)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    "!   4.0 KiB             [####       ] /unreadable                               "
    "e   4.0 KiB             [####       ] /empty                                    "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// Opening a directory again puts the cursor on the entry it was on.
#[test]
fn enter_subdirectory_restores_cursor() {
    let tree = tree_of(SOURCE, false);
    let keys = [Key::Right, Key::Down, Key::Down, Key::Left, Key::Right];
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &keys, (80, 6)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project/src -----------------------------------------------------"
    "                          /..                                                   "
    "    8.0 KiB [###########]  main.rs                                              "
    "H   4.0 KiB [########   ]  linked                                               "
    "*Total disk usage:  16.0 KiB   Apparent size:   9.2 KiB   Items: 2              "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// Enter on the row of the parent directory goes back to it.
#[test]
fn enter_parent_row() {
    let tree = tree_of(SOURCE, false);
    let keys = [Key::Down, Key::Down, Key::Enter, Key::Enter];
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &keys, (80, 8)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 KiB S   4.0 KiB [###########] /src                                      "
    "!   4.0 KiB             [####       ] /unreadable                               "
    "e   4.0 KiB             [####       ] /empty                                    "
    "    4.0 KiB             [####       ]  README.md                                "
    "H   4.0 KiB             [####       ]  also-linked                              "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// Enter does nothing on a file or on a directory that was not read, such as one on another
/// filesystem, and going to the parent does nothing in the root.
#[test]
fn enter_without_directory() {
    let tree = tree_of(SOURCE, false);
    let screen = |keys: &[Key]| screen(&tree, root(&tree), &imported(), keys, (80, 18));
    let mut readme = vec![Key::Down; 3];
    assert_eq!(screen(&readme), {
        readme.push(Key::Enter);
        screen(&readme)
    });
    let mut mnt = vec![Key::Down; 11];
    assert_eq!(screen(&mnt), {
        mnt.push(Key::Enter);
        screen(&mnt)
    });
    assert_eq!(screen(&[]), screen(&[Key::Left]));
    for parent in [Key::Char('h'), Key::Char('<'), Key::Backspace] {
        assert_eq!(screen(&[]), screen(&[Key::Right, parent]));
    }
}

/// Going to the parent directory cuts the last component of the path, so a root whose name ends
/// with `/` loses it, and the view that was saved under its name is not found.
#[test]
fn enter_parent_cuts_path() {
    let tree = tree_of(
        br#"[1,2,{"progname":"ncdu","progver":"2.9.2"},
[{"name":"/data/","asize":4096,"dsize":4096},
{"name":"big","asize":8192,"dsize":8192},
[{"name":"sub","asize":4096,"dsize":4096}]]]"#,
        false,
    );
    let keys = [Key::Down, Key::Right, Key::Left];
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &keys, (80, 5)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /data ----------------------------------------------------------------------"
    "    8.0 KiB [###########]  big                                                  "
    "e   4.0 KiB [########   ] /sub                                                  "
    "*Total disk usage:  16.0 KiB   Apparent size:  16.0 KiB   Items: 2              "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// A sort key keeps the cursor on its entry and restores the scroll position that was saved
/// with the previous key.
#[test]
fn sort_keeps_cursor() {
    let tree = tree_of(SOURCE, false);
    let mut keys = vec![Key::Down; 9];
    keys.push(Key::Char('n'));
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &keys, (80, 8)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    ">   0.0   B             [           ] /mnt                                      "
    "<   0.0   B             [           ]  node_modules                             "
    "^   0.0   B             [           ] /proc                                     "
    "    0.0   B             [           ]  quote " backslash \...\x09 control \x01 é"
    "    0.0   B             [           ]  raw \xFF\xFE                             "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// A sort key before any other key puts the cursor on the first row, as no view of the directory
/// was saved yet.
#[test]
fn sort_first_key() {
    let tree = tree_of(SOURCE, false);
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &[Key::Char('n')], (80, 8)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    "    4.0 KiB             [####       ]  README.md                                "
    "H   4.0 KiB             [####       ]  also-linked                              "
    "    0.0   B             [           ]  broken                                   "
    "e   4.0 KiB             [####       ] /empty                                    "
    ">   0.0   B             [           ] /mnt                                      "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// The same sort key again reverses the order.
#[test]
fn sort_name_toggle() {
    let tree = tree_of(NAMES.as_bytes(), false);
    let keys = [Key::Char('n'), Key::Char('n')];
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &keys, (80, 9)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /names ---------------------------------------------------------------------"
    ">   0.0   B [           ] /zfs                                                  "
    "    4.0 KiB [###########]  file10                                               "
    "    4.0 KiB [###########]  file9                                                "
    "e   4.0 KiB [###########] /dir                                                  "
    "<   0.0   B [           ]  apattern                                             "
    "    4.0 KiB [###########]  File2                                                "
    "*Total disk usage:  16.0 KiB   Apparent size:  40.0   B   Items: 6              "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// `a` switches to apparent sizes and sorts by them, and `s` then reverses the order.
#[test]
fn sort_apparent_size_ascending() {
    let tree = tree_of(SOURCE, false);
    let keys = [Key::Char('a'), Key::Char('s')];
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &keys, (80, 10)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    "    2.0   B             [           ]  raw \xFF\xFE                             "
    "H 300.0   B             [           ]  also-linked                              "
    "    1.2 KiB             [           ]  README.md                                "
    "e   4.0 KiB             [           ] /empty                                    "
    "!   4.0 KiB             [           ] /unreadable                               "
    "    9.2 KiB S 300.0   B [           ] /src                                      "
    "    1.0 MiB             [###########]  sparse                                   "
    " Total disk usage:  32.0 KiB  *Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    RRRRRRRRRRRRRRRRRRRRRRRRRRRR##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// `C` sorts by item count in descending order, and again in ascending order.
#[test]
fn sort_item_count_ascending() {
    let tree = tree_of(
        item_counts(&[("few", 3), ("thousands", 1234), ("many", 12345)]).as_bytes(),
        false,
    );
    let keys = [Key::Char('c'), Key::Char('C'), Key::Char('C')];
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &keys, (80, 7)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /items ---------------------------------------------------------------------"
    "    0.0   B [           ]         file                                          "
    "    0.0   B [           ]      3 /few                                           "
    "    0.0   B [           ]  1,234 /thousands                                     "
    "    0.0   B [           ] 12,345 /many                                          "
    "*Total disk usage:   0.0   B   Apparent size:   0.0   B   Items: 13,586         "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// `m` shows the mtime and `M` sorts by it, in descending order and then in ascending order.
#[test]
fn sort_mtime_ascending() {
    let tree = tree_of(MTIME.as_bytes(), true);
    let options = BrowserOptions {
        extended: true,
        ..imported()
    };
    let keys = [Key::Char('m'), Key::Char('M'), Key::Char('M')];
    assert_snapshot!(redact_times(&screen(&tree, root(&tree), &options, &keys, (80, 8))), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /mtime ---------------------------------------------------------------------"
    "    4.0 KiB [########   ]                   no mtime  nomtime                   "
    "    4.0 KiB [########   ]  2024-01-03 HH:MM:SS +ZZZZ  early.txt                 "
    "    8.0 KiB [###########]  2024-01-04 HH:MM:SS +ZZZZ /old                       "
    "    4.0 KiB [########   ]  2024-01-06 HH:MM:SS +ZZZZ  latest.txt                "
    "    4.0 KiB [########   ]                   no mtime  plain                     "
    "*Total disk usage:  28.0 KiB   Apparent size:   8.5 KiB   Items: 6              "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// Without `-e`, `m` and `M` do nothing.
#[test]
fn mtime_keys_without_extended() {
    let tree = tree_of(MTIME.as_bytes(), false);
    let keys = [Key::Char('m'), Key::Char('M')];
    assert_eq!(
        screen(&tree, root(&tree), &imported(), &keys, (80, 8)),
        screen(&tree, root(&tree), &imported(), &[], (80, 8))
    );
}

/// `t` lists the directories first.
#[test]
fn sort_directories_first_toggle() {
    let tree = tree_of(NAMES.as_bytes(), false);
    let keys = [Key::Char('n'), Key::Char('t')];
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &keys, (80, 9)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /names ---------------------------------------------------------------------"
    "e   4.0 KiB [###########] /dir                                                  "
    ">   0.0   B [           ] /zfs                                                  "
    "    4.0 KiB [###########]  File2                                                "
    "<   0.0   B [           ]  apattern                                             "
    "    4.0 KiB [###########]  file9                                                "
    "    4.0 KiB [###########]  file10                                               "
    "*Total disk usage:  16.0 KiB   Apparent size:  40.0   B   Items: 6              "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// `e` hides the hidden entries and keeps the cursor on its entry.
#[test]
fn hide_hidden_keeps_cursor() {
    let tree = tree_of(HIDDEN.as_bytes(), false);
    let keys = [Key::Down, Key::Down, Key::Char('e')];
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &keys, (80, 9)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /data ----------------------------------------------------------------------"
    "   24.0 KiB [######     ] /photos                                               "
    "   10.0 KiB [##         ]  notes.txt                                            "
    "    4.0 KiB [#          ]  a-very-long-file-name.txt                            "
    "H   4.0 KiB [#          ]  settings-link                                        "
    "                                                                                "
    "                                                                                "
    "*Total disk usage:  98.0 KiB   Apparent size:  92.5 KiB   Items: 12             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// When `e` hides the entry under the cursor, the cursor goes to the first row.
#[test]
fn hide_hidden_cursor_entry() {
    let tree = tree_of(HIDDEN.as_bytes(), false);
    let keys = [Key::Down, Key::Down, Key::Down, Key::Char('e')];
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &keys, (80, 9)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /data ----------------------------------------------------------------------"
    "   24.0 KiB [######     ] /photos                                               "
    "   10.0 KiB [##         ]  notes.txt                                            "
    "    4.0 KiB [#          ]  a-very-long-file-name.txt                            "
    "H   4.0 KiB [#          ]  settings-link                                        "
    "                                                                                "
    "                                                                                "
    "*Total disk usage:  98.0 KiB   Apparent size:  92.5 KiB   Items: 12             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// `g` cycles through the graph, the percentage, both, and neither, `u` cycles the shared column
/// through unique and off, `c` shows the item counts, and `a` twice goes back to disk usage. Each
/// draws as its option does.
#[test]
fn display_keys() {
    let tree = tree_of(SOURCE, false);
    let screen = |options: &BrowserOptions, keys: &[Key]| {
        screen(&tree, root(&tree), options, keys, (80, 18))
    };
    let graph = |show_graph, show_percent| BrowserOptions {
        show_graph,
        show_percent,
        ..imported()
    };
    assert_eq!(
        screen(&imported(), &[Key::Char('g')]),
        screen(&graph(false, true), &[])
    );
    assert_eq!(
        screen(&imported(), &[Key::Char('g'); 2]),
        screen(&graph(true, true), &[])
    );
    assert_eq!(
        screen(&imported(), &[Key::Char('g'); 3]),
        screen(&graph(false, false), &[])
    );
    assert_eq!(
        screen(&imported(), &[Key::Char('g'); 4]),
        screen(&imported(), &[])
    );
    let shared = |shared_column| BrowserOptions {
        shared_column,
        ..imported()
    };
    assert_eq!(
        screen(&imported(), &[Key::Char('u')]),
        screen(&shared(SharedColumn::Unique), &[])
    );
    assert_eq!(
        screen(&imported(), &[Key::Char('u'); 2]),
        screen(&shared(SharedColumn::Off), &[])
    );
    assert_eq!(
        screen(&imported(), &[Key::Char('u'); 3]),
        screen(&imported(), &[])
    );
    let item_count = BrowserOptions {
        show_item_count: true,
        ..imported()
    };
    assert_eq!(
        screen(&imported(), &[Key::Char('c')]),
        screen(&item_count, &[])
    );
    // The first `a` puts the cursor on `sparse`, the first row by apparent size, as no view was
    // saved yet, and the second keeps it there.
    assert_eq!(
        screen(&imported(), &[Key::Char('a'); 2]),
        screen(&imported(), &[Key::Down; 5])
    );
}

/// With `--confirm-quit`, `q` opens a window that asks for confirmation. The window clears the
/// reverse video of the cursor row below it.
#[test]
fn quit_confirmation() {
    let tree = tree_of(SOURCE, false);
    let options = BrowserOptions {
        confirm_quit: true,
        ..imported()
    };
    let keys = [Key::Down, Key::Down, Key::Char('q')];
    assert_snapshot!(screen(&tree, root(&tree), &options, &keys, (80, 10)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 KiB S   4.0 KiB [###########] /src                                      "
    "!   4.0 KiB             [####┌── Confirm quit ────┐                             "
    "e   4.0 KiB             [####│                    │                             "
    "    4.0 KiB             [####│ Really quit? (y/N) │                             "
    "H   4.0 KiB             [####└────────────────────┘                             "
    "    0.0   B             [           ]  sparse                                   "
    "    0.0   B             [           ]  raw \xFF\xFE                             "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................BBBBBBBBBBBBBB..................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRR......................RRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    .............................................B.B................................
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// `q` quits, or with `--confirm-quit` asks first: `y` or `Y` quits, and any other key closes
/// the window and does nothing else.
#[test]
fn quit_keys() {
    let tree = tree_of(SOURCE, false);
    let quits = |options: &BrowserOptions, keys: &[Key]| {
        let mut options = options.clone();
        let mut browser = Browser::new(&tree, root(&tree), &options);
        keys.iter()
            .map(|&key| browser.key_input(&tree, &mut options, key, 24))
            .collect::<Vec<_>>()
    };
    assert_eq!(quits(&imported(), &[Key::Char('q')]), [true]);
    let options = BrowserOptions {
        confirm_quit: true,
        ..imported()
    };
    let keys = [
        Key::Char('q'),
        Key::Char('q'),
        Key::Char('q'),
        Key::Char('n'),
        Key::Char('q'),
        Key::Char('y'),
        Key::Char('q'),
        Key::Char('Y'),
    ];
    assert_eq!(
        quits(&options, &keys),
        [false, false, false, false, false, true, false, true]
    );
    assert_eq!(
        screen(
            &tree,
            root(&tree),
            &options,
            &[Key::Char('q'), Key::Char('j')],
            (80, 10)
        ),
        screen(&tree, root(&tree), &options, &[], (80, 10))
    );
}

/// `i` shows the name, the type, and the sizes of a file.
#[test]
fn info_file() {
    let tree = tree_of(SOURCE, false);
    let keys = [Key::Down, Key::Down, Key::Down, Key::Char('i')];
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &keys, (80, 14)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 KiB S   4.0 KiB [###########] /src                                      "
    "!   4.0 Ki┌── Item info ─────────────────────────────────────────────┐          "
    "e   4.0 Ki│                                                          │          "
    "    4.0 Ki│  Name: README.md                                         │          "
    "H   4.0 Ki│  Type: File                                              │          "
    "    0.0   │     Disk usage:   4.0 KiB (4,096)                        │          "
    "    0.0   │  Apparent size:   1.2 KiB (1,234)                        │          "
    "    0.0   │                                                          │rol \x01 é"
    "@   0.0   │                             Press i to close this window │          "
    "^   0.0   └──────────────────────────────────────────────────────────┘          "
    "<   0.0   B             [           ]  node_modules                             "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    .............BBBBBBBBBBB........................................................
    ................................................................................
    RRRRRRRRRR...BBBBBB...................................................RRRRRRRRRR
    .............BBBBBB.............................................................
    .............BBBBBBBBBBBBBBB....................................................
    .............BBBBBBBBBBBBBBB....................................................
    ................................................................................
    ..............................................B.................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// The info of a directory adds the shared and unique parts of its sizes, which hard links with
/// links outside of it give, and its number of items.
#[test]
fn info_directory() {
    let tree = tree_of(SOURCE, false);
    let keys = [Key::Char('i')];
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &keys, (80, 18)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 Ki┌── Item info ─────────────────────────────────────────────┐          "
    "!   4.0 Ki│                                                          │          "
    "e   4.0 Ki│  Name: src                                               │          "
    "    4.0 Ki│  Type: Directory                                         │          "
    "H   4.0 Ki│     Disk usage:  16.0 KiB (16,384)                       │          "
    "    0.0   │       > shared:   4.0 KiB (4,096)                        │          "
    "    0.0   │       > unique:  12.0 KiB (12,288)                       │          "
    "    0.0   │  Apparent size:   9.2 KiB (9,396)                        │rol \x01 é"
    "@   0.0   │       > shared: 300.0   B (300)                          │          "
    "^   0.0   │       > unique:   8.9 KiB (9,096)                        │          "
    "<   0.0   │      Sub items: 2                                        │          "
    ">   0.0   │                                                          │          "
    "    0.0   │                             Press i to close this window │          "
    "          └──────────────────────────────────────────────────────────┘          "
    "                                                                                "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRR...BBBBBBBBBBB..............................................RRRRRRRRRR
    ................................................................................
    .............BBBBBB.............................................................
    .............BBBBBB.............................................................
    .............BBBBBBBBBBBBBBB....................................................
    ................................................................................
    ................................................................................
    .............BBBBBBBBBBBBBBB....................................................
    ................................................................................
    ................................................................................
    .............BBBBBBBBBBBBBBB....................................................
    ................................................................................
    ..............................................B.................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// With `-e`, the mode, the owner, and the latest mtime replace the type, and a hard link has
/// tabs, its link count, and its inode. `--si` sizes count in powers of 1000.
#[test]
fn info_extended_hard_link() {
    let tree = tree_of(SOURCE, true);
    let options = BrowserOptions {
        si: true,
        extended: true,
        ..imported()
    };
    let keys = [Key::Down, Key::Down, Key::Down, Key::Down, Key::Char('i')];
    assert_snapshot!(redact_times(&screen(&tree, root(&tree), &options, &keys, (80, 16))), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    "   16.4 kB S   4.1 kB [###########] /src                                        "
    "!   4.1 kB┌── Item info ───────────────────────────1:Info───2:Links──┐          "
    "e   4.1 kB│                                                          │          "
    "    4.1 kB│  Name: also-linked                                       │          "
    "H   4.1 kB│  Mode: -rw-r--r--  UID: 1000   GID: 1000                 │          "
    "    0.0  B│  Last modified: 2023-11-14 HH:MM:SS +ZZZZ                │          "
    "    0.0  B│     Disk usage:   4.1 kB (4,096)                         │          "
    "    0.0  B│  Apparent size: 300.0  B (300)                           │rol \x01 é"
    "@   0.0  B│     Link count: 2      Inode: 104                        │          "
    "^   0.0  B│                                                          │          "
    "<   0.0  B│                             Press i to close this window │          "
    ">   0.0  B└──────────────────────────────────────────────────────────┘          "
    "    0.0  B            [           ]  broken                                     "
    "*Total disk usage:  32.8 kB   Apparent size:   1.1 MB   Items: 15               "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    .............BBBBBBBBBBB...........................#RRRRR...B...................
    ................................................................................
    .............BBBBBB.............................................................
    RRRRRRRRRR...BBBBBB..........BBBBBBB......BBBBBB......................RRRRRRRRRR
    .............BBBBBBBBBBBBBBB....................................................
    .............BBBBBBBBBBBBBBB....................................................
    .............BBBBBBBBBBBBBBB....................................................
    .............BBBBBBBBBBBBBBB.....BBBBBBBBB......................................
    ................................................................................
    ..............................................B.................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// Extended information without a mode or an mtime leaves out its fields, and the row that the
/// mtime would take stays empty.
#[test]
fn info_extended_without_mtime() {
    let tree = tree_of(MTIME.as_bytes(), true);
    let options = BrowserOptions {
        extended: true,
        ..imported()
    };
    let keys = [Key::End, Key::Up, Key::Up, Key::Char('i')];
    assert_snapshot!(screen(&tree, root(&tree), &options, &keys, (80, 14)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /mtime ---------------------------------------------------------------------"
    "    8.0 Ki┌── Item info ─────────────────────────────────────────────┐          "
    "    4.0 Ki│                                                          │          "
    "    4.0 Ki│  Name: nomtime                                           │          "
    "    4.0 Ki│    UID: 1000                                             │          "
    "    4.0 Ki│     Disk usage:   4.0 KiB (4,096)                        │          "
    "          │  Apparent size: 100.0   B (100)                          │          "
    "          │                                                          │          "
    "          │                                                          │          "
    "          │                             Press i to close this window │          "
    "          └──────────────────────────────────────────────────────────┘          "
    "                                                                                "
    "*Total disk usage:  28.0 KiB   Apparent size:   8.5 KiB   Items: 6              "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    .............BBBBBBBBBBB........................................................
    ................................................................................
    RRRRRRRRRR...BBBBBB...................................................RRRRRRRRRR
    .............BBBBBBB............................................................
    .............BBBBBBBBBBBBBBB....................................................
    .............BBBBBBBBBBBBBBB....................................................
    ................................................................................
    ................................................................................
    ..............................................B.................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// An entry that could not be read has no sizes, and counts as excluded.
#[test]
fn info_read_error() {
    let tree = tree_of(SOURCE, false);
    let keys = [Key::End, Key::Char('i')];
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &keys, (80, 14)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    "e   4.0 KiB             [####       ] /empty                                    "
    "    4.0 Ki┌── Item info ─────────────────────────────────────────────┐          "
    "H   4.0 Ki│                                                          │          "
    "    0.0   │  Name: broken                                            │          "
    "    0.0   │  Type: Excluded                                          │          "
    "    0.0   │     Disk usage:   0.0   B (0)                            │rol \x01 é"
    "@   0.0   │  Apparent size:   0.0   B (0)                            │          "
    "^   0.0   │                                                          │          "
    "<   0.0   │                             Press i to close this window │          "
    ">   0.0   └──────────────────────────────────────────────────────────┘          "
    "    0.0   B             [           ]  broken                                   "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    .............BBBBBBBBBBB........................................................
    ................................................................................
    .............BBBBBB.............................................................
    .............BBBBBB.............................................................
    .............BBBBBBBBBBBBBBB....................................................
    .............BBBBBBBBBBBBBBB....................................................
    ................................................................................
    ..............................................B.................................
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// The links tab lists the paths of the links to the inode below the root, sorted, and marks the
/// selected entry. The cursor of the list moves on its own.
#[test]
fn info_links() {
    let tree = tree_of(SOURCE, false);
    let keys = [
        Key::Down,
        Key::Down,
        Key::Down,
        Key::Down,
        Key::Char('i'),
        Key::Char('2'),
        Key::Down,
    ];
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &keys, (80, 16)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 Ki┌── Item info ───────────────────────────1:Info───2:Links──┐          "
    "!   4.0 Ki│                                                          │          "
    "e   4.0 Ki│ */also-linked                                            │          "
    "    4.0 Ki│  src/linked                                              │          "
    "H   4.0 Ki│                                                          │          "
    "    0.0   │                                                          │          "
    "    0.0   │                                                          │          "
    "    0.0   │                                                          │rol \x01 é"
    "@   0.0   │                                                          │          "
    "^   0.0   │                                                          │          "
    "<   0.0   │                                                          │          "
    ">   0.0   │     2/2                     Press i to close this window │          "
    "    0.0   └──────────────────────────────────────────────────────────┘          "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    .............BBBBBBBBBBB...........................B........#RRRRRR.............
    ................................................................................
    ................................................................................
    ............RRRRRRRRRRR.........................................................
    RRRRRRRRRR............................................................RRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ..............................................B.................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// The links tab scrolls to keep its cursor on the screen, and pages by 5 links. Long paths are
/// shortened.
#[test]
fn info_links_scroll() {
    let tree = tree_of(LINKS.as_bytes(), false);
    let keys = [
        Key::Down,
        Key::Char('i'),
        Key::Char('2'),
        Key::PageDown,
        Key::PageUp,
    ];
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &keys, (80, 16)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /links ---------------------------------------------------------------------"
    "    8.0 Ki┌── Item info ───────────────────────────1:Info───2:Links──┐          "
    "H   4.0 Ki│                                                          │          "
    "H   4.0 Ki│  /c                                                      │          "
    "H   4.0 Ki│  /d                                                      │          "
    "H   4.0 Ki│  /e                                                      │          "
    "H   4.0 Ki│  /f                                                      │          "
    "H   4.0 Ki│  /g                                                      │          "
    "H   4.0 Ki│ */h                                                      │          "
    "H   4.0 Ki│  sub/a-file-name-that-is-lo...be-shortened-in-the-window │          "
    "          │  sub/x                                                   │          "
    "          │  sub/y                                                   │          "
    "          │     6/11                    Press i to close this window │          "
    "          └──────────────────────────────────────────────────────────┘          "
    "*Total disk usage:  12.0 KiB   Apparent size:   8.1 KiB   Items: 12             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    .............BBBBBBBBBBB...........................B........#RRRRRR.............
    RRRRRRRRRR............................................................RRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    ............RRR.................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ..............................................B.................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// Enter in the links tab opens the directory of the selected link with the cursor on it, but
/// keeps the path, the scroll position, and the parent directories of the directory that was open:
/// there is no row of the parent directory, and going to the parent does nothing.
#[test]
fn info_links_enter() {
    let tree = tree_of(SOURCE, false);
    let keys = [
        Key::Down,
        Key::Down,
        Key::Down,
        Key::Down,
        Key::Char('i'),
        Key::Char('2'),
        Key::Down,
        Key::Enter,
        Key::Char('h'),
    ];
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &keys, (80, 6)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    "H   4.0 KiB [########   ]  linked                                               "
    "                                                                                "
    "                                                                                "
    "*Total disk usage:  16.0 KiB   Apparent size:   9.2 KiB   Items: 2              "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// The keys of the info window, and the keys of the listing that close it.
#[test]
fn info_keys() {
    let tree = tree_of(SOURCE, false);
    let src = subdirectory(&tree, b"src");
    let screen_of = |directory: DirectoryId, keys: &[Key]| {
        screen(&tree, directory, &imported(), keys, (80, 18))
    };
    let root = root(&tree);
    let (i, j, k) = (Key::Char('i'), Key::Down, Key::Up);
    let link = [j, j, j, j];

    // `i` and `q` close the window, and `q` does not quit.
    assert_ne!(screen_of(root, &[i]), screen_of(root, &[]));
    assert_eq!(screen_of(root, &[i, i]), screen_of(root, &[]));
    assert_eq!(screen_of(root, &[i, Key::Char('q')]), screen_of(root, &[]));
    let mut options = imported();
    let mut browser = Browser::new(&tree, root, &options);
    browser.key_input(&tree, &mut options, i, 18);
    assert!(!browser.key_input(&tree, &mut options, Key::Char('q'), 18));
    // The cursor keys move the cursor of the listing, and the window shows the entry under it.
    assert_eq!(screen_of(root, &[i, j, j]), screen_of(root, &[j, j, i]));
    assert_eq!(
        screen_of(root, &[i, Key::End]),
        screen_of(root, &[Key::End, i])
    );
    // On the row of the parent directory, the window closes, and `i` does not open it.
    assert_eq!(screen_of(src, &[j, i, k]), screen_of(src, &[j, k]));
    assert_eq!(screen_of(src, &[i]), screen_of(src, &[]));
    // Sort and display keys keep the window open on the same entry.
    assert_eq!(
        screen_of(root, &[j, i, Key::Char('n'), Key::Char('g')]),
        screen_of(root, &[j, Key::Char('n'), Key::Char('g'), i])
    );
    // Opening a directory, going to the parent, and `e` close the window. Without a parent
    // directory, `h` does nothing and the window stays.
    assert_eq!(
        screen_of(root, &[i, Key::Enter]),
        screen_of(root, &[Key::Enter])
    );
    assert_eq!(
        screen_of(src, &[j, i, Key::Char('<')]),
        screen_of(src, &[j, Key::Char('<')])
    );
    assert_eq!(
        screen_of(root, &[i, Key::Char('e')]),
        screen_of(root, &[Key::Char('e')])
    );
    assert_eq!(screen_of(root, &[i, Key::Char('h')]), screen_of(root, &[i]));
    // The tabs are only for hard links, and switch with `1`, `h`, or left and `2`, `l`, or right.
    assert_eq!(screen_of(root, &[i, Key::Char('2')]), screen_of(root, &[i]));
    let tab = |keys: &[Key]| screen_of(root, &[&link[..], &[i], keys].concat());
    assert_eq!(tab(&[Key::Char('l')]), tab(&[Key::Char('2')]));
    assert_eq!(tab(&[Key::Right]), tab(&[Key::Char('2')]));
    assert_eq!(tab(&[Key::Char('2'), Key::Char('1')]), tab(&[]));
    assert_eq!(tab(&[Key::Char('2'), Key::Char('h')]), tab(&[]));
    assert_eq!(tab(&[Key::Char('2'), Key::Left]), tab(&[]));
    // In the links tab, the cursor keys move the cursor of the list, not that of the listing.
    assert_eq!(tab(&[Key::Char('2'), j, Key::Char('1')]), tab(&[]));
    assert_eq!(
        tab(&[Key::Char('2'), Key::End]),
        tab(&[Key::Char('2'), j, j])
    );
    // The window keeps the cursor of the list until it shows another entry, also after `e`
    // closed it.
    assert_eq!(
        tab(&[Key::Char('2'), j, Key::Char('e'), i, Key::Char('2')]),
        screen_of(
            root,
            &[&link[..], &[Key::Char('e'), i, Key::Char('2'), j]].concat()
        )
    );
    assert_eq!(
        tab(&[Key::Char('2'), j, Key::Char('1'), k, j, Key::Char('2')]),
        tab(&[Key::Char('2')])
    );
}

/// `?` opens the help window on the keys tab, with the first 10 keys and a mark that there are
/// more.
#[test]
fn help() {
    let tree = tree_of(SOURCE, false);
    let keys = [Key::Char('?')];
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &keys, (80, 18)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 Ki┌── ncdu help ────────────────1:Keys───2:Format───3:About──┐          "
    "!   4.0 Ki│                                                          │          "
    "e   4.0 Ki│       up, k Move cursor up                               │          "
    "    4.0 Ki│     down, j Move cursor down                             │          "
    "H   4.0 Ki│ right/enter Open selected directory                      │          "
    "    0.0   │  left, <, h Open parent directory                        │          "
    "    0.0   │           n Sort by name (ascending/descending)          │          "
    "    0.0   │           s Sort by size (ascending/descending)          │rol \x01 é"
    "@   0.0   │           C Sort by items (ascending/descending)         │          "
    "^   0.0   │           M Sort by mtime (-e flag)                      │          "
    "<   0.0   │           d Delete selected file or directory            │          "
    ">   0.0   │           t Toggle dirs before files when sorting        │          "
    "    0.0   │                        -- more --                        │          "
    "          │                                         Press q to close │          "
    "          └──────────────────────────────────────────────────────────┘          "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRR...BBBBBBBBBBB................#RRRRR...B..........B.........RRRRRRRRRR
    ................................................................................
    ..................BBBBB.........................................................
    ................BBBBBBB.........................................................
    ............BBBBBBBBBBB.........................................................
    .............BBBBBBBBBB.........................................................
    ......................B.........................................................
    ......................B.........................................................
    ......................B.........................................................
    ......................B.........................................................
    ......................B.........................................................
    ......................B.........................................................
    ................................................................................
    ..........................................................B.....................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// The keys tab scrolls one line at a time, and stops at the last key.
#[test]
fn help_scroll() {
    let tree = tree_of(SOURCE, false);
    let keys = [&[Key::Char('?')], &[Key::Char('j'); 11][..]].concat();
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &keys, (80, 18)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 Ki┌── ncdu help ────────────────1:Keys───2:Format───3:About──┐          "
    "!   4.0 Ki│                                                          │          "
    "e   4.0 Ki│           g Show percentage and/or graph                 │          "
    "    4.0 Ki│           u Show/hide hard link shared sizes             │          "
    "H   4.0 Ki│           a Toggle between apparent size and disk usage  │          "
    "    0.0   │           c Toggle display of child item counts          │          "
    "    0.0   │           m Toggle display of latest mtime (-e flag)     │          "
    "    0.0   │           e Show/hide hidden or excluded files           │rol \x01 é"
    "@   0.0   │           i Show information about selected item         │          "
    "^   0.0   │           r Recalculate the current directory            │          "
    "<   0.0   │           b Spawn shell in current directory             │          "
    ">   0.0   │           q Quit ncdu                                    │          "
    "    0.0   │                                                          │          "
    "          │                                         Press q to close │          "
    "          └──────────────────────────────────────────────────────────┘          "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRR...BBBBBBBBBBB................#RRRRR...B..........B.........RRRRRRRRRR
    ................................................................................
    ......................B.........................................................
    ......................B.........................................................
    ......................B.........................................................
    ......................B.........................................................
    ......................B.........................................................
    ......................B.........................................................
    ......................B.........................................................
    ......................B.........................................................
    ......................B.........................................................
    ......................B.........................................................
    ................................................................................
    ..........................................................B.....................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// The format tab explains the flags in front of the entries.
#[test]
fn help_format() {
    let tree = tree_of(SOURCE, false);
    let keys = [Key::Char('?'), Key::Char('2')];
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &keys, (80, 18)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 Ki┌── ncdu help ────────────────1:Keys───2:Format───3:About──┐          "
    "!   4.0 Ki│                                                          │          "
    "e   4.0 Ki│  X  [size] [graph] [file or directory]                   │          "
    "    4.0 Ki│   The X is only present in the following cases:          │          "
    "H   4.0 Ki│                                                          │          "
    "    0.0   │   ! An error occurred while reading this directory       │          "
    "    0.0   │   . An error occurred while reading a subdirectory       │          "
    "    0.0   │   < File or directory is excluded from the statistics    │rol \x01 é"
    "@   0.0   │   e Empty directory                                      │          "
    "^   0.0   │   > Directory was on another filesystem                  │          "
    "<   0.0   │   @ This is not a file nor a dir (symlink, socket, ...)  │          "
    ">   0.0   │   ^ Excluded Linux pseudo-filesystem                     │          "
    "    0.0   │   H Same file was already counted (hard link)            │          "
    "          │                                         Press q to close │          "
    "          └──────────────────────────────────────────────────────────┘          "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRR...BBBBBBBBBBB................B........#RRRRRRR...B.........RRRRRRRRRR
    ................................................................................
    .............BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB..............................
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
    ..........................................................B.....................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// The about tab draws the logo in reverse video, and the version of duit.
#[test]
fn help_about() {
    let tree = tree_of(SOURCE, false);
    let keys = [Key::Char('?'), Key::Char('3')];
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &keys, (80, 18)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 Ki┌── ncdu help ────────────────1:Keys───2:Format───3:About──┐          "
    "!   4.0 Ki│                                                          │          "
    "e   4.0 Ki│                                                          │          "
    "    4.0 Ki│                                          NCurses         │          "
    "H   4.0 Ki│                                          Disk            │          "
    "    0.0   │                                          Usage           │          "
    "    0.0   │                                                          │          "
    "    0.0   │                                          0.0.1           │rol \x01 é"
    "@   0.0   │                                                          │          "
    "^   0.0   │          Written by Yorhel <projects@yorhel.nl>          │          "
    "<   0.0   │               https://dev.yorhel.nl/ncdu                 │          "
    ">   0.0   │                                                          │          "
    "    0.0   │                                                          │          "
    "          │                                         Press q to close │          "
    "          └──────────────────────────────────────────────────────────┘          "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRR...BBBBBBBBBBB................B........B..........#RRRRRR...RRRRRRRRRR
    ................................................................................
    ................................................................................
    ......................RRRRRR..RRRRR......RR..RR..R..............................
    ......................RR..RR..RR.........RR..RR..R..............................
    ......................RR..RR..RR.....RRRRRR..RR..R..............................
    ......................RR..RR..RR.....RR..RR..RR..R..............................
    ......................RR..RR..RRRRR..RRRRRR..RRRRR..............................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ..........................................................B.....................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// The keys of the help window, which takes every key, and `?` in the info window.
#[test]
fn help_keys() {
    let tree = tree_of(SOURCE, false);
    let screen_of = |keys: &[Key]| screen(&tree, root(&tree), &imported(), keys, (80, 18));
    let (help, i, j, k) = (Key::Char('?'), Key::Char('i'), Key::Down, Key::Up);
    let tab = |key: char| [help, Key::Char(key)];

    // Any key other than those of the window closes it, and the listing does not see it.
    assert_ne!(screen_of(&[help]), screen_of(&[]));
    for key in [
        Key::Char('q'),
        help,
        i,
        Key::Enter,
        Key::Backspace,
        Key::Home,
        Key::End,
        Key::Other,
    ] {
        assert_eq!(screen_of(&[help, key]), screen_of(&[]), "{key:?}");
    }
    // `q` does not quit, and does not ask to.
    let mut options = imported();
    let mut browser = Browser::new(&tree, root(&tree), &options);
    browser.key_input(&tree, &mut options, help, 18);
    assert!(!browser.key_input(&tree, &mut options, Key::Char('q'), 18));
    let confirm = BrowserOptions {
        confirm_quit: true,
        ..imported()
    };
    assert_eq!(
        screen(
            &tree,
            root(&tree),
            &confirm,
            &[help, Key::Char('q')],
            (80, 18)
        ),
        screen_of(&[])
    );
    // The tabs switch with `1`, `2`, and `3`, and with `h` or left and `l` or right.
    assert_ne!(screen_of(&tab('2')), screen_of(&[help]));
    assert_ne!(screen_of(&tab('3')), screen_of(&tab('2')));
    assert_eq!(
        screen_of(&[&tab('3')[..], &[Key::Char('1')]].concat()),
        screen_of(&[help])
    );
    assert_eq!(screen_of(&[help, Key::Char('l')]), screen_of(&tab('2')));
    assert_eq!(
        screen_of(&[help, Key::Right, Key::Right]),
        screen_of(&tab('3'))
    );
    assert_eq!(
        screen_of(&[help, Key::Char('l'), Key::Char('l'), Key::Char('l')]),
        screen_of(&tab('3'))
    );
    assert_eq!(
        screen_of(&[&tab('3')[..], &[Key::Char('h')]].concat()),
        screen_of(&tab('2'))
    );
    assert_eq!(
        screen_of(&[&tab('3')[..], &[Key::Left, Key::Left]].concat()),
        screen_of(&[help])
    );
    assert_eq!(screen_of(&[help, Key::Char('h')]), screen_of(&[help]));
    // Space, `j`, down, and page down scroll the keys tab down one line, and `k`, up, and page up
    // scroll it up.
    let scrolled = screen_of(&[help, j]);
    assert_ne!(scrolled, screen_of(&[help]));
    assert_eq!(screen_of(&[help, Key::Char('j')]), scrolled);
    assert_eq!(screen_of(&[help, Key::Char(' ')]), scrolled);
    assert_eq!(screen_of(&[help, Key::PageDown]), scrolled);
    assert_eq!(screen_of(&[help, j, j, Key::Char('k')]), scrolled);
    assert_eq!(screen_of(&[help, j, j, k]), scrolled);
    assert_eq!(screen_of(&[help, j, j, Key::PageUp]), scrolled);
    assert_eq!(screen_of(&[help, k]), screen_of(&[help]));
    // The other tabs do not scroll.
    assert_eq!(
        screen_of(&[&tab('2')[..], &[j]].concat()),
        screen_of(&tab('2'))
    );
    assert_eq!(
        screen_of(&[&tab('3')[..], &[j, k]].concat()),
        screen_of(&tab('3'))
    );
    // A key that keeps the tab keeps the scroll position, and one that changes the tab or closes
    // the window starts at the top again.
    assert_eq!(screen_of(&[help, j, Key::Char('1')]), scrolled);
    assert_eq!(screen_of(&[help, j, Key::Char('h')]), scrolled);
    assert_eq!(screen_of(&[help, j, Key::Left]), scrolled);
    assert_eq!(
        screen_of(&[help, j, Key::Char('2'), Key::Char('1')]),
        screen_of(&[help])
    );
    assert_eq!(
        screen_of(&[help, j, Key::Char('q'), help]),
        screen_of(&[help])
    );
    // The window opens again on the tab it was closed on.
    assert_eq!(
        screen_of(&[&tab('2')[..], &[Key::Char('q'), help]].concat()),
        screen_of(&tab('2'))
    );
    // `?` in the info window replaces it with the help window, and closing that one shows the
    // listing.
    assert_eq!(screen_of(&[i, help]), screen_of(&[help]));
    assert_eq!(screen_of(&[i, help, Key::Char('q')]), screen_of(&[]));
    // The info window keeps the cursor of the links tab while the help window is open.
    let link = [j, j, j, j, i, Key::Char('2'), j];
    assert_eq!(
        screen_of(&[&link[..], &[help, Key::Char('q'), i, Key::Char('2')]].concat()),
        screen_of(&link)
    );
}
