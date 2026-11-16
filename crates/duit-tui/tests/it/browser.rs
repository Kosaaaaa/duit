use std::fmt::Write as _;

use duit_format::import;
use duit_model::{DirectoryId, EntryKind, MemorySink, Tree};
use duit_tui::{Browser, BrowserOptions, GraphStyle, SharedColumn, SortColumn, SortOrder};
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

/// Draw `directory` of `tree` on a terminal of `width` by `height`, with the cursor on the row
/// `cursor` of the listing. Returns the text of each line, quoted, then one line of attributes per
/// line: `R` for reverse video, `B` for bold, `#` for both, and `.` for neither.
fn screen(
    tree: &Tree,
    directory: DirectoryId,
    options: &BrowserOptions,
    cursor: usize,
    (width, height): (u16, u16),
) -> String {
    let mut browser = Browser::new(tree, directory, options);
    browser.select(cursor);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal opens");
    terminal
        .draw(|frame| browser.draw(frame, tree, options))
        .expect("browser draws");
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
    assert_snapshot!(screen(&tree, root(&tree), &imported(), 0, (80, 18)), @r#"
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
    assert_snapshot!(screen(&tree, root(&tree), &options, 0, (80, 18)), @r#"
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
    assert_snapshot!(screen(&tree, root(&tree), &options, 0, (80, 18)), @r#"
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
    assert_snapshot!(screen(&tree, root(&tree), &options, 0, (80, 18)), @r#"
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
    assert_snapshot!(screen(&tree, root(&tree), &options, 0, (80, 14)), @r#"
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
    assert_snapshot!(screen(&tree, root(&tree), &options, 0, (80, 9)), @r#"
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
    assert_snapshot!(screen(&tree, root(&tree), &options, 0, (80, 9)), @r#"
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
    assert_snapshot!(screen(&tree, root(&tree), &options, 0, (80, 9)), @r#"
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
    assert_snapshot!(screen(&tree, root(&tree), &options, 0, (80, 9)), @r#"
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
    assert_snapshot!(screen(&tree, root(&tree), &options, 0, (80, 7)), @r#"
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
    assert_snapshot!(redact_times(&screen(&tree, root(&tree), &options, 0, (80, 8))), @r#"
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
    assert_snapshot!(redact_times(&screen(&tree, old, &options, 0, (80, 5))), @r#"
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
    assert_snapshot!(screen(&tree, root(&tree), &options, 0, (80, 9)), @r#"
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
    assert_snapshot!(screen(&tree, root(&tree), &options, 0, (80, 9)), @r#"
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
    assert_snapshot!(screen(&tree, src, &imported(), 1, (80, 6)), @r#"
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
    assert_snapshot!(screen(&tree, root(&tree), &imported(), 9, (80, 8)), @r#"
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
    assert_snapshot!(screen(&tree, root(&tree), &options, 0, (31, 9)), @r#"
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
