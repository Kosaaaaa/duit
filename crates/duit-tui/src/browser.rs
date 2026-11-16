//! The file browser of `browser.zig`: the sorted listing of the open directory, the screen that
//! shows it, and the keys that move through it.

use std::cmp::Ordering;
use std::collections::HashMap;

use duit_model::{DirectoryId, Entry, EntryId, EntryKind, Special, Tree};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::style::Style;

use crate::ui::{
    BOLD, BOLD_REVERSE, PLAIN, REVERSE, Window, draw_line, draw_size, draw_text, format_number,
    format_timestamp, shorten, to_utf8,
};
use crate::util::{fmt5dec, strnatcmp};

/// The style of the bars in the graph column, `--graph-style`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphStyle {
    Hash,
    HalfBlock,
    EighthBlock,
}

/// The column that the browser sorts by, `--sort`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortColumn {
    Name,
    Blocks,
    Size,
    Items,
    Mtime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Ascending,
    Descending,
}

/// What the shared column of a directory shows, `--shared-column`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedColumn {
    Off,
    Shared,
    Unique,
}

/// The options that change the listing and its columns, with ncdu's defaults.
#[derive(Debug, Clone)]
pub struct BrowserOptions {
    /// Sizes in powers of 1000, `--si`.
    pub si: bool,
    pub show_hidden: bool,
    /// Show and compare disk usage rather than apparent sizes.
    pub show_blocks: bool,
    pub shared_column: SharedColumn,
    pub show_item_count: bool,
    pub show_mtime: bool,
    pub show_graph: bool,
    pub show_percent: bool,
    pub graph_style: GraphStyle,
    pub sort_column: SortColumn,
    pub sort_order: SortOrder,
    pub sort_directories_first: bool,
    pub sort_natural: bool,
    /// The tree was imported from a file, which the header marks.
    pub imported: bool,
    /// Deleting is enabled. The header marks a tree that is not imported and cannot be deleted
    /// from as read-only.
    pub can_delete: bool,
    /// The separator between groups of three digits in full numbers.
    pub thousands_separator: String,
    /// The tree has extended information, `-e`, which the mtime keys need.
    pub extended: bool,
    /// Ask before quitting, `--confirm-quit`.
    pub confirm_quit: bool,
}

impl Default for BrowserOptions {
    fn default() -> Self {
        Self {
            si: false,
            show_hidden: true,
            show_blocks: true,
            shared_column: SharedColumn::Shared,
            show_item_count: false,
            show_mtime: false,
            show_graph: true,
            show_percent: false,
            graph_style: GraphStyle::Hash,
            sort_column: SortColumn::Blocks,
            sort_order: SortOrder::Descending,
            sort_directories_first: false,
            sort_natural: true,
            imported: false,
            can_delete: true,
            thousands_separator: ",".to_string(),
            extended: false,
            confirm_quit: false,
        }
    }
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
}

/// The window over the listing that takes the keys, `state` in `browser.zig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Main,
    /// The confirmation of `--confirm-quit`.
    Quit,
}

/// Where the user left a directory, `View` in `browser.zig`.
#[derive(Debug, Clone, Default)]
struct View {
    /// The index of the first row on the screen.
    top: usize,
    /// The name of the entry under the cursor, or `None` for the row of the parent directory and
    /// for an empty listing.
    cursor: Option<Vec<u8>>,
}

/// The open directory of the browser, with its entries in the order they are listed.
#[derive(Debug)]
pub struct Browser {
    directory: DirectoryId,
    /// The path of the directory: the names from the root down, joined with `/`. Going to the
    /// parent directory cuts its last component, as ncdu does, so it is kept rather than computed
    /// from the tree.
    path: Vec<u8>,
    /// The shown entries, sorted. `None` is the row of the parent directory, which is first when
    /// the directory is not the root.
    items: Vec<Option<EntryId>>,
    /// The index in `items` of the row under the cursor.
    cursor: usize,
    /// The index in `items` of the first row on the screen.
    top: usize,
    /// The largest disk usage in 512-byte blocks and apparent size of the entries, hidden ones
    /// included, and at least 1. The graph scales to them.
    max_blocks: u64,
    max_size: u64,
    /// Whether a shown directory has a shared size, which adds the shared column.
    has_shared: bool,
    /// The view of each directory that was open, by path, `opened_dir_views` in `browser.zig`.
    /// Directories with the same path share a view.
    views: HashMap<Vec<u8>, View>,
    state: State,
}

/// Whether ncdu shows `kind` as a directory, with a `/` in front of the name: a directory, also
/// when it is on another filesystem or a pseudo filesystem.
fn is_directory(kind: EntryKind) -> bool {
    match kind {
        EntryKind::Directory(_)
        | EntryKind::Special(Special::OtherFilesystem | Special::KernelFilesystem) => true,
        EntryKind::HardLink(_)
        | EntryKind::Regular
        | EntryKind::NonRegular
        | EntryKind::Special(Special::ReadError | Special::Pattern) => false,
    }
}

/// The directory data of `entry`, if it is a directory that was read.
fn directory_of(entry: &Entry) -> Option<DirectoryId> {
    match entry.kind {
        EntryKind::Directory(directory) => Some(directory),
        EntryKind::HardLink(_)
        | EntryKind::Regular
        | EntryKind::NonRegular
        | EntryKind::Special(_) => None,
    }
}

/// Whether `--hide-hidden` hides the entry `name` of `kind`: an excluded entry, or a name that
/// starts with `.` or ends with `~`.
fn is_hidden(kind: EntryKind, name: &[u8]) -> bool {
    let excluded = match kind {
        EntryKind::Special(
            Special::Pattern | Special::OtherFilesystem | Special::KernelFilesystem,
        ) => true,
        EntryKind::Directory(_)
        | EntryKind::HardLink(_)
        | EntryKind::Regular
        | EntryKind::NonRegular
        | EntryKind::Special(Special::ReadError) => false,
    };
    excluded || name.first() == Some(&b'.') || name.last() == Some(&b'~')
}

/// The disk usage of `blocks` 512-byte blocks in bytes.
fn blocks_to_size(blocks: u64) -> u64 {
    blocks.saturating_mul(512)
}

/// Compare `a` and `b` in `order`, or `None` when they are equal.
fn compare_numbers(a: u64, b: u64, order: SortOrder) -> Option<bool> {
    match (a == b, order) {
        (true, _) => None,
        (false, SortOrder::Ascending) => Some(a < b),
        (false, SortOrder::Descending) => Some(a > b),
    }
}

/// Whether `a` is listed before `b`.
fn sort_less(tree: &Tree, options: &BrowserOptions, a: EntryId, b: EntryId) -> bool {
    let (a_entry, b_entry) = (tree.entry(a), tree.entry(b));
    if options.sort_directories_first && is_directory(a_entry.kind) != is_directory(b_entry.kind) {
        return is_directory(a_entry.kind);
    }

    let order = options.sort_order;
    let items = |entry: &Entry| directory_of(entry).map_or(0, |id| tree.directory(id).items);
    let numbers: &[(u64, u64)] = match options.sort_column {
        // Names are compared last in any case.
        SortColumn::Name => &[],
        SortColumn::Blocks => &[
            (a_entry.blocks, b_entry.blocks),
            (a_entry.size, b_entry.size),
        ],
        SortColumn::Size => &[
            (a_entry.size, b_entry.size),
            (a_entry.blocks, b_entry.blocks),
        ],
        SortColumn::Items => &[
            (items(a_entry).into(), items(b_entry).into()),
            (a_entry.blocks, b_entry.blocks),
            (a_entry.size, b_entry.size),
        ],
        SortColumn::Mtime => match (tree.extended(a), tree.extended(b)) {
            (Some(a_extended), Some(b_extended)) => {
                &[(a_extended.latest_mtime, b_extended.latest_mtime)]
            }
            // Entries with extended information come first in either order, and entries without
            // keep the order of the tree.
            (a_extended, _) => return a_extended.is_some(),
        },
    };
    for &(a_number, b_number) in numbers {
        if let Some(less) = compare_numbers(a_number, b_number, order) {
            return less;
        }
    }

    let (first, second) = match order {
        SortOrder::Ascending => (tree.name(a), tree.name(b)),
        SortOrder::Descending => (tree.name(b), tree.name(a)),
    };
    if options.sort_natural {
        strnatcmp(first, second) == Ordering::Less
    } else {
        first < second
    }
}

/// Append `name` to `path` with a `/` in between, as Zig's `std.fs.path.join` does: an empty name
/// adds nothing, and two separators where they meet become one.
fn join_path(path: &mut Vec<u8>, name: &[u8]) {
    if name.is_empty() {
        return;
    }
    if path.is_empty() {
        path.extend_from_slice(name);
        return;
    }
    match (path.last() == Some(&b'/'), name.first() == Some(&b'/')) {
        (true, true) => path.extend_from_slice(&name[1..]),
        (false, false) => {
            path.push(b'/');
            path.extend_from_slice(name);
        }
        (true, false) | (false, true) => path.extend_from_slice(name),
    }
}

/// The path of `directory`: the names of the directories from the root down to it, joined.
fn path_of(tree: &Tree, directory: DirectoryId) -> Vec<u8> {
    let mut names = Vec::new();
    let mut current = Some(directory);
    while let Some(id) = current {
        let directory = tree.directory(id);
        names.push(tree.name(directory.entry));
        current = directory.parent;
    }
    let mut path = Vec::new();
    for name in names.into_iter().rev() {
        join_path(&mut path, name);
    }
    path
}

/// The path `path` without its last component, as Zig's `std.fs.path.dirname`: trailing
/// separators are ignored, and a path with one component has none.
fn dirname(path: &[u8]) -> Option<&[u8]> {
    let end = path.iter().rposition(|&byte| byte != b'/')?;
    let separator = path[..end].iter().rposition(|&byte| byte == b'/')?;
    match separator {
        0 => Some(&path[..1]),
        _ => Some(&path[..separator]),
    }
}

/// Move `index` in a list of `length` rows for a selection key, with `page` rows to a page, as
/// `keyInputSelection` in `browser.zig`. Other keys do nothing.
fn select_with_key(key: Key, index: &mut usize, length: usize, page: usize) {
    match key {
        Key::Char('j') | Key::Down => {
            if *index + 1 < length {
                *index += 1;
            }
        }
        Key::Char('k') | Key::Up => *index = index.saturating_sub(1),
        Key::Home => *index = 0,
        Key::End => *index = length.saturating_sub(1),
        Key::PageUp => *index = index.saturating_sub(page),
        Key::PageDown => *index = length.saturating_sub(1).min(*index + page),
        Key::Char(_) | Key::Enter | Key::Backspace | Key::Left | Key::Right => {}
    }
}

/// Format the item count `count` of a directory in 6 columns.
fn format_item_count(count: u32, separator: &str) -> String {
    match count {
        0..1000 => format!("  {count:>4}"),
        1000..10_000 => format!(" {}", format_number(count.into(), separator)),
        10_000..100_000 => format_number(count.into(), separator),
        100_000..999_950 => format!("{}k", fmt5dec(u64::from(count + 50) / 100)),
        999_950..999_950_000 => format!("{}M", fmt5dec(u64::from(count + 50_000) / 100_000)),
        999_950_000.. => "  > 1G".to_string(),
    }
}

impl Browser {
    /// Open `directory` of `tree`, with the cursor on the first row.
    pub fn new(tree: &Tree, directory: DirectoryId, options: &BrowserOptions) -> Self {
        let mut browser = Self {
            directory,
            path: path_of(tree, directory),
            items: Vec::new(),
            cursor: 0,
            top: 0,
            max_blocks: 1,
            max_size: 1,
            has_shared: false,
            views: HashMap::new(),
            state: State::Main,
        };
        browser.load(tree, options);
        browser
    }

    /// List the entries of the directory that `options` shows, then sort them, `loadDir` in
    /// `browser.zig`.
    fn load(&mut self, tree: &Tree, options: &BrowserOptions) {
        self.items.clear();
        self.max_blocks = 1;
        self.max_size = 1;
        self.has_shared = false;
        if tree.directory(self.directory).parent.is_some() {
            self.items.push(None);
        }
        for id in tree.entries(self.directory) {
            let entry = tree.entry(id);
            self.max_blocks = self.max_blocks.max(entry.blocks);
            self.max_size = self.max_size.max(entry.size);
            if !options.show_hidden && is_hidden(entry.kind, tree.name(id)) {
                continue;
            }
            self.items.push(Some(id));
            if let Some(directory) = directory_of(entry) {
                let directory = tree.directory(directory);
                if directory.shared_blocks > 0 || directory.shared_size > 0 {
                    self.has_shared = true;
                }
            }
        }
        self.sort(tree, options);
    }

    /// Sort the listing as `options` says, and restore the view of the directory, `sortDir` in
    /// `browser.zig`.
    fn sort(&mut self, tree: &Tree, options: &BrowserOptions) {
        let start = usize::from(self.items.first() == Some(&None));
        // A stable sort, as Zig's `std.mem.sort`, keeps entries that compare equal in the order
        // of the tree.
        self.items[start..].sort_by(|a, b| match (a, b) {
            (Some(a), Some(b)) => {
                if sort_less(tree, options, *a, *b) {
                    Ordering::Less
                } else if sort_less(tree, options, *b, *a) {
                    Ordering::Greater
                } else {
                    Ordering::Equal
                }
            }
            (None, _) | (_, None) => Ordering::Equal,
        });
        self.load_view(tree);
    }

    /// Remember the view of the directory, `View.save` in `browser.zig`.
    fn save_view(&mut self, tree: &Tree) {
        let cursor = self
            .items
            .get(self.cursor)
            .copied()
            .flatten()
            .map(|id| tree.name(id).to_vec());
        self.views.insert(
            self.path.clone(),
            View {
                top: self.top,
                cursor,
            },
        );
    }

    /// Restore the view that was saved for the path of the directory, or start at the top, and
    /// put the cursor on its entry if that is listed or on the first row, `View.load` in
    /// `browser.zig`.
    fn load_view(&mut self, tree: &Tree) {
        let view = self.views.get(&self.path).cloned().unwrap_or_default();
        self.top = view.top;
        self.cursor = self
            .items
            .iter()
            .position(|item| item.map(|id| tree.name(id)) == view.cursor.as_deref())
            .unwrap_or(0);
    }

    /// Open the subdirectory `directory`, `enterSub` in `browser.zig`.
    fn enter_subdirectory(
        &mut self,
        tree: &Tree,
        options: &BrowserOptions,
        directory: DirectoryId,
    ) {
        self.directory = directory;
        join_path(&mut self.path, tree.name(tree.directory(directory).entry));
        self.load(tree, options);
    }

    /// Open the parent directory, if there is one, `enterParent` in `browser.zig`.
    fn enter_parent(&mut self, tree: &Tree, options: &BrowserOptions) {
        let Some(parent) = tree.directory(self.directory).parent else {
            return;
        };
        self.directory = parent;
        // An empty name leaves a path without a separator to cut, for example a root named `foo`
        // with a subdirectory without a name, which a scan never gives. ncdu reaches `unreachable`
        // there.
        self.path = dirname(&self.path).map_or_else(|| path_of(tree, parent), <[u8]>::to_vec);
        self.load(tree, options);
    }

    /// Sort by `column`: in `default_order` when it is another column than before, and in the
    /// other order when it is the same, `sortToggle` in `browser.zig`.
    fn sort_toggle(
        &mut self,
        tree: &Tree,
        options: &mut BrowserOptions,
        column: SortColumn,
        default_order: SortOrder,
    ) {
        options.sort_order = if options.sort_column != column {
            default_order
        } else {
            match options.sort_order {
                SortOrder::Ascending => SortOrder::Descending,
                SortOrder::Descending => SortOrder::Ascending,
            }
        };
        options.sort_column = column;
        self.sort(tree, options);
    }

    /// Handle `key` on a terminal with `height` rows, which sets the size of a page, and change
    /// `options` as the display and sort keys say, `keyInput` in `browser.zig`. Returns whether
    /// to quit.
    pub fn key_input(
        &mut self,
        tree: &Tree,
        options: &mut BrowserOptions,
        key: Key,
        height: u16,
    ) -> bool {
        let quit = self.handle_key(tree, options, key, height);
        self.save_view(tree);
        quit
    }

    fn handle_key(
        &mut self,
        tree: &Tree,
        options: &mut BrowserOptions,
        key: Key,
        height: u16,
    ) -> bool {
        match self.state {
            State::Main => {}
            State::Quit => {
                self.state = State::Main;
                return key == Key::Char('y') || key == Key::Char('Y');
            }
        }

        match key {
            Key::Char('q') => {
                if !options.confirm_quit {
                    return true;
                }
                self.state = State::Quit;
            }
            // The help and info windows, refresh, shell, and delete.
            Key::Char('?' | 'i' | 'r' | 'b' | 'd') => {}

            Key::Char('n') => {
                self.sort_toggle(tree, options, SortColumn::Name, SortOrder::Ascending);
            }
            Key::Char('s') => {
                let column = if options.show_blocks {
                    SortColumn::Blocks
                } else {
                    SortColumn::Size
                };
                self.sort_toggle(tree, options, column, SortOrder::Descending);
            }
            Key::Char('C') => {
                self.sort_toggle(tree, options, SortColumn::Items, SortOrder::Descending);
            }
            Key::Char('M') => {
                if options.extended {
                    self.sort_toggle(tree, options, SortColumn::Mtime, SortOrder::Descending);
                }
            }
            Key::Char('e') => {
                options.show_hidden = !options.show_hidden;
                self.load(tree, options);
            }
            Key::Char('t') => {
                options.sort_directories_first = !options.sort_directories_first;
                self.sort(tree, options);
            }
            Key::Char('a') => {
                options.show_blocks = !options.show_blocks;
                if options.show_blocks && options.sort_column == SortColumn::Size {
                    options.sort_column = SortColumn::Blocks;
                    self.sort(tree, options);
                }
                if !options.show_blocks && options.sort_column == SortColumn::Blocks {
                    options.sort_column = SortColumn::Size;
                    self.sort(tree, options);
                }
            }

            Key::Enter | Key::Char('l') | Key::Right => match self.items.get(self.cursor) {
                Some(Some(id)) => {
                    if let Some(directory) = directory_of(tree.entry(*id)) {
                        self.enter_subdirectory(tree, options, directory);
                    }
                }
                Some(None) => self.enter_parent(tree, options),
                None => {}
            },
            Key::Char('h' | '<') | Key::Backspace | Key::Left => self.enter_parent(tree, options),

            Key::Char('c') => options.show_item_count = !options.show_item_count,
            Key::Char('m') => {
                if options.extended {
                    options.show_mtime = !options.show_mtime;
                }
            }
            Key::Char('g') => {
                (options.show_graph, options.show_percent) =
                    match (options.show_graph, options.show_percent) {
                        (false, false) => (true, false),
                        (true, false) => (false, true),
                        (false, true) => (true, true),
                        (true, true) => (false, false),
                    };
            }
            Key::Char('u') => {
                options.shared_column = match options.shared_column {
                    SharedColumn::Off => SharedColumn::Shared,
                    SharedColumn::Shared => SharedColumn::Unique,
                    SharedColumn::Unique => SharedColumn::Off,
                };
            }

            Key::Char(_)
            | Key::Up
            | Key::Down
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown => select_with_key(
                key,
                &mut self.cursor,
                self.items.len(),
                usize::from(height.saturating_sub(3)),
            ),
        }
        false
    }

    /// Draw the browser on all of `frame`, which starts at the top left corner of the terminal:
    /// the header, the path, the rows of the listing that fit, and the totals of the directory in
    /// the footer.
    pub fn draw(&mut self, frame: &mut Frame<'_>, tree: &Tree, options: &BrowserOptions) {
        let (width, height) = (frame.area().width, frame.area().height);
        let buffer = frame.buffer_mut();
        let directory = tree.directory(self.directory);
        let directory_entry = tree.entry(directory.entry);

        draw_line(buffer, 0, 0, ' ', width, REVERSE);
        let x = draw_text(
            buffer,
            0,
            0,
            concat!(
                "duit ",
                env!("CARGO_PKG_VERSION"),
                " ~ Use the arrow keys to navigate, press "
            ),
            REVERSE,
        );
        let x = draw_text(buffer, x, 0, "?", BOLD_REVERSE);
        draw_text(buffer, x, 0, " for help", REVERSE);
        let mode = if options.imported {
            Some("[imported]")
        } else if !options.can_delete {
            Some("[readonly]")
        } else {
            None
        };
        if let Some(mode) = mode {
            draw_text(buffer, width.saturating_sub(10), 0, mode, REVERSE);
        }

        draw_line(buffer, 0, 1, '-', width, PLAIN);
        let path = to_utf8(&self.path);
        let x = draw_text(buffer, 3, 1, " ", PLAIN);
        let x = draw_text(
            buffer,
            x,
            1,
            &shorten(&path, width.saturating_sub(5).into()),
            PLAIN,
        );
        draw_text(buffer, x, 1, " ", PLAIN);

        let rows = usize::from(height.saturating_sub(3));
        if self.cursor < self.top {
            self.top = self.cursor;
        }
        if self.cursor >= self.top + rows {
            self.top = self.cursor + 1 - rows;
        }
        for (y, index) in (2..).zip(self.top..self.items.len()).take(rows) {
            let row = Row {
                browser: self,
                tree,
                options,
                y,
                x: 0,
                width,
                item: self.items[index],
                style: if index == self.cursor { REVERSE } else { PLAIN },
            };
            row.draw(buffer);
        }

        let footer = height.saturating_sub(1);
        draw_line(buffer, 0, footer, ' ', width, REVERSE);
        let (disk_usage_style, apparent_size_style) = if options.show_blocks {
            (BOLD_REVERSE, REVERSE)
        } else {
            (REVERSE, BOLD_REVERSE)
        };
        let x = draw_text(
            buffer,
            0,
            footer,
            if options.show_blocks { "*" } else { " " },
            REVERSE,
        );
        let x = draw_text(buffer, x, footer, "Total disk usage: ", disk_usage_style);
        let x = draw_size(
            buffer,
            x,
            footer,
            blocks_to_size(directory_entry.blocks),
            options.si,
            REVERSE,
        );
        let x = draw_text(
            buffer,
            x,
            footer,
            if options.show_blocks { "   " } else { "  *" },
            apparent_size_style,
        );
        let x = draw_text(buffer, x, footer, "Apparent size: ", apparent_size_style);
        let x = draw_size(buffer, x, footer, directory_entry.size, options.si, REVERSE);
        let x = draw_text(buffer, x, footer, "   Items: ", REVERSE);
        draw_text(
            buffer,
            x,
            footer,
            &format_number(directory.items.into(), &options.thousands_separator),
            REVERSE,
        );

        match self.state {
            State::Main => {}
            State::Quit => {
                let window = Window::new(buffer, 4, 22, "Confirm quit");
                let x = window.text(buffer, 2, 2, "Really quit? (", PLAIN);
                let x = window.text(buffer, 2, x, "y", BOLD);
                let x = window.text(buffer, 2, x, "/", PLAIN);
                let x = window.text(buffer, 2, x, "N", BOLD);
                window.text(buffer, 2, x, ")", PLAIN);
            }
        }
    }
}

/// One row of the listing, which `Row` in `browser.zig` draws column by column.
struct Row<'a> {
    browser: &'a Browser,
    tree: &'a Tree,
    options: &'a BrowserOptions,
    /// The row on the screen, and the column where the next column of the listing starts.
    y: u16,
    x: u16,
    /// The width of the screen.
    width: u16,
    /// The entry, or `None` for the row of the parent directory.
    item: Option<EntryId>,
    /// [`REVERSE`] on the cursor row, and [`PLAIN`] on the others.
    style: Style,
}

impl Row<'_> {
    /// The columns that are left from `x` to the right edge of the screen.
    fn remaining(&self) -> u16 {
        self.width.saturating_sub(self.x)
    }

    fn draw(mut self, buffer: &mut Buffer) {
        if self.style == REVERSE {
            draw_line(buffer, self.x, self.y, ' ', self.width, self.style);
        }
        self.flag(buffer);
        self.size(buffer);
        self.graph(buffer);
        self.items(buffer);
        self.mtime(buffer);
        self.name(buffer);
    }

    fn flag(&mut self, buffer: &mut Buffer) {
        let x = self.x;
        self.x += 2;
        let Some(item) = self.item else {
            return;
        };
        let flag = match self.tree.entry(item).kind {
            EntryKind::Directory(id) => {
                let directory = self.tree.directory(id);
                if directory.read_error {
                    "!"
                } else if directory.sub_error {
                    "."
                } else if self.tree.entries(id).next().is_none() {
                    "e"
                } else {
                    return;
                }
            }
            EntryKind::HardLink(_) => "H",
            EntryKind::Special(Special::Pattern) => "<",
            EntryKind::Special(Special::OtherFilesystem) => ">",
            EntryKind::Special(Special::KernelFilesystem) => "^",
            EntryKind::NonRegular => "@",
            EntryKind::Regular | EntryKind::Special(Special::ReadError) => return,
        };
        draw_text(buffer, x, self.y, flag, self.style);
    }

    fn size(&mut self, buffer: &mut Buffer) {
        let options = self.options;
        let shared_column = self.browser.has_shared && options.shared_column != SharedColumn::Off;
        let mut width = if options.si { 9 } else { 10 };
        if shared_column {
            width = 2 * width + 2;
        }
        let x = self.x;
        self.x += width;
        let Some(item) = self.item else {
            return;
        };
        let entry = self.tree.entry(item);
        let size = if options.show_blocks {
            blocks_to_size(entry.blocks)
        } else {
            entry.size
        };
        let mut shared = directory_of(entry).map_or(0, |directory| {
            let directory = self.tree.directory(directory);
            if options.show_blocks {
                blocks_to_size(directory.shared_blocks)
            } else {
                directory.shared_size
            }
        });
        if options.shared_column == SharedColumn::Unique {
            shared = size.saturating_sub(shared);
        }

        let x = draw_size(buffer, x, self.y, size, options.si, self.style);
        if shared_column && shared > 0 {
            let label = match options.shared_column {
                SharedColumn::Unique => " U ",
                SharedColumn::Shared | SharedColumn::Off => " S ",
            };
            let x = draw_text(buffer, x, self.y, label, self.style);
            draw_size(buffer, x, self.y, shared, options.si, self.style);
        }
    }

    fn graph(&mut self, buffer: &mut Buffer) {
        let options = self.options;
        if (!options.show_graph && !options.show_percent) || self.remaining() < 20 {
            return;
        }
        let bar_size = (self.width / 7).max(10);
        let x = self.x;
        self.x += 3
            + if options.show_graph { bar_size } else { 0 }
            + if options.show_percent { 6 } else { 0 }
            + u16::from(options.show_graph && options.show_percent);
        let Some(item) = self.item else {
            return;
        };
        let entry = self.tree.entry(item);
        let directory = self
            .tree
            .entry(self.tree.directory(self.browser.directory).entry);

        let mut text = String::from("[");
        if options.show_percent {
            let (mut number, mut total) = if options.show_blocks {
                (entry.blocks, directory.blocks)
            } else {
                (entry.size, directory.size)
            };
            // Keep `number * 1000` in range.
            if number > 1 << 54 {
                number >>= 10;
                total >>= 10;
            }
            let permille = ((number * 1000).saturating_add(total / 2) / total.max(1)).min(1000);
            text.push_str(&fmt5dec(permille));
            text.push('%');
        }
        if options.show_graph && options.show_percent {
            text.push(' ');
        }
        if options.show_graph {
            let bar_size = u64::from(bar_size);
            let (mut max, mut number) = if options.show_blocks {
                (self.browser.max_blocks, entry.blocks)
            } else {
                (self.browser.max_size, entry.size)
            };
            if max < bar_size {
                max *= bar_size;
                number *= bar_size;
            }
            let per_block = max / bar_size;
            for _ in 0..bar_size {
                let eighths = (number.saturating_mul(8) / per_block).min(8);
                text.push(
                    match options.graph_style {
                        GraphStyle::Hash => [' ', ' ', ' ', ' ', ' ', ' ', ' ', ' ', '#'],
                        GraphStyle::HalfBlock => [' ', ' ', ' ', ' ', '▌', '▌', '▌', '▌', '█'],
                        GraphStyle::EighthBlock => [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'],
                    }[usize::try_from(eighths).unwrap_or(8)],
                );
                number = number.saturating_sub(per_block);
            }
        }
        text.push(']');
        draw_text(buffer, x, self.y, &text, self.style);
    }

    fn items(&mut self, buffer: &mut Buffer) {
        if !self.options.show_item_count || self.remaining() < 10 {
            return;
        }
        let x = self.x;
        self.x += 7;
        let Some(directory) = self
            .item
            .and_then(|item| directory_of(self.tree.entry(item)))
        else {
            return;
        };
        let count = format_item_count(
            self.tree.directory(directory).items,
            &self.options.thousands_separator,
        );
        draw_text(buffer, x, self.y, &count, self.style);
    }

    fn mtime(&mut self, buffer: &mut Buffer) {
        if !self.options.show_mtime || self.remaining() < 37 {
            return;
        }
        let x = self.x + 1;
        self.x += 27;
        // The row of the parent directory shows the mtime of the open directory.
        let entry = self
            .item
            .unwrap_or(self.tree.directory(self.browser.directory).entry);
        let text = match self.tree.extended(entry) {
            Some(extended) if extended.extended.mtime.is_some() => {
                format_timestamp(extended.latest_mtime)
                    .unwrap_or_else(|| "            invalid mtime".to_string())
            }
            Some(_) | None => "                 no mtime".to_string(),
        };
        draw_text(buffer, x, self.y, &text, self.style);
    }

    fn name(&self, buffer: &mut Buffer) {
        let Some(item) = self.item else {
            draw_text(buffer, self.x, self.y, "/..", self.style);
            return;
        };
        let prefix = if is_directory(self.tree.entry(item).kind) {
            "/"
        } else {
            " "
        };
        let x = draw_text(buffer, self.x, self.y, prefix, self.style);
        let name = to_utf8(self.tree.name(item));
        let width = self.remaining().saturating_sub(1);
        draw_text(buffer, x, self.y, &shorten(&name, width.into()), self.style);
    }
}

#[cfg(test)]
mod tests {
    use super::{dirname, format_item_count, join_path};

    /// Each range of item counts in its 6 columns. Upstream has no test for `Row.items`.
    #[test]
    fn item_counts() {
        let counts = [
            0,
            999,
            1000,
            9999,
            10_000,
            99_999,
            100_000,
            999_949,
            999_950,
            999_949_999,
            999_950_000,
            u32::MAX,
        ];
        let formatted: Vec<String> = counts
            .into_iter()
            .map(|count| format_item_count(count, ","))
            .collect();
        assert_eq!(
            formatted,
            [
                "     0", "   999", " 1,000", " 9,999", "10,000", "99,999", "100.0k", "999.9k",
                "  1.0M", "999.9M", "  > 1G", "  > 1G",
            ]
        );
    }

    /// Paths join as Zig's `std.fs.path.join` joins them. Upstream has no test for the path of
    /// the browser.
    #[test]
    fn join_paths() {
        let joined = |path: &[u8], name: &[u8]| {
            let mut path = path.to_vec();
            join_path(&mut path, name);
            String::from_utf8(path).expect("path is UTF-8")
        };
        assert_eq!(joined(b"/", b"etc"), "/etc");
        assert_eq!(joined(b"/home", b"user"), "/home/user");
        assert_eq!(joined(b"/home/", b"/user"), "/home/user");
        assert_eq!(joined(b"/home", b""), "/home");
        assert_eq!(joined(b"", b"user"), "user");
    }

    /// The `dirnamePosix` test of Zig's `std.fs.path`, which ncdu uses to go to the parent
    /// directory, and a separator that repeats in the middle.
    #[test]
    fn dirnames() {
        let parent = |path: &str| dirname(path.as_bytes()).map(<[u8]>::to_vec);
        assert_eq!(parent("/a/b/c"), Some(b"/a/b".to_vec()));
        assert_eq!(parent("/a/b/c///"), Some(b"/a/b".to_vec()));
        assert_eq!(parent("/a"), Some(b"/".to_vec()));
        assert_eq!(parent("/"), None);
        assert_eq!(parent("//"), None);
        assert_eq!(parent("///"), None);
        assert_eq!(parent("////"), None);
        assert_eq!(parent(""), None);
        assert_eq!(parent("a"), None);
        assert_eq!(parent("a/"), None);
        assert_eq!(parent("a//"), None);
        assert_eq!(parent("a//b"), Some(b"a/".to_vec()));
    }
}
