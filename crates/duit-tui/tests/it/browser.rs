use std::env;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs::{self, File, Permissions};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use duit_format::{BinaryReader, import};
use duit_model::{DirectoryId, EntryKind, EntryType, MemorySink, Stat, Tree};
use duit_tui::{
    Action, Browser, BrowserOptions, ColorScheme, GraphStyle, Key, SharedColumn, SortColumn,
    SortOrder,
};
use insta::assert_snapshot;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tempfile::TempDir;

use crate::render;

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
    sink.finish(&mut |_, _| {})
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

/// The options of an import, which ncdu marks in the header, and which disables refreshing,
/// deleting, and the shell unless they are enabled.
fn imported() -> BrowserOptions {
    BrowserOptions {
        imported: true,
        can_delete: false,
        can_shell: false,
        can_refresh: false,
        ..BrowserOptions::default()
    }
}

/// Open `directory` of `tree` on a terminal of `width` by `height`, and draw it again after each
/// of `keys`, as ncdu does between key presses. Returns the text of each line of the last screen,
/// quoted, then one line of attributes per line: `R` for reverse video, `B` for bold, `#` for
/// both, and `.` for neither. When a cell has a color, the foreground and the background colors of
/// each line follow, as one [`color_code`] per cell.
fn screen(
    tree: &Tree,
    directory: DirectoryId,
    options: &BrowserOptions,
    keys: &[Key],
    size: (u16, u16),
) -> String {
    let browser = Browser::new(tree, directory, options, &mut |_| {});
    run(browser, tree.clone(), options, keys, size, None)
}

/// Open the root of `tree` as [`screen`] does, where the command of `--delete-command` leaves
/// `stat` at the path of the entry, or nothing without it.
fn command_screen(
    tree: &Tree,
    options: &BrowserOptions,
    keys: &[Key],
    stat: Option<Stat>,
) -> String {
    let browser = Browser::new(tree, root(tree), options, &mut |_| {});
    run(browser, tree.clone(), options, keys, (80, 12), stat)
}

/// Open the binary export `fixture` of `crates/duit/tests/fixtures` as [`screen`] opens a tree,
/// reading it from the file one directory at a time.
fn file_screen(fixture: &str, options: &BrowserOptions, keys: &[Key], size: (u16, u16)) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../duit/tests/fixtures")
        .join(fixture);
    let file = File::open(path).expect("export is opened");
    let reader = BinaryReader::open(file).expect("index is read");
    let (browser, tree) = Browser::open(reader, options, &mut |_| {})
        .expect("root is read")
        .expect("root is a directory");
    run(browser, tree, options, keys, size, None)
}

/// Draw `browser` on a terminal of `width` by `height`, and again after each of `keys`, and render
/// the last screen as [`screen`] describes.
///
/// A deletion runs only in a temporary directory. Its windows that wait for a key take the next
/// of `keys`. When none is left, the deletion is aborted, and its window is the last screen. The
/// command of `--delete-command` runs, and leaves `stat` at the path of the entry.
fn run(
    mut browser: Browser,
    mut tree: Tree,
    options: &BrowserOptions,
    keys: &[Key],
    (width, height): (u16, u16),
    stat: Option<Stat>,
) -> String {
    let mut options = options.clone();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal opens");
    terminal
        .draw(|frame| browser.draw(frame, &tree, &options))
        .expect("browser draws");
    let mut keys = keys.iter().copied();
    while let Some(key) = keys.next() {
        let action = browser
            .key_input(&mut tree, &mut options, key, height, &mut |_| {})
            .expect("key is handled");
        if action == Action::Delete {
            let root = tree.name(tree.directory(root(&tree)).entry);
            assert!(
                Path::new(OsStr::from_bytes(root)).starts_with(env::temp_dir()),
                "deletes only in a temporary directory"
            );
            if !options.delete_command.is_empty() {
                browser
                    .delete_command(&mut tree, &options, &mut |command, _| {
                        assert!(command.status().expect("command runs").success());
                        stat
                    })
                    .expect("tree takes the entry");
                browser.load_directory(&tree, &options, &mut |_| {});
                terminal
                    .draw(|frame| browser.draw(frame, &tree, &options))
                    .expect("browser draws");
                continue;
            }
            let mut waiting = false;
            browser.delete(&mut tree, &mut |deletion, tree, block| {
                if block {
                    terminal
                        .draw(|frame| deletion.draw(frame, tree, &options))
                        .expect("deletion draws");
                    if let Some(key) = keys.next() {
                        deletion.key_input(key);
                    } else {
                        waiting = true;
                        deletion.key_input(Key::Char('q'));
                    }
                }
            });
            if waiting {
                return render(&terminal);
            }
            browser.load_directory(&tree, &options, &mut |_| {});
        }
        terminal
            .draw(|frame| browser.draw(frame, &tree, &options))
            .expect("browser draws");
    }
    render(&terminal)
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
        let mut tree = tree.clone();
        let mut browser = Browser::new(&tree, root(&tree), &options, &mut |_| {});
        keys.iter()
            .map(|&key| {
                browser
                    .key_input(&mut tree, &mut options, key, 24, &mut |_| {})
                    .expect("key is handled")
                    == Action::Quit
            })
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

/// Refreshing an import is disabled unless `--enable-refresh` enables it, which a message says.
#[test]
fn refresh_disabled_imported() {
    let tree = tree_of(SOURCE, false);
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &[Key::Char('r')], (80, 12)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 KiB S   4.0 KiB [###########] /src                                      "
    "!   4.0 Ki┌── Message ───────────────────────────────────────────────┐          "
    "e   4.0 Ki│                                                          │          "
    "    4.0 Ki│ Refresh feature disabled.                                │          "
    "H   4.0 Ki│ Re-run with --enable-refresh to enable this option.      │          "
    "    0.0   │                                                          │          "
    "    0.0   │                                Press any key to continue │          "
    "    0.0   └──────────────────────────────────────────────────────────┘rol \x01 é"
    "@   0.0   B             [           ]  socket                                   "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    .............BBBBBBBBB..........................................................
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

/// `r` asks the main loop to scan the open directory again, which [`Browser::directory`] gives.
#[test]
fn refresh() {
    let mut tree = tree_of(SOURCE, false);
    let mut options = BrowserOptions::default();
    let mut browser = Browser::new(&tree, root(&tree), &options, &mut |_| {});
    let mut action = |browser: &mut Browser, tree: &mut Tree, key| {
        browser
            .key_input(tree, &mut options, key, 12, &mut |_| {})
            .expect("key is handled")
    };
    assert_eq!(
        action(&mut browser, &mut tree, Key::Char('r')),
        Action::Refresh
    );
    assert_eq!(browser.directory(), root(&tree));
    assert_eq!(action(&mut browser, &mut tree, Key::Enter), Action::Browse);
    assert_eq!(
        action(&mut browser, &mut tree, Key::Char('r')),
        Action::Refresh
    );
    assert_eq!(browser.directory(), subdirectory(&tree, b"src"));
}

/// Refreshing a scan is disabled with `--disable-refresh`.
#[test]
fn refresh_disabled() {
    let tree = tree_of(SOURCE, false);
    let options = BrowserOptions {
        can_refresh: false,
        ..BrowserOptions::default()
    };
    assert_snapshot!(screen(&tree, root(&tree), &options, &[Key::Char('r')], (80, 12)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help                   "
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 KiB S   4.0 KiB [###########] /src                                      "
    "!   4.0 Ki┌── Message ───────────────────────────────────────────────┐          "
    "e   4.0 Ki│                                                          │          "
    "    4.0 Ki│ Directory refresh feature disabled.                      │          "
    "H   4.0 Ki│                                                          │          "
    "    0.0   │                                Press any key to continue │          "
    "    0.0   └──────────────────────────────────────────────────────────┘          "
    "    0.0   B             [           ]  quote " backslash \...\x09 control \x01 é"
    "@   0.0   B             [           ]  socket                                   "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    .............BBBBBBBBB..........................................................
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

/// The shell is disabled with `--disable-shell`, and the message names `--enable-shell` also for a
/// scan.
#[test]
fn shell_disabled() {
    let tree = tree_of(SOURCE, false);
    let options = BrowserOptions {
        can_shell: false,
        ..BrowserOptions::default()
    };
    assert_snapshot!(screen(&tree, root(&tree), &options, &[Key::Char('b')], (80, 12)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help                   "
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 KiB S   4.0 KiB [###########] /src                                      "
    "!   4.0 Ki┌── Message ───────────────────────────────────────────────┐          "
    "e   4.0 Ki│                                                          │          "
    "    4.0 Ki│ Shell feature disabled.                                  │          "
    "H   4.0 Ki│ Re-run with --enable-shell to enable this option.        │          "
    "    0.0   │                                                          │          "
    "    0.0   │                                Press any key to continue │          "
    "    0.0   └──────────────────────────────────────────────────────────┘rol \x01 é"
    "@   0.0   B             [           ]  socket                                   "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    .............BBBBBBBBB..........................................................
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

/// Deleting from an import is disabled unless `--enable-delete` enables it, which a message says.
#[test]
fn delete_disabled_imported() {
    let tree = tree_of(SOURCE, false);
    assert_snapshot!(screen(&tree, root(&tree), &imported(), &[Key::Char('d')], (80, 12)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [imported]"
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 KiB S   4.0 KiB [###########] /src                                      "
    "!   4.0 Ki┌── Message ───────────────────────────────────────────────┐          "
    "e   4.0 Ki│                                                          │          "
    "    4.0 Ki│ File deletion is disabled.                               │          "
    "H   4.0 Ki│ Re-run with --enable-delete to enable this option.       │          "
    "    0.0   │                                                          │          "
    "    0.0   │                                Press any key to continue │          "
    "    0.0   └──────────────────────────────────────────────────────────┘rol \x01 é"
    "@   0.0   B             [           ]  socket                                   "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    .............BBBBBBBBB..........................................................
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

/// Deleting from a scan is disabled with `--disable-delete`, also on the row of the parent
/// directory. In a root without entries, `d` does nothing.
#[test]
fn delete_disabled() {
    let tree = tree_of(SOURCE, false);
    let options = BrowserOptions {
        can_delete: false,
        ..BrowserOptions::default()
    };
    let keys = [Key::Down, Key::Down, Key::Enter, Key::Char('d')];
    assert_snapshot!(screen(&tree, root(&tree), &options, &keys, (80, 12)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help         [readonly]"
    "--- /home/user/project/empty ---------------------------------------------------"
    "                          /..                                                   "
    "          ┌── Message ───────────────────────────────────────────────┐          "
    "          │                                                          │          "
    "          │ File deletion is disabled.                               │          "
    "          │                                                          │          "
    "          │                                Press any key to continue │          "
    "          └──────────────────────────────────────────────────────────┘          "
    "                                                                                "
    "                                                                                "
    "*Total disk usage:   4.0 KiB   Apparent size:   4.0 KiB   Items: 0              "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    .............BBBBBBBBB..........................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
    let empty = tree_of(
        br#"[1,2,{"progname":"ncdu","progver":"2.9.2"},[{"name":"/empty"}]]"#,
        false,
    );
    assert_eq!(
        screen(&empty, root(&empty), &options, &[Key::Char('d')], (80, 12)),
        screen(&empty, root(&empty), &options, &[], (80, 12))
    );
}

/// A tree to delete from, whose root is named `[DIR]`: three files, and a directory with a file and
/// a directory with a file below it.
const DELETE: &str = r#"[1,2,{"progname":"ncdu","progver":"2.9.2"},
[{"name":"[DIR]","asize":4096,"dsize":4096},
{"name":"a.txt","asize":1000,"dsize":4096},
{"name":"b.txt","asize":2000,"dsize":8192},
{"name":"c.txt","asize":3000,"dsize":12288},
[{"name":"top","asize":4096,"dsize":4096},
[{"name":"dir","asize":4096,"dsize":4096},
{"name":"f","asize":5000,"dsize":8192},
[{"name":"deep","asize":4096,"dsize":4096},{"name":"g","asize":6000,"dsize":8192}]],
{"name":"keep","asize":100,"dsize":4096}]]]"#;

/// The files of [`DELETE`].
const DELETE_FILES: [&str; 6] = [
    "a.txt",
    "b.txt",
    "c.txt",
    "top/dir/f",
    "top/dir/deep/g",
    "top/keep",
];

/// A temporary directory with the empty files `files`, each with the directories above it, and the
/// tree of the JSON export `export`, whose root `[DIR]` is named after the directory.
fn on_disk(export: &str, files: &[&str]) -> (TempDir, Tree) {
    let directory = TempDir::new().expect("temporary directory is created");
    for file in files {
        let path = directory.path().join(file);
        fs::create_dir_all(path.parent().expect("file is in a directory"))
            .expect("directory is created");
        fs::write(&path, b"").expect("file is written");
    }
    let root = directory.path().to_str().expect("path is UTF-8");
    let tree = tree_of(export.replace("[DIR]", root).as_bytes(), false);
    (directory, tree)
}

/// `screen` with the path of `directory` replaced by `[DIR]`, and the line of the path filled with
/// `-` to its width again.
fn redact_directory(screen: &str, directory: &TempDir) -> String {
    let path = directory.path().to_str().expect("path is UTF-8");
    let fill = "-".repeat(path.chars().count() - "[DIR]".len());
    let mut redacted = String::new();
    for line in screen.lines() {
        if let Some(start) = line.strip_suffix('"')
            && start.contains(path)
        {
            let _ = writeln!(redacted, "{}{fill}\"", start.replacen(path, "[DIR]", 1));
        } else {
            let _ = writeln!(redacted, "{line}");
        }
    }
    redacted
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

/// `d` asks before it deletes, with "no" selected, over the listing. A directory is deleted with
/// all of its contents.
#[test]
fn delete_confirmation() {
    let tree = tree_of(SOURCE, false);
    let options = BrowserOptions::default();
    assert_snapshot!(screen(&tree, root(&tree), &options, &[Key::Char('d')], (80, 12)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help                   "
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 KiB S   4.0 KiB [###########] /src                                      "
    "!   4.0 Ki┌── Confirm delete ────────────────────────────────────────┐          "
    "e   4.0 Ki│ Are you sure you want to delete "src"                    │          "
    "    4.0 Ki│                 and all of its contents?                 │          "
    "H   4.0 Ki│                                                          │          "
    "    0.0   │              yes       no    don't ask me again          │          "
    "    0.0   └──────────────────────────────────────────────────────────┘          "
    "    0.0   B             [           ]  quote " backslash \...\x09 control \x01 é"
    "@   0.0   B             [           ]  socket                                   "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    .............BBBBBBBBBBBBBBBB...................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ...................................RR...........................................
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// Left and `h` select the button on the left, and Right and `l` the one on the right. A file is
/// deleted without its contents.
#[test]
fn delete_confirmation_keys() {
    let tree = tree_of(SOURCE, false);
    let options = BrowserOptions::default();
    let keys = [
        Key::Down,
        Key::Down,
        Key::Down,
        Key::Char('d'),
        Key::Right,
        Key::Char('h'),
        Key::Left,
    ];
    assert_snapshot!(screen(&tree, root(&tree), &options, &keys, (80, 12)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help                   "
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 KiB S   4.0 KiB [###########] /src                                      "
    "!   4.0 Ki┌── Confirm delete ────────────────────────────────────────┐          "
    "e   4.0 Ki│ Are you sure you want to delete "README.md"?             │          "
    "    4.0 Ki│                                                          │          "
    "H   4.0 Ki│                                                          │          "
    "    0.0   │              yes       no    don't ask me again          │          "
    "    0.0   └──────────────────────────────────────────────────────────┘          "
    "    0.0   B             [           ]  quote " backslash \...\x09 control \x01 é"
    "@   0.0   B             [           ]  socket                                   "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    .............BBBBBBBBBBBBBBBB...................................................
    ................................................................................
    RRRRRRRRRR............................................................RRRRRRRRRR
    ................................................................................
    .........................RRR....................................................
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// Enter on "no", or `q` on any button, closes the confirmation and deletes nothing.
#[test]
fn delete_cancel() {
    let (directory, tree) = on_disk(DELETE, &DELETE_FILES);
    let options = BrowserOptions::default();
    let before = screen(&tree, root(&tree), &options, &[], (80, 12));
    let cases: [&[Key]; 4] = [
        &[Key::Char('d'), Key::Enter],
        &[Key::Char('d'), Key::Char('q')],
        &[Key::Char('d'), Key::Left, Key::Char('l'), Key::Enter],
        &[Key::Char('d'), Key::Left, Key::Char('q')],
    ];
    for keys in cases {
        assert_eq!(
            screen(&tree, root(&tree), &options, keys, (80, 12)),
            before,
            "{keys:?}"
        );
    }
    for file in DELETE_FILES {
        assert!(directory.path().join(file).exists(), "{file}");
    }
}

/// "yes" deletes the file, takes its sizes from the totals, and puts the cursor on the entry after
/// it.
#[test]
fn delete_file() {
    let (directory, tree) = on_disk(DELETE, &DELETE_FILES);
    let keys = [Key::Down, Key::Down, Key::Char('d'), Key::Left, Key::Enter];
    let screen = screen(
        &tree,
        root(&tree),
        &BrowserOptions::default(),
        &keys,
        (80, 12),
    );
    assert_snapshot!(redact_directory(&screen, &directory), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help                   "
    "--- [DIR] ----------------------------------------------------------------------"
    "   32.0 KiB [###########] /top                                                  "
    "   12.0 KiB [####       ]  c.txt                                                "
    "    4.0 KiB [#          ]  a.txt                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "*Total disk usage:  52.0 KiB   Apparent size:  30.7 KiB   Items: 8              "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
    assert!(!directory.path().join("b.txt").exists());
}

/// Deleting a directory deletes everything below it, and takes its sizes and items from the
/// totals of its parent and of the root.
#[test]
fn delete_directory() {
    let (directory, tree) = on_disk(DELETE, &DELETE_FILES);
    let keys = [
        Key::Enter,
        Key::Down,
        Key::Char('d'),
        Key::Left,
        Key::Enter,
        Key::Left,
    ];
    let screen = screen(
        &tree,
        root(&tree),
        &BrowserOptions::default(),
        &keys,
        (80, 12),
    );
    assert_snapshot!(redact_directory(&screen, &directory), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help                   "
    "--- [DIR] ----------------------------------------------------------------------"
    "   12.0 KiB [###########]  c.txt                                                "
    "    8.0 KiB [########   ] /top                                                  "
    "    8.0 KiB [########   ]  b.txt                                                "
    "    4.0 KiB [####       ]  a.txt                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "*Total disk usage:  36.0 KiB   Apparent size:  14.0 KiB   Items: 5              "
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
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
    assert!(!directory.path().join("top/dir").exists());
    assert!(directory.path().join("top/keep").exists());
}

/// After the last entry is deleted, the cursor is on the entry before it, and after the only entry,
/// on the row of the parent directory.
#[test]
fn delete_cursor() {
    let (directory, tree) = on_disk(DELETE, &DELETE_FILES);
    let keys = [
        Key::Enter,
        Key::End,
        Key::Char('d'),
        Key::Left,
        Key::Enter,
        Key::Char('d'),
        Key::Left,
        Key::Enter,
    ];
    let screen = screen(
        &tree,
        root(&tree),
        &BrowserOptions::default(),
        &keys,
        (80, 12),
    );
    assert_snapshot!(redact_directory(&screen, &directory), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help                   "
    "--- [DIR]/top ------------------------------------------------------------------"
    "                          /..                                                   "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "*Total disk usage:   4.0 KiB   Apparent size:   4.0 KiB   Items: 0              "
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
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
    assert!(!directory.path().join("top/keep").exists());
    assert!(!directory.path().join("top/dir").exists());
}

/// "don't ask me again" deletes, and the deletes after it are not confirmed, as with
/// `--no-confirm-delete`.
#[test]
fn delete_without_confirmation() {
    let (directory, tree) = on_disk(DELETE, &DELETE_FILES);
    let keys = [
        Key::Down,
        Key::Char('d'),
        Key::Right,
        Key::Enter,
        Key::Char('d'),
    ];
    let rendered = screen(
        &tree,
        root(&tree),
        &BrowserOptions::default(),
        &keys,
        (80, 12),
    );
    assert_snapshot!(redact_directory(&rendered, &directory), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help                   "
    "--- [DIR] ----------------------------------------------------------------------"
    "   32.0 KiB [###########] /top                                                  "
    "    4.0 KiB [#          ]  a.txt                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "*Total disk usage:  40.0 KiB   Apparent size:  27.8 KiB   Items: 7              "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
    assert!(!directory.path().join("c.txt").exists());
    assert!(!directory.path().join("b.txt").exists());

    let (directory, tree) = on_disk(DELETE, &DELETE_FILES);
    let options = BrowserOptions {
        confirm_delete: false,
        ..BrowserOptions::default()
    };
    screen(&tree, root(&tree), &options, &[Key::Char('d')], (80, 12));
    assert!(!directory.path().join("top").exists());
}

/// Deleting a hard link takes its inode from the totals, and the other link of the inode is counted
/// again, still as shared, because its link count stays 2.
#[test]
fn delete_hard_link() {
    let export = r#"[1,2,{"progname":"ncdu","progver":"2.9.2"},
[{"name":"[DIR]","asize":4096,"dsize":4096},
{"name":"x","asize":100,"dsize":4096,"ino":5,"hlnkc":true,"nlink":2},
[{"name":"sub","asize":4096,"dsize":4096},
{"name":"y","asize":100,"dsize":4096,"ino":5,"hlnkc":true,"nlink":2}]]]"#;
    let (directory, tree) = on_disk(export, &["x", "sub/y"]);
    let keys = [Key::Down, Key::Char('d'), Key::Left, Key::Enter];
    let screen = screen(
        &tree,
        root(&tree),
        &BrowserOptions::default(),
        &keys,
        (80, 12),
    );
    assert_snapshot!(redact_directory(&screen, &directory), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help                   "
    "--- [DIR] ----------------------------------------------------------------------"
    "    8.0 KiB S   4.0 KiB [###########] /sub                                      "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "*Total disk usage:   8.0 KiB   Apparent size:   8.0 KiB   Items: 2              "
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
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
    assert!(!directory.path().join("x").exists());
}

/// A read-only directory with two files, which cannot be deleted.
const READ_ONLY: &str = r#"[1,2,{"progname":"ncdu","progver":"2.9.2"},
[{"name":"[DIR]","asize":4096,"dsize":4096},
[{"name":"ro","asize":4096,"dsize":4096},
{"name":"a","asize":10,"dsize":4096},
{"name":"b","asize":10,"dsize":4096}]]]"#;

/// An entry that cannot be deleted opens an error window on an erased screen, with the path from
/// below the root, and "abort" selected.
#[test]
fn delete_error() {
    let (directory, tree) = on_disk(READ_ONLY, &["ro/a", "ro/b"]);
    let _read_only = ReadOnly::new(directory.path().join("ro"));
    let keys = [Key::Char('d'), Key::Left, Key::Enter];
    let screen = screen(
        &tree,
        root(&tree),
        &BrowserOptions::default(),
        &keys,
        (80, 12),
    );
    assert_snapshot!(screen, @r#"
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "          ┌── Error ─────────────────────────────────────────────────┐          "
    "          │ Error deleting ro/b                                      │          "
    "          │   Access denied                                          │          "
    "          │                                                          │          "
    "          │             abort    ignore    ignore all                │          "
    "          └──────────────────────────────────────────────────────────┘          "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    ................................................................................
    ................................................................................
    ................................................................................
    .............BBBBBBB............................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ........................RRRRR...................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    "#);
}

/// "abort" and `q` stop the deletion, "ignore" goes on to the next entry, where the error window
/// opens with "ignore" selected, and "ignore all" goes on without error windows, also in the next
/// deletion. A directory that still holds an entry is kept without an error.
#[test]
fn delete_error_options() {
    let (directory, tree) = on_disk(READ_ONLY, &["ro/a", "ro/b"]);
    let _read_only = ReadOnly::new(directory.path().join("ro"));
    let options = BrowserOptions::default();
    let before = screen(&tree, root(&tree), &options, &[], (80, 12));
    let confirm = [Key::Char('d'), Key::Left, Key::Enter];
    let cases: [&[Key]; 3] = [
        &[Key::Enter],
        &[Key::Char('q')],
        &[
            Key::Right,
            Key::Right,
            Key::Enter,
            Key::Char('d'),
            Key::Left,
            Key::Enter,
        ],
    ];
    for keys in cases {
        let keys = [&confirm[..], keys].concat();
        assert_eq!(
            screen(&tree, root(&tree), &options, &keys, (80, 12)),
            before,
            "{keys:?}"
        );
    }
    let keys = [&confirm[..], &[Key::Char('l'), Key::Enter]].concat();
    assert_snapshot!(screen(&tree, root(&tree), &options, &keys, (80, 12)), @r#"
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "          ┌── Error ─────────────────────────────────────────────────┐          "
    "          │ Error deleting ro/a                                      │          "
    "          │   Access denied                                          │          "
    "          │                                                          │          "
    "          │             abort    ignore    ignore all                │          "
    "          └──────────────────────────────────────────────────────────┘          "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    ................................................................................
    ................................................................................
    ................................................................................
    .............BBBBBBB............................................................
    ................................................................................
    ................................................................................
    ................................................................................
    .................................RRRRRR.........................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    "#);
    assert!(directory.path().join("ro/a").exists());
    assert!(directory.path().join("ro/b").exists());
}

/// A directory that is not empty on disk, although it is empty in the tree, opens the error window.
/// The window joins the directory to the last entry that the deletion went through, which is the
/// directory itself when the tree has no entries in it.
#[test]
fn delete_error_not_empty() {
    let export = r#"[1,2,{"progname":"ncdu","progver":"2.9.2"},
[{"name":"[DIR]","asize":4096,"dsize":4096},
[{"name":"full","asize":4096,"dsize":4096}]]]"#;
    let (directory, tree) = on_disk(export, &["full/unknown"]);
    let keys = [Key::Char('d'), Key::Left, Key::Enter];
    let screen = screen(
        &tree,
        root(&tree),
        &BrowserOptions::default(),
        &keys,
        (80, 12),
    );
    assert_snapshot!(screen, @r#"
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "          ┌── Error ─────────────────────────────────────────────────┐          "
    "          │ Error deleting full/full                                 │          "
    "          │   Directory not empty                                    │          "
    "          │                                                          │          "
    "          │             abort    ignore    ignore all                │          "
    "          └──────────────────────────────────────────────────────────┘          "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    ................................................................................
    ................................................................................
    ................................................................................
    .............BBBBBBB............................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ........................RRRRR...................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    "#);
    assert!(directory.path().join("full/unknown").exists());
}

/// While it deletes, the deletion draws on an erased screen the path of the entry that it deletes,
/// from below the root, here a directory in the root.
#[test]
fn delete_progress() {
    let (directory, mut tree) = on_disk(DELETE, &DELETE_FILES);
    let mut options = BrowserOptions {
        confirm_delete: false,
        ..BrowserOptions::default()
    };
    let mut browser = Browser::new(&tree, root(&tree), &options, &mut |_| {});
    let action = browser
        .key_input(&mut tree, &mut options, Key::Char('d'), 12, &mut |_| {})
        .expect("key is handled");
    assert_eq!(action, Action::Delete);
    let mut terminal = Terminal::new(TestBackend::new(80, 12)).expect("terminal opens");
    let mut drawn = false;
    browser.delete(&mut tree, &mut |deletion, tree, _| {
        if !drawn {
            terminal
                .draw(|frame| deletion.draw(frame, tree, &options))
                .expect("deletion draws");
            drawn = true;
        }
    });
    assert_snapshot!(render(&terminal), @r#"
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "          ┌── Deleting... ───────────────────────────────────────────┐          "
    "          │                                                          │          "
    "          │ /top                                                     │          "
    "          │                                                          │          "
    "          │                                        Press q to abort  │          "
    "          └──────────────────────────────────────────────────────────┘          "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    ................................................................................
    ................................................................................
    ................................................................................
    .............BBBBBBBBBBBBB......................................................
    ................................................................................
    ................................................................................
    ................................................................................
    .........................................................B......................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    "#);
    assert!(!directory.path().join("top").exists());
}

/// With `--delete-command`, `d` asks to run the command on the entry, both shortened, the same for
/// a directory as for a file.
#[test]
fn delete_command_confirmation() {
    let tree = tree_of(SOURCE, false);
    let options = BrowserOptions {
        delete_command: b"trash-put --verbose --trash-dir=/tmp/trash".to_vec(),
        ..BrowserOptions::default()
    };
    let mut keys = [Key::Down; 8];
    keys[7] = Key::Char('d');
    assert_snapshot!(screen(&tree, root(&tree), &options, &keys, (80, 12)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help                   "
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 KiB S   4.0 KiB [###########] /src                                      "
    "!   4.0 Ki┌── Confirm delete ────────────────────────────────────────┐          "
    "e   4.0 Ki│ Are you sure you want to run "trash-put -...=/tmp/trash" │          "
    "    4.0 Ki│   on "quote " backslash \ tab \x09 control \x01 é"       │          "
    "H   4.0 Ki│                                                          │          "
    "    0.0   │              yes       no    don't ask me again          │          "
    "    0.0   └──────────────────────────────────────────────────────────┘          "
    "    0.0   B             [           ]  quote " backslash \...\x09 control \x01 é"
    "@   0.0   B             [           ]  socket                                   "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    .............BBBBBBBBBBBBBBBB...................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ...................................RR...........................................
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// When nothing is left at the path after the command, the entry is taken out of the tree and its
/// sizes from the totals, and the cursor goes to the entry after it. duit does not delete the file
/// itself.
#[test]
fn delete_command_removed() {
    let (directory, tree) = on_disk(DELETE, &DELETE_FILES);
    let options = BrowserOptions {
        delete_command: b"true".to_vec(),
        ..BrowserOptions::default()
    };
    let keys = [Key::Down, Key::Down, Key::Char('d'), Key::Left, Key::Enter];
    let screen = command_screen(&tree, &options, &keys, None);
    assert_snapshot!(redact_directory(&screen, &directory), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help                   "
    "--- [DIR] ----------------------------------------------------------------------"
    "   32.0 KiB [###########] /top                                                  "
    "   12.0 KiB [####       ]  c.txt                                                "
    "    4.0 KiB [#          ]  a.txt                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "*Total disk usage:  52.0 KiB   Apparent size:  30.7 KiB   Items: 8              "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
    assert!(directory.path().join("b.txt").exists());
}

/// A file that the command leaves at the path is added again with the sizes that it has now, in
/// the totals too, and the cursor stays on it.
#[test]
fn delete_command_changed() {
    let (directory, tree) = on_disk(DELETE, &DELETE_FILES);
    let options = BrowserOptions {
        delete_command: b"true".to_vec(),
        confirm_delete: false,
        ..BrowserOptions::default()
    };
    let stat = Stat {
        entry_type: EntryType::Regular,
        blocks: 40,
        size: 20_000,
        ..Stat::default()
    };
    let keys = [Key::Down, Key::Down, Key::Char('d')];
    let screen = command_screen(&tree, &options, &keys, Some(stat));
    assert_snapshot!(redact_directory(&screen, &directory), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help                   "
    "--- [DIR] ----------------------------------------------------------------------"
    "   32.0 KiB [###########] /top                                                  "
    "   20.0 KiB [########   ]  b.txt                                                "
    "   12.0 KiB [####       ]  c.txt                                                "
    "    4.0 KiB [#          ]  a.txt                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "*Total disk usage:  72.0 KiB   Apparent size:  50.3 KiB   Items: 9              "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
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

/// A hard link that the command leaves at the path is added as a hard link, whose sizes the
/// totals count once for its inode.
#[test]
fn delete_command_hard_link() {
    let (directory, tree) = on_disk(DELETE, &DELETE_FILES);
    let options = BrowserOptions {
        delete_command: b"true".to_vec(),
        confirm_delete: false,
        ..BrowserOptions::default()
    };
    let stat = Stat {
        entry_type: EntryType::HardLink,
        blocks: 16,
        size: 5000,
        inode: 7,
        link_count: 2,
        ..Stat::default()
    };
    let keys = [Key::Down, Key::Down, Key::Char('d')];
    let screen = command_screen(&tree, &options, &keys, Some(stat));
    assert_snapshot!(redact_directory(&screen, &directory), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help                   "
    "--- [DIR] ----------------------------------------------------------------------"
    "   32.0 KiB [###########] /top                                                  "
    "   12.0 KiB [####       ]  c.txt                                                "
    "H   8.0 KiB [###        ]  b.txt                                                "
    "    4.0 KiB [#          ]  a.txt                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "                                                                                "
    "*Total disk usage:  60.0 KiB   Apparent size:  35.6 KiB   Items: 9              "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
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
    let mut tree = tree.clone();
    let mut browser = Browser::new(&tree, root, &options, &mut |_| {});
    let mut key_input = |key| {
        browser
            .key_input(&mut tree, &mut options, key, 18, &mut |_| {})
            .expect("key is handled")
            == Action::Quit
    };
    key_input(i);
    assert!(!key_input(Key::Char('q')));
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
    let mut tree = tree.clone();
    let mut browser = Browser::new(&tree, root(&tree), &options, &mut |_| {});
    let mut key_input = |key| {
        browser
            .key_input(&mut tree, &mut options, key, 18, &mut |_| {})
            .expect("key is handled")
            == Action::Quit
    };
    key_input(help);
    assert!(!key_input(Key::Char('q')));
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

/// The options of the color tests: every column of the listing, in `color`.
fn colored(color: ColorScheme) -> BrowserOptions {
    BrowserOptions {
        show_percent: true,
        show_item_count: true,
        show_mtime: true,
        color,
        ..imported()
    }
}

/// `--color off` draws the listing with attributes only, and no colors.
#[test]
fn color_off() {
    let tree = tree_of(SOURCE, false);
    let options = colored(ColorScheme::Off);
    assert_snapshot!(screen(&tree, root(&tree), &options, &[], (100, 8)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help                             [imported]"
    "--- /home/user/project -----------------------------------------------------------------------------"
    "   16.0 KiB S   4.0 KiB [ 50.0% ##############]      2                   no mtime /src              "
    "!   4.0 KiB             [ 12.5% ####          ]      0                   no mtime /unreadable       "
    "e   4.0 KiB             [ 12.5% ####          ]      0                   no mtime /empty            "
    "    4.0 KiB             [ 12.5% ####          ]                          no mtime  README.md        "
    "H   4.0 KiB             [ 12.5% ####          ]                          no mtime  also-linked      "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15                                 "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ....................................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ....................................................................................................
    ....................................................................................................
    ....................................................................................................
    ....................................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// `--color dark` colors the header, the footer, and each column of the listing, on green in the
/// cursor row. Without an mtime, the row of a directory with fewer than 1000 items draws "no mtime"
/// in the style of its item count, which ncdu leaves set.
#[test]
fn color_dark() {
    let tree = tree_of(SOURCE, false);
    let options = colored(ColorScheme::Dark);
    assert_snapshot!(screen(&tree, root(&tree), &options, &[], (100, 8)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help                             [imported]"
    "--- /home/user/project -----------------------------------------------------------------------------"
    "   16.0 KiB S   4.0 KiB [ 50.0% ##############]      2                   no mtime /src              "
    "!   4.0 KiB             [ 12.5% ####          ]      0                   no mtime /unreadable       "
    "e   4.0 KiB             [ 12.5% ####          ]      0                   no mtime /empty            "
    "    4.0 KiB             [ 12.5% ####          ]                          no mtime  README.md        "
    "H   4.0 KiB             [ 12.5% ####          ]                          no mtime  also-linked      "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15                                 "
    ...................................................B................................................
    ....BBBBBBBBBBBBBBBBBB..............................................................................
    BBBBBBBBBBB...BBBBBBBBBBBBBBBBBB..............BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB
    ..BBBBB..................BBBBB..................BBBBBB..BBBBBBBBBBBBBBBBBBBBBBBBB.BBBBBBBBBBB.......
    ..BBBBB..................BBBBB..................BBBBBB..BBBBBBBBBBBBBBBBBBBBBBBBB.BBBBBB............
    ..BBBBB..................BBBBB......................................................................
    ..BBBBB..................BBBBB......................................................................
    .BBBBBBBBBBBBBBBBBBBBBBB......................BBBBB..............BB.................................
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkykkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    ....bbbbbbbbbbbbbbbbbb..............................................................................
    wwyyyyywwwwrrryyyyywwwwwwyyyyywwmmmmmmmmmmmmmmwwyyyyyywwyyyyyyyyyyyyyyyyyyyyyyyyywbbbbwwwwwwwwwwwwww
    r.yyyyy..................yyyyy..mmmmmmmmmmmmmm..yyyyyy..yyyyyyyyyyyyyyyyyyyyyyyyy.bbbbbbbbbbb.......
    r.yyyyy..................yyyyy..mmmmmmmmmmmmmm..yyyyyy..yyyyyyyyyyyyyyyyyyyyyyyyy.bbbbbb............
    ..yyyyy..................yyyyy..mmmmmmmmmmmmmm......................................................
    r.yyyyy..................yyyyy..mmmmmmmmmmmmmm......................................................
    kkkkkkkkkkkkkkkkkkkyyyyykkkkkkkkkkkkkkkkkkkkkkyyyyykkkkkkkkkkkkkkyykkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
    ....................................................................................................
    gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg
    ....................................................................................................
    ....................................................................................................
    ....................................................................................................
    ....................................................................................................
    cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
    "#);
}

/// `--color dark-bg` draws the cells that have no color in `--color dark` white on black,
/// including the empty ones.
#[test]
fn color_dark_background() {
    let tree = tree_of(SOURCE, false);
    let options = colored(ColorScheme::DarkBackground);
    assert_snapshot!(screen(&tree, root(&tree), &options, &[], (100, 8)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help                             [imported]"
    "--- /home/user/project -----------------------------------------------------------------------------"
    "   16.0 KiB S   4.0 KiB [ 50.0% ##############]      2                   no mtime /src              "
    "!   4.0 KiB             [ 12.5% ####          ]      0                   no mtime /unreadable       "
    "e   4.0 KiB             [ 12.5% ####          ]      0                   no mtime /empty            "
    "    4.0 KiB             [ 12.5% ####          ]                          no mtime  README.md        "
    "H   4.0 KiB             [ 12.5% ####          ]                          no mtime  also-linked      "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15                                 "
    ...................................................B................................................
    ....BBBBBBBBBBBBBBBBBB..............................................................................
    BBBBBBBBBBB...BBBBBBBBBBBBBBBBBB..............BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB
    ..BBBBB..................BBBBB..................BBBBBB..BBBBBBBBBBBBBBBBBBBBBBBBB.BBBBBBBBBBB.......
    ..BBBBB..................BBBBB..................BBBBBB..BBBBBBBBBBBBBBBBBBBBBBBBB.BBBBBB............
    ..BBBBB..................BBBBB......................................................................
    ..BBBBB..................BBBBB......................................................................
    .BBBBBBBBBBBBBBBBBBBBBBB......................BBBBB..............BB.................................
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkykkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    wwwwbbbbbbbbbbbbbbbbbbwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    wwyyyyywwwwrrryyyyywwwwwwyyyyywwmmmmmmmmmmmmmmwwyyyyyywwyyyyyyyyyyyyyyyyyyyyyyyyywbbbbwwwwwwwwwwwwww
    rwyyyyywwwwwwwwwwwwwwwwwwyyyyywwmmmmmmmmmmmmmmwwyyyyyywwyyyyyyyyyyyyyyyyyyyyyyyyywbbbbbbbbbbbwwwwwww
    rwyyyyywwwwwwwwwwwwwwwwwwyyyyywwmmmmmmmmmmmmmmwwyyyyyywwyyyyyyyyyyyyyyyyyyyyyyyyywbbbbbbwwwwwwwwwwww
    wwyyyyywwwwwwwwwwwwwwwwwwyyyyywwmmmmmmmmmmmmmmwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    rwyyyyywwwwwwwwwwwwwwwwwwyyyyywwmmmmmmmmmmmmmmwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    kkkkkkkkkkkkkkkkkkkyyyyykkkkkkkkkkkkkkkkkkkkkkyyyyykkkkkkkkkkkkkkyykkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
    "#);
}

/// The info window of a hard link in `--color dark`: a blue title, the selected tab on cyan, and
/// the numbers in yellow.
#[test]
fn color_dark_info() {
    let tree = tree_of(SOURCE, true);
    let options = BrowserOptions {
        si: true,
        extended: true,
        color: ColorScheme::Dark,
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
    ...................................................B............................
    ....BBBBBBBBBBBBBBBBBB..........................................................
    ..BBBBB......BBBBB..................BBBB........................................
    ..BBBBB......BBBBBBBBBBB...........................B........B...................
    ..BBBBB.........................................................................
    ..BBBBB......BBBBBB.............................................................
    .BBBBBBBBB...BBBBBB..........BBBBBBB......BBBBBB......................BBBBBBBBBB
    ..BBBBB......BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB...........................
    ..BBBBB......BBBBBBBBBBBBBBBBBBBB.....BBBBB.....................................
    ..BBBBB......BBBBBBBBBBBBBBBBBBBB.....BBB.......................................
    ..BBBBB......BBBBBBBBBBBBBBBB....BBBBBBBBB......................................
    ..BBBBB.........................................................................
    ..BBBBB.......................................B.................................
    ..BBBBB.........................................................................
    ..BBBBB.........................................................................
    .BBBBBBBBBBBBBBBBBBBBBBB.....................BBBBB.............BB...............
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkykkkkkkkkkkkkkkkkkkkkkkkkkkkk
    ....bbbbbbbbbbbbbbbbbb..........................................................
    ..yyyyy...rrryyyyy.....mmmmmmmmmmm..bbbb........................................
    r.yyyyy......bbbbbbbbbbb...........................ykkkkk...y...................
    r.yyyyy.........................................................................
    ..yyyyy.........................................................................
    rwyyyyywww............................................................wwwwwwwwww
    ..yyyyy.....................yyyyyyyyyyyyyyyyyyyyyyyyy...........................
    ..yyyyy.....................yyyyy.....yyyyy.....................................
    ..yyyyy.....................yyyyy.....yyy.......................................
    r.yyyyy.....................y...................................................
    r.yyyyy.........................................................................
    r.yyyyy.......................................y.................................
    r.yyyyy.........................................................................
    ..yyyyy................mmmmmmmmmmm..............................................
    kkkkkkkkkkkkkkkkkkkyyyyykkkkkkkkkkkkkkkkkkkkkyyyyykkkkkkkkkkkkkyykkkkkkkkkkkkkkk
    cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
    ................................................................................
    ................................................................................
    ...................................................cccccc.......................
    ................................................................................
    ................................................................................
    gggggggggg............................................................gggggggggg
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
    "#);
}

/// The info window of a hard link in `--color dark-bg`.
#[test]
fn color_dark_background_info() {
    let tree = tree_of(SOURCE, true);
    let options = BrowserOptions {
        si: true,
        extended: true,
        color: ColorScheme::DarkBackground,
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
    ...................................................B............................
    ....BBBBBBBBBBBBBBBBBB..........................................................
    ..BBBBB......BBBBB..................BBBB........................................
    ..BBBBB......BBBBBBBBBBB...........................B........B...................
    ..BBBBB.........................................................................
    ..BBBBB......BBBBBB.............................................................
    .BBBBBBBBB...BBBBBB..........BBBBBBB......BBBBBB......................BBBBBBBBBB
    ..BBBBB......BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB...........................
    ..BBBBB......BBBBBBBBBBBBBBBBBBBB.....BBBBB.....................................
    ..BBBBB......BBBBBBBBBBBBBBBBBBBB.....BBB.......................................
    ..BBBBB......BBBBBBBBBBBBBBBB....BBBBBBBBB......................................
    ..BBBBB.........................................................................
    ..BBBBB.......................................B.................................
    ..BBBBB.........................................................................
    ..BBBBB.........................................................................
    .BBBBBBBBBBBBBBBBBBBBBBB.....................BBBBB.............BB...............
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkykkkkkkkkkkkkkkkkkkkkkkkkkkkk
    wwwwbbbbbbbbbbbbbbbbbbwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    wwyyyyywwwrrryyyyywwwwwmmmmmmmmmmmwwbbbbwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    rwyyyyywwwwwwbbbbbbbbbbbwwwwwwwwwwwwwwwwwwwwwwwwwwwykkkkkwwwywwwwwwwwwwwwwwwwwww
    rwyyyyywwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    wwyyyyywwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    rwyyyyywwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    wwyyyyywwwwwwwwwwwwwwwwwwwwwyyyyyyyyyyyyyyyyyyyyyyyyywwwwwwwwwwwwwwwwwwwwwwwwwww
    wwyyyyywwwwwwwwwwwwwwwwwwwwwyyyyywwwwwyyyyywwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    wwyyyyywwwwwwwwwwwwwwwwwwwwwyyyyywwwwwyyywwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    rwyyyyywwwwwwwwwwwwwwwwwwwwwywwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    rwyyyyywwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    rwyyyyywwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwywwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    rwyyyyywwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    wwyyyyywwwwwwwwwwwwwwwwmmmmmmmmmmmwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    kkkkkkkkkkkkkkkkkkkyyyyykkkkkkkkkkkkkkkkkkkkkyyyyykkkkkkkkkkkkkyykkkkkkkkkkkkkkk
    cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkcccccckkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    ggggggggggkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkgggggggggg
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
    "#);
}

/// The links tab in `--color dark` draws the selected link on green.
#[test]
fn color_dark_info_links() {
    let tree = tree_of(SOURCE, false);
    let options = BrowserOptions {
        color: ColorScheme::Dark,
        ..imported()
    };
    let keys = [
        Key::Down,
        Key::Down,
        Key::Down,
        Key::Down,
        Key::Char('i'),
        Key::Char('2'),
        Key::Down,
    ];
    assert_snapshot!(screen(&tree, root(&tree), &options, &keys, (80, 16)), @r#"
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
    ...................................................B............................
    ....BBBBBBBBBBBBBBBBBB..........................................................
    ..BBBBB......BBBBBBBBBBB...........................B........B...................
    ..BBBBB.........................................................................
    ..BBBBB.........................................................................
    ..BBBBB.....BBBBBBBBBBB.........................................................
    .BBBBBBBBB............................................................BBBBBBBBBB
    ..BBBBB.........................................................................
    ..BBBBB.........................................................................
    ..BBBBB.........................................................................
    ..BBBBB.........................................................................
    ..BBBBB.........................................................................
    ..BBBBB.........................................................................
    ..BBBBB.......................................B.................................
    ..BBBBB.........................................................................
    .BBBBBBBBBBBBBBBBBBBBBBB......................BBBBB..............BB.............
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkykkkkkkkkkkkkkkkkkkkkkkkkkkkk
    ....bbbbbbbbbbbbbbbbbb..........................................................
    ..yyyyy......bbbbbbbbbbb...........................y........ykkkkkk.............
    r.yyyyy.........................................................................
    r.yyyyy.........................................................................
    ..yyyyy.....wwwwwwwwwww.........................................................
    rwyyyyywww............................................................wwwwwwwwww
    ..yyyyy.........................................................................
    ..yyyyy.........................................................................
    ..yyyyy.........................................................................
    r.yyyyy.........................................................................
    r.yyyyy.........................................................................
    r.yyyyy.........................................................................
    r.yyyyy.......................................y.................................
    ..yyyyy.........................................................................
    kkkkkkkkkkkkkkkkkkkyyyyykkkkkkkkkkkkkkkkkkkkkkyyyyykkkkkkkkkkkkkkyykkkkkkkkkkkkk
    cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
    ................................................................................
    ............................................................ccccccc.............
    ................................................................................
    ................................................................................
    ............ggggggggggg.........................................................
    gggggggggg............................................................gggggggggg
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
    "#);
}

/// The keys tab of the help window in `--color dark` draws the keys in yellow.
#[test]
fn color_dark_help() {
    let tree = tree_of(SOURCE, false);
    let options = BrowserOptions {
        color: ColorScheme::Dark,
        ..imported()
    };
    assert_snapshot!(screen(&tree, root(&tree), &options, &[Key::Char('?')], (80, 18)), @r#"
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
    ...................................................B............................
    ....BBBBBBBBBBBBBBBBBB..........................................................
    BBBBBBBBBB...BBBBBBBBBBB................B........B..........B.........BBBBBBBBBB
    ..BBBBB.........................................................................
    ..BBBBB...........BBBBB.........................................................
    ..BBBBB.........BBBBBBB.........................................................
    ..BBBBB.....BBBBBBBBBBB.........................................................
    ..BBBBB......BBBBBBBBBB.........................................................
    ..BBBBB...............B.........................................................
    ..BBBBB...............B.........................................................
    ..BBBBB...............B.........................................................
    ..BBBBB...............B.........................................................
    ..BBBBB...............B.........................................................
    ..BBBBB...............B.........................................................
    ..BBBBB.........................................................................
    ..........................................................B.....................
    ................................................................................
    .BBBBBBBBBBBBBBBBBBBBBBB......................BBBBB..............BB.............
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkykkkkkkkkkkkkkkkkkkkkkkkkkkkk
    ....bbbbbbbbbbbbbbbbbb..........................................................
    wwyyyyywww...bbbbbbbbbbb................ykkkkk...y..........y.........wwwwwwwwww
    r.yyyyy.........................................................................
    r.yyyyy...........yyyyy.........................................................
    ..yyyyy.........yyyyyyy.........................................................
    r.yyyyy.....yyyyyyyyyyy.........................................................
    ..yyyyy......yyyyyyyyyy.........................................................
    ..yyyyy...............y.........................................................
    ..yyyyy...............y.........................................................
    r.yyyyy...............y.........................................................
    r.yyyyy...............y.........................................................
    r.yyyyy...............y.........................................................
    r.yyyyy...............y.........................................................
    ..yyyyy.........................................................................
    ..........................................................y.....................
    ................................................................................
    kkkkkkkkkkkkkkkkkkkyyyyykkkkkkkkkkkkkkkkkkkkkkyyyyykkkkkkkkkkkkkkyykkkkkkkkkkkkk
    cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
    ................................................................................
    gggggggggg..............................cccccc........................gggggggggg
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
    cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
    "#);
}

/// The keys tab of the help window in `--color dark-bg`.
#[test]
fn color_dark_background_help() {
    let tree = tree_of(SOURCE, false);
    let options = BrowserOptions {
        color: ColorScheme::DarkBackground,
        ..imported()
    };
    assert_snapshot!(screen(&tree, root(&tree), &options, &[Key::Char('?')], (80, 18)), @r#"
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
    ...................................................B............................
    ....BBBBBBBBBBBBBBBBBB..........................................................
    BBBBBBBBBB...BBBBBBBBBBB................B........B..........B.........BBBBBBBBBB
    ..BBBBB.........................................................................
    ..BBBBB...........BBBBB.........................................................
    ..BBBBB.........BBBBBBB.........................................................
    ..BBBBB.....BBBBBBBBBBB.........................................................
    ..BBBBB......BBBBBBBBBB.........................................................
    ..BBBBB...............B.........................................................
    ..BBBBB...............B.........................................................
    ..BBBBB...............B.........................................................
    ..BBBBB...............B.........................................................
    ..BBBBB...............B.........................................................
    ..BBBBB...............B.........................................................
    ..BBBBB.........................................................................
    ..........................................................B.....................
    ................................................................................
    .BBBBBBBBBBBBBBBBBBBBBBB......................BBBBB..............BB.............
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkykkkkkkkkkkkkkkkkkkkkkkkkkkkk
    wwwwbbbbbbbbbbbbbbbbbbwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    wwyyyyywwwwwwbbbbbbbbbbbwwwwwwwwwwwwwwwwykkkkkwwwywwwwwwwwwwywwwwwwwwwwwwwwwwwww
    rwyyyyywwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    rwyyyyywwwwwwwwwwwyyyyywwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    wwyyyyywwwwwwwwwyyyyyyywwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    rwyyyyywwwwwyyyyyyyyyyywwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    wwyyyyywwwwwwyyyyyyyyyywwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    wwyyyyywwwwwwwwwwwwwwwywwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    wwyyyyywwwwwwwwwwwwwwwywwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    rwyyyyywwwwwwwwwwwwwwwywwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    rwyyyyywwwwwwwwwwwwwwwywwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    rwyyyyywwwwwwwwwwwwwwwywwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    rwyyyyywwwwwwwwwwwwwwwywwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    wwyyyyywwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    wwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwywwwwwwwwwwwwwwwwwwwww
    wwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww
    kkkkkkkkkkkkkkkkkkkyyyyykkkkkkkkkkkkkkkkkkkkkkyyyyykkkkkkkkkkkkkkyykkkkkkkkkkkkk
    cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    ggggggggggkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkcccccckkkkkkkkkkkkkkkkkkkkkkkkgggggggggg
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
    cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
    "#);
}

/// The format tab of the help window in `--color dark` draws the flags in red.
#[test]
fn color_dark_help_format() {
    let tree = tree_of(SOURCE, false);
    let options = BrowserOptions {
        color: ColorScheme::Dark,
        ..imported()
    };
    let keys = [Key::Char('?'), Key::Char('2')];
    assert_snapshot!(screen(&tree, root(&tree), &options, &keys, (80, 18)), @r#"
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
    ...................................................B............................
    ....BBBBBBBBBBBBBBBBBB..........................................................
    BBBBBBBBBB...BBBBBBBBBBB................B........B..........B.........BBBBBBBBBB
    ..BBBBB.........................................................................
    ..BBBBB......BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB..............................
    ..BBBBB.........................................................................
    ..BBBBB.........................................................................
    ..BBBBB.........................................................................
    ..BBBBB.........................................................................
    ..BBBBB.........................................................................
    ..BBBBB.........................................................................
    ..BBBBB.........................................................................
    ..BBBBB.........................................................................
    ..BBBBB.........................................................................
    ..BBBBB.........................................................................
    ..........................................................B.....................
    ................................................................................
    .BBBBBBBBBBBBBBBBBBBBBBB......................BBBBB..............BB.............
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkykkkkkkkkkkkkkkkkkkkkkkkkkkkk
    ....bbbbbbbbbbbbbbbbbb..........................................................
    wwyyyyywww...bbbbbbbbbbb................y........ykkkkkkk...y.........wwwwwwwwww
    r.yyyyy.........................................................................
    r.yyyyy.........................................................................
    ..yyyyy.........................................................................
    r.yyyyy.........................................................................
    ..yyyyy.......r.................................................................
    ..yyyyy.......r.................................................................
    ..yyyyy.......r.................................................................
    r.yyyyy.......r.................................................................
    r.yyyyy.......r.................................................................
    r.yyyyy.......r.................................................................
    r.yyyyy.......r.................................................................
    ..yyyyy.......r.................................................................
    ..........................................................y.....................
    ................................................................................
    kkkkkkkkkkkkkkkkkkkyyyyykkkkkkkkkkkkkkkkkkkkkkyyyyykkkkkkkkkkkkkkyykkkkkkkkkkkkk
    cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
    ................................................................................
    gggggggggg.......................................cccccccc.............gggggggggg
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
    cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
    "#);
}

/// The about tab of the help window in `--color dark` draws the logo on green and the version in
/// yellow.
#[test]
fn color_dark_help_about() {
    let tree = tree_of(SOURCE, false);
    let options = BrowserOptions {
        color: ColorScheme::Dark,
        ..imported()
    };
    let keys = [Key::Char('?'), Key::Char('3')];
    assert_snapshot!(screen(&tree, root(&tree), &options, &keys, (80, 18)), @r#"
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
    ...................................................B............................
    ....BBBBBBBBBBBBBBBBBB..........................................................
    BBBBBBBBBB...BBBBBBBBBBB................B........B..........B.........BBBBBBBBBB
    ..BBBBB.........................................................................
    ..BBBBB.........................................................................
    ..BBBBB...............BBBBBB..BBBBB......BB..BB..B..............................
    ..BBBBB...............BB..BB..BB.........BB..BB..B..............................
    ..BBBBB...............BB..BB..BB.....BBBBBB..BB..B..............................
    ..BBBBB...............BB..BB..BB.....BB..BB..BB..B..............................
    ..BBBBB...............BB..BB..BBBBB..BBBBBB..BBBBB...BBBBB......................
    ..BBBBB.........................................................................
    ..BBBBB.........................................................................
    ..BBBBB.........................................................................
    ..BBBBB.........................................................................
    ..BBBBB.........................................................................
    ..........................................................B.....................
    ................................................................................
    .BBBBBBBBBBBBBBBBBBBBBBB......................BBBBB..............BB.............
    kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkykkkkkkkkkkkkkkkkkkkkkkkkkkkk
    ....bbbbbbbbbbbbbbbbbb..........................................................
    wwyyyyywww...bbbbbbbbbbb................y........y..........ykkkkkk...wwwwwwwwww
    r.yyyyy.........................................................................
    r.yyyyy.........................................................................
    ..yyyyy...............wwwwww..wwwww......ww..ww..w..............................
    r.yyyyy...............ww..ww..ww.........ww..ww..w..............................
    ..yyyyy...............ww..ww..ww.....wwwwww..ww..w..............................
    ..yyyyy...............ww..ww..ww.....ww..ww..ww..w..............................
    ..yyyyy...............ww..ww..wwwww..wwwwww..wwwww...yyyyy......................
    r.yyyyy.........................................................................
    r.yyyyy.........................................................................
    r.yyyyy.........................................................................
    r.yyyyy.........................................................................
    ..yyyyy.........................................................................
    ..........................................................y.....................
    ................................................................................
    kkkkkkkkkkkkkkkkkkkyyyyykkkkkkkkkkkkkkkkkkkkkkyyyyykkkkkkkkkkkkkkyykkkkkkkkkkkkk
    cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
    ................................................................................
    gggggggggg..................................................ccccccc...gggggggggg
    ................................................................................
    ................................................................................
    ......................gggggg..ggggg......gg..gg..g..............................
    ......................gg..gg..gg.........gg..gg..g..............................
    ......................gg..gg..gg.....gggggg..gg..g..............................
    ......................gg..gg..gg.....gg..gg..gg..g..............................
    ......................gg..gg..ggggg..gggggg..ggggg..............................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
    "#);
}

/// A binary export is read from the file, and the header marks it instead of the import. The
/// listing is that of the same tree imported from JSON, in [`default_columns`].
#[test]
fn from_file() {
    assert_snapshot!(file_screen("tree.ncdu", &imported(), &[], (80, 18)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help        [from file]"
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

/// A subdirectory is read from the file when it is opened, with the totals that the export
/// stores for it, and the row of the parent directory.
#[test]
fn from_file_subdirectory() {
    assert_snapshot!(file_screen("tree.ncdu", &imported(), &[Key::Enter], (80, 6)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help        [from file]"
    "--- /home/user/project/src -----------------------------------------------------"
    "                          /..                                                   "
    "    8.0 KiB [###########]  main.rs                                              "
    "H   4.0 KiB [########   ]  linked                                               "
    "*Total disk usage:  16.0 KiB   Apparent size:   9.2 KiB   Items: 2              "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// The parent directory is read from the file again, with the cursor where it was left.
#[test]
fn from_file_enter_parent() {
    let keys = [Key::Down, Key::Enter, Key::Left];
    assert_snapshot!(file_screen("tree.ncdu", &imported(), &keys, (80, 6)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help        [from file]"
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 KiB S   4.0 KiB [###########] /src                                      "
    "!   4.0 KiB             [####       ] /unreadable                               "
    "e   4.0 KiB             [####       ] /empty                                    "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// While a directory is read from the file, the screen has the header, the path of the directory,
/// no listing, and the number of entries read so far in the footer, also for the parent
/// directory.
#[test]
fn from_file_loading() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../duit/tests/fixtures/tree.ncdu");
    let file = File::open(path).expect("export is opened");
    let reader = BinaryReader::open(file).expect("index is read");
    let mut options = imported();
    let (mut browser, mut tree) = Browser::open(reader, &options, &mut |_| {})
        .expect("root is read")
        .expect("root is a directory");
    let mut terminal = Terminal::new(TestBackend::new(80, 6)).expect("terminal opens");
    browser
        .key_input(&mut tree, &mut options, Key::Enter, 6, &mut |loading| {
            terminal
                .draw(|frame| loading.draw(frame))
                .expect("loading draws");
        })
        .expect("key is handled");
    assert_snapshot!(render(&terminal), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help        [from file]"
    "--- /home/user/project/src -----------------------------------------------------"
    "                                                                                "
    "                                                                                "
    "                                                                                "
    " Loading... 2                                                                   "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
    // Going back reads the parent directory, under its path.
    browser
        .key_input(&mut tree, &mut options, Key::Left, 6, &mut |loading| {
            terminal
                .draw(|frame| loading.draw(frame))
                .expect("loading draws");
        })
        .expect("key is handled");
    let screen = terminal.backend().to_string();
    assert_eq!(
        screen.lines().nth(1),
        Some(
            r#""--- /home/user/project ---------------------------------------------------------""#
        )
    );
}

/// The footer counts the entries that the browser has listed, and it is drawn at each count
/// without the bits of 1 and 64, as `loadDir` calls `handleEvent`.
#[test]
fn loading_counts() {
    let tree = tree_of(item_counts(&[("many", 140)]).as_bytes(), false);
    let mut terminal = Terminal::new(TestBackend::new(20, 3)).expect("terminal opens");
    let mut footers = Vec::new();
    Browser::new(
        &tree,
        subdirectory(&tree, b"many"),
        &imported(),
        &mut |loading| {
            terminal
                .draw(|frame| loading.draw(frame))
                .expect("loading draws");
            footers.extend(
                terminal
                    .backend()
                    .to_string()
                    .lines()
                    .nth(2)
                    .map(str::to_owned),
            );
        },
    );
    assert_snapshot!(footers.join("\n"), @r#"
    " Loading... 2       "
    " Loading... 4       "
    " Loading... 6       "
    " Loading... 8       "
    " Loading... 10      "
    " Loading... 12      "
    " Loading... 14      "
    " Loading... 16      "
    " Loading... 18      "
    " Loading... 20      "
    " Loading... 22      "
    " Loading... 24      "
    " Loading... 26      "
    " Loading... 28      "
    " Loading... 30      "
    " Loading... 32      "
    " Loading... 34      "
    " Loading... 36      "
    " Loading... 38      "
    " Loading... 40      "
    " Loading... 42      "
    " Loading... 44      "
    " Loading... 46      "
    " Loading... 48      "
    " Loading... 50      "
    " Loading... 52      "
    " Loading... 54      "
    " Loading... 56      "
    " Loading... 58      "
    " Loading... 60      "
    " Loading... 62      "
    " Loading... 128     "
    " Loading... 130     "
    " Loading... 132     "
    " Loading... 134     "
    " Loading... 136     "
    " Loading... 138     "
    " Loading... 140     "
    "#);
}

/// The mtime of a directory is the one that the export stores, its own, which sorts `/src` below
/// `also-linked`, while a tree in memory has the latest mtime below the directory. The entries
/// without an mtime keep the order of the export.
#[test]
fn from_file_mtime() {
    let options = BrowserOptions {
        extended: true,
        ..imported()
    };
    let keys = [Key::Char('m'), Key::Char('M')];
    let screen = file_screen("tree-extended.ncdu", &options, &keys, (100, 18));
    assert_snapshot!(redact_times(&screen), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help                            [from file]"
    "--- /home/user/project -----------------------------------------------------------------------------"
    "    0.0   B             [              ]  2023-11-14 HH:MM:SS +ZZZZ  sparse                         "
    "@   0.0   B             [              ]  2023-11-14 HH:MM:SS +ZZZZ  socket                         "
    "!   4.0 KiB             [####          ]  2023-11-14 HH:MM:SS +ZZZZ /unreadable                     "
    "e   4.0 KiB             [####          ]  2023-11-14 HH:MM:SS +ZZZZ /empty                          "
    "H   4.0 KiB             [####          ]  2023-11-14 HH:MM:SS +ZZZZ  also-linked                    "
    "   16.0 KiB S   4.0 KiB [##############]  2023-11-14 HH:MM:SS +ZZZZ /src                            "
    "    4.0 KiB             [####          ]  2023-11-14 HH:MM:SS +ZZZZ  README.md                      "
    "    0.0   B             [              ]                   no mtime  raw \xFF\xFE                   "
    "    0.0   B             [              ]                   no mtime  quote " backsl...control \x01 é"
    "^   0.0   B             [              ]                   no mtime /proc                           "
    "<   0.0   B             [              ]                   no mtime  node_modules                   "
    ">   0.0   B             [              ]                   no mtime /mnt                            "
    "    0.0   B             [              ]                   no mtime  broken                         "
    "                                                                                                    "
    "                                                                                                    "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15                                 "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ....................................................................................................
    ....................................................................................................
    ....................................................................................................
    ....................................................................................................
    ....................................................................................................
    ....................................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ....................................................................................................
    ....................................................................................................
    ....................................................................................................
    ....................................................................................................
    ....................................................................................................
    ....................................................................................................
    ....................................................................................................
    ....................................................................................................
    ....................................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// A directory of an export of several blocks lists entries that are read from each of them.
#[test]
fn from_file_blocks() {
    let keys = [Key::Char('n'), Key::Enter];
    assert_snapshot!(file_screen("blocks.ncdu", &imported(), &keys, (80, 10)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help        [from file]"
    "--- /blocks/directory-0 --------------------------------------------------------"
    "                          /..                                                   "
    "    4.0 KiB [###########]  file-with-a-long-name-0                              "
    "    4.0 KiB [###########]  file-with-a-long-name-7                              "
    "    4.0 KiB [###########]  file-with-a-long-name-14                             "
    "    4.0 KiB [###########]  file-with-a-long-name-21                             "
    "    4.0 KiB [###########]  file-with-a-long-name-28                             "
    "    4.0 KiB [###########]  file-with-a-long-name-35                             "
    "*Total disk usage: 176.0 KiB   Apparent size:  10.2 KiB   Items: 43             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// The links tab of a hard link has the height of the info tab, and says that the links are not
/// available.
#[test]
fn from_file_links() {
    let keys = [
        Key::Down,
        Key::Down,
        Key::Down,
        Key::Down,
        Key::Char('i'),
        Key::Char('2'),
    ];
    assert_snapshot!(file_screen("tree.ncdu", &imported(), &keys, (80, 18)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help        [from file]"
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 KiB S   4.0 KiB [###########] /src                                      "
    "!   4.0 KiB             [####       ] /unreadable                               "
    "e   4.0 Ki┌── Item info ───────────────────────────1:Info───2:Links──┐          "
    "    4.0 Ki│                                                          │          "
    "H   4.0 Ki│ This feature is not available when reading from file.    │          "
    "    0.0   │                                                          │          "
    "    0.0   │                                                          │          "
    "    0.0   │                                                          │rol \x01 é"
    "@   0.0   │                                                          │          "
    "^   0.0   │                                                          │          "
    "<   0.0   │                             Press i to close this window │          "
    ">   0.0   └──────────────────────────────────────────────────────────┘          "
    "    0.0   B             [           ]  broken                                   "
    "                                                                                "
    "                                                                                "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    .............BBBBBBBBBBB...........................B........#RRRRRR.............
    ................................................................................
    RRRRRRRRRR............................................................RRRRRRRRRR
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    ..............................................B.................................
    ................................................................................
    ................................................................................
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// A selection key on the links tab moves the cursor of the listing and shows the info tab of the
/// entry under it, as on the info tab.
#[test]
fn from_file_links_down() {
    let keys = [
        Key::Down,
        Key::Down,
        Key::Down,
        Key::Down,
        Key::Char('i'),
        Key::Char('2'),
        Key::Down,
    ];
    assert_snapshot!(file_screen("tree.ncdu", &imported(), &keys, (80, 18)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help        [from file]"
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 KiB S   4.0 KiB [###########] /src                                      "
    "!   4.0 KiB             [####       ] /unreadable                               "
    "e   4.0 KiB             [####       ] /empty                                    "
    "    4.0 Ki┌── Item info ─────────────────────────────────────────────┐          "
    "H   4.0 Ki│                                                          │          "
    "    0.0   │  Name: sparse                                            │          "
    "    0.0   │  Type: File                                              │          "
    "    0.0   │     Disk usage:   0.0   B (0)                            │rol \x01 é"
    "@   0.0   │  Apparent size:   1.0 MiB (1,048,576)                    │          "
    "^   0.0   │                                                          │          "
    "<   0.0   │                             Press i to close this window │          "
    ">   0.0   └──────────────────────────────────────────────────────────┘          "
    "    0.0   B             [           ]  broken                                   "
    "                                                                                "
    "                                                                                "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
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
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}

/// Refreshing is not available, which a message says.
#[test]
fn from_file_refresh() {
    assert_snapshot!(file_screen("tree.ncdu", &imported(), &[Key::Char('r')], (80, 12)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help        [from file]"
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 KiB S   4.0 KiB [###########] /src                                      "
    "!   4.0 Ki┌── Message ───────────────────────────────────────────────┐          "
    "e   4.0 Ki│                                                          │          "
    "    4.0 Ki│ Refresh feature is not available when reading from file. │          "
    "H   4.0 Ki│                                                          │          "
    "    0.0   │                                Press any key to continue │          "
    "    0.0   └──────────────────────────────────────────────────────────┘          "
    "    0.0   B             [           ]  quote " backslash \...\x09 control \x01 é"
    "@   0.0   B             [           ]  socket                                   "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    .............BBBBBBBBB..........................................................
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

/// Deleting is not available either.
#[test]
fn from_file_delete() {
    assert_snapshot!(file_screen("tree.ncdu", &imported(), &[Key::Char('d')], (80, 12)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help        [from file]"
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 KiB S   4.0 KiB [###########] /src                                      "
    "!   4.0 Ki┌── Message ───────────────────────────────────────────────┐          "
    "e   4.0 Ki│                                                          │          "
    "    4.0 Ki│ File deletion is not available when reading from file.   │          "
    "H   4.0 Ki│                                                          │          "
    "    0.0   │                                Press any key to continue │          "
    "    0.0   └──────────────────────────────────────────────────────────┘          "
    "    0.0   B             [           ]  quote " backslash \...\x09 control \x01 é"
    "@   0.0   B             [           ]  socket                                   "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    .............BBBBBBBBB..........................................................
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

/// The key after a message only closes it.
#[test]
fn from_file_message_closes() {
    let keys = [Key::Char('d'), Key::Down];
    assert_snapshot!(file_screen("tree.ncdu", &imported(), &keys, (80, 6)), @r#"
    "duit 0.0.1 ~ Use the arrow keys to navigate, press ? for help        [from file]"
    "--- /home/user/project ---------------------------------------------------------"
    "   16.0 KiB S   4.0 KiB [###########] /src                                      "
    "!   4.0 KiB             [####       ] /unreadable                               "
    "e   4.0 KiB             [####       ] /empty                                    "
    "*Total disk usage:  32.0 KiB   Apparent size:   1.0 MiB   Items: 15             "
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR#RRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    ................................................................................
    ................................................................................
    R##################RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR
    "#);
}
