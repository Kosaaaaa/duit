//! The file browser of `browser.zig`: the sorted listing of the open directory, the screen that
//! shows it, and the keys that move through it.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs::File;
use std::iter;

use duit_format::{BinaryImportError, BinaryReader, ListedDirectory, Listing};
use duit_model::{DirectoryId, Entry, EntryId, EntryKind, LinkId, Special, Tree};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::style::Style as StyleAttr;

use crate::terminal::Key;
use crate::ui::{
    Bg, ColorScheme, Style, Window, draw_line, draw_size, draw_text, format_mode, format_number,
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
    /// The shell of `b` is enabled.
    pub can_shell: bool,
    /// Refreshing with `r` is enabled.
    pub can_refresh: bool,
    /// The separator between groups of three digits in full numbers.
    pub thousands_separator: String,
    /// The tree has extended information, `-e`, which the mtime keys need.
    pub extended: bool,
    /// Ask before quitting, `--confirm-quit`.
    pub confirm_quit: bool,
    /// The color scheme, `--color`.
    pub color: ColorScheme,
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
            can_shell: true,
            can_refresh: true,
            thousands_separator: ",".to_string(),
            extended: false,
            confirm_quit: false,
            color: ColorScheme::Off,
        }
    }
}

/// What the main loop does after a key, the `state` in `main.zig` that the browser sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Keep browsing.
    Browse,
    Quit,
    /// Run a shell in the open directory, [`Browser::path`].
    Shell,
}

/// The window over the listing that takes the keys, `state` in `browser.zig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Main,
    /// The confirmation of `--confirm-quit`.
    Quit,
    /// The help window, which shows [`Help`].
    Help,
    /// The info window, which shows [`Info`].
    Info,
}

/// A tab of the help window.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum HelpTab {
    #[default]
    Keys,
    Format,
    About,
}

/// The help window, `help` in `browser.zig`. It opens again on the tab it was closed on.
#[derive(Debug, Default)]
struct Help {
    tab: HelpTab,
    /// The index of the first key on the keys tab.
    offset: usize,
}

/// A tab of the info window. Only a hard link has the links tab.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Tab {
    #[default]
    Info,
    Links,
}

/// The info window, `info` in `browser.zig`. It stays as it is when a key of the listing closes the
/// window, so opening it again on the same entry keeps the cursor of the links tab.
#[derive(Debug, Default)]
struct Info {
    tab: Tab,
    /// The entry that the window was opened on, or `None` once a key of the window closed it.
    entry: Option<EntryId>,
    /// The links to the inode of the entry, sorted by path, once the links tab was opened.
    links: Option<Vec<LinkId>>,
    /// The index in `links` of the first row on the screen, and of the row under the cursor.
    links_top: usize,
    links_index: usize,
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

/// Where the browser reads its directories from, `main.config.binreader` in `browser.zig`.
enum Source {
    /// The tree in memory of a scan or a JSON import, with the directories that were opened to
    /// reach the open one, from the root, with the open one last, `dir_parents` in `browser.zig`.
    /// Going to the parent directory goes back to the one before the last. Enter in the links tab
    /// of the info window opens another directory without changing them or the path, as ncdu
    /// does, and the listing then has no row of the parent directory when the directory that was
    /// open was the root.
    Memory(Vec<DirectoryId>),
    /// A binary export, whose directories are read when they are opened, each into a tree of
    /// its own that replaces the one before.
    File {
        reader: BinaryReader<File>,
        /// The references of the directories that were opened to reach the open one, from the
        /// root, with the open one last.
        parents: Vec<u64>,
        /// The subdirectories of the open directory, `dir_refs` in `browser.zig`.
        directories: Vec<ListedDirectory>,
    },
}

/// The open directory of the browser, with its entries in the order they are listed.
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
    source: Source,
    state: State,
    info: Info,
    help: Help,
    /// The lines of the message window, which the next key closes.
    message: Option<&'static [&'static str]>,
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
/// `keyInputSelection` in `browser.zig`. Returns whether `key` is a selection key. Other keys do
/// nothing.
fn select_with_key(key: Key, index: &mut usize, length: usize, page: usize) -> bool {
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
        Key::Char(_) | Key::Enter | Key::Backspace | Key::Left | Key::Right | Key::Other => {
            return false;
        }
    }
    true
}

/// The tab that `key` switches the info window of a hard link to.
fn tab_of_key(key: Key) -> Option<Tab> {
    match key {
        Key::Char('1' | 'h') | Key::Left => Some(Tab::Info),
        Key::Char('2' | 'l') | Key::Right => Some(Tab::Links),
        Key::Char(_)
        | Key::Enter
        | Key::Backspace
        | Key::Up
        | Key::Down
        | Key::Home
        | Key::End
        | Key::PageUp
        | Key::PageDown
        | Key::Other => None,
    }
}

/// Format the item count `count` of a directory in 6 columns: the text before the number, the
/// number, and its unit.
fn format_item_count(count: u32, separator: &str) -> (&'static str, String, &'static str) {
    match count {
        0..1000 => ("", format!("  {count:>4}"), ""),
        1000..10_000 => (
            "",
            format!(" {}", format_number(count.into(), separator)),
            "",
        ),
        10_000..100_000 => ("", format_number(count.into(), separator), ""),
        100_000..999_950 => ("", fmt5dec(u64::from(count + 50) / 100), "k"),
        999_950..999_950_000 => ("", fmt5dec(u64::from(count + 50_000) / 100_000), "M"),
        999_950_000.. => ("  > ", "1".to_string(), "G"),
    }
}

/// The screen while the browser lists a directory, which `draw` in `browser.zig` shows while
/// `dir_loading` is set, as `loadDir` calls `handleEvent` in `main.zig` for it.
pub struct Loading<'a> {
    /// The path of the directory that is listed.
    path: &'a [u8],
    /// Whether the directory is read from a binary export.
    from_file: bool,
    options: &'a BrowserOptions,
    /// The number of entries listed so far, hidden ones included.
    count: u64,
}

impl Loading<'_> {
    /// Draw the header, the path, no listing, and the number of entries listed so far in the
    /// footer, on all of `frame`.
    pub fn draw(&self, frame: &mut Frame<'_>) {
        let (width, height) = (frame.area().width, frame.area().height);
        let buffer = frame.buffer_mut();
        let style = |style| self.options.color.style(style);
        draw_header(buffer, self.path, self.from_file, self.options);
        let footer = height.saturating_sub(1);
        draw_line(buffer, 0, footer, ' ', width, style(Style::Hd));
        let x = draw_text(buffer, 0, footer, " Loading... ", style(Style::Hd));
        draw_text(
            buffer,
            x,
            footer,
            &format_number(self.count, &self.options.thousands_separator),
            style(Bg::Hd.fg(Style::Num)),
        );
    }
}

/// Give `loading` the screen of `count` entries listed of the directory at `path`, at the counts
/// where `loadDir` in `browser.zig` calls `handleEvent`: those without the bits of 1 and 64.
fn report_loading(
    loading: &mut dyn FnMut(&Loading<'_>),
    path: &[u8],
    from_file: bool,
    options: &BrowserOptions,
    count: u64,
) {
    if count & 0b100_0001 == 0 {
        loading(&Loading {
            path,
            from_file,
            options,
            count,
        });
    }
}

/// Erase `buffer` and draw the first two rows of the browser in it: the header, marked `[from
/// file]` when `from_file`, and the path `path` of the open directory.
fn draw_header(buffer: &mut Buffer, path: &[u8], from_file: bool, options: &BrowserOptions) {
    let width = buffer.area.width;
    let style = |style| options.color.style(style);
    // ncurses erases the screen to the background of the `default` style before each frame.
    buffer.set_style(buffer.area, style(Style::Default));
    draw_line(buffer, 0, 0, ' ', width, style(Style::Hd));
    let x = draw_text(
        buffer,
        0,
        0,
        concat!(
            "duit ",
            env!("CARGO_PKG_VERSION"),
            " ~ Use the arrow keys to navigate, press "
        ),
        style(Style::Hd),
    );
    let x = draw_text(buffer, x, 0, "?", style(Style::KeyHd));
    draw_text(buffer, x, 0, " for help", style(Style::Hd));
    let mode = if from_file {
        Some("[from file]")
    } else if options.imported {
        Some("[imported]")
    } else if !options.can_delete {
        Some("[readonly]")
    } else {
        None
    };
    if let Some(mode) = mode {
        let x = width.saturating_sub(u16::try_from(mode.len()).unwrap_or(width));
        draw_text(buffer, x, 0, mode, style(Style::Hd));
    }

    draw_line(buffer, 0, 1, '-', width, style(Style::Default));
    let path = to_utf8(path);
    let x = draw_text(buffer, 3, 1, " ", style(Style::Default));
    let x = draw_text(
        buffer,
        x,
        1,
        &shorten(&path, width.saturating_sub(5).into()),
        style(Style::Dir),
    );
    draw_text(buffer, x, 1, " ", style(Style::Default));
}

impl Browser {
    /// Open `directory` of `tree`, with the cursor on the first row. `loading` gets the screen
    /// while the directory is listed.
    pub fn new(
        tree: &Tree,
        directory: DirectoryId,
        options: &BrowserOptions,
        loading: &mut dyn FnMut(&Loading<'_>),
    ) -> Self {
        let mut parents: Vec<DirectoryId> =
            iter::successors(Some(directory), |&id| tree.directory(id).parent).collect();
        parents.reverse();
        Self::with_source(tree, directory, Source::Memory(parents), options, loading)
    }

    /// Open the root of the binary export of `reader`, `initRoot` in `browser.zig`, and return
    /// the browser with the tree of the root, which the browser replaces as it opens other
    /// directories. `None` if the root is not a directory. `loading` gets the screen while the
    /// root is read, whose path is its name.
    pub fn open(
        mut reader: BinaryReader<File>,
        options: &BrowserOptions,
        loading: &mut dyn FnMut(&Loading<'_>),
    ) -> Result<Option<(Self, Tree)>, BinaryImportError> {
        let root = reader.root();
        let Some(Listing {
            tree,
            directory,
            directories,
        }) = reader.read_directory(root, options.extended, &mut |name, count| {
            report_loading(loading, name, true, options, count);
        })?
        else {
            return Ok(None);
        };
        let source = Source::File {
            reader,
            parents: vec![root],
            directories,
        };
        let browser = Self::with_source(&tree, directory, source, options, &mut |_| {});
        Ok(Some((browser, tree)))
    }

    /// Open `directory` of `tree`, which `source` reads, with the cursor on the first row.
    /// `loading` gets the screen while the directory is listed.
    fn with_source(
        tree: &Tree,
        directory: DirectoryId,
        source: Source,
        options: &BrowserOptions,
        loading: &mut dyn FnMut(&Loading<'_>),
    ) -> Self {
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
            source,
            state: State::Main,
            info: Info::default(),
            help: Help::default(),
            message: None,
        };
        browser.load(tree, options, loading);
        browser
    }

    /// Whether the directories are read from a binary export.
    fn reads_file(&self) -> bool {
        match self.source {
            Source::Memory(_) => false,
            Source::File { .. } => true,
        }
    }

    /// The number of directories that were opened to reach the open one, the open one included.
    fn depth(&self) -> usize {
        match &self.source {
            Source::Memory(parents) => parents.len(),
            Source::File { parents, .. } => parents.len(),
        }
    }

    /// Whether the directory `id` of `tree` has no entries.
    fn is_empty(&self, tree: &Tree, id: DirectoryId) -> bool {
        match &self.source {
            Source::Memory(_) => tree.entries(id).next().is_none(),
            Source::File { directories, .. } => directories
                .binary_search_by_key(&id, |directory| directory.id)
                .ok()
                .and_then(|index| directories.get(index))
                .is_some_and(|directory| directory.empty),
        }
    }

    /// List the entries of the directory that `options` shows, then sort them, `loadDir` in
    /// `browser.zig`. `loading` gets the screen while the entries are listed.
    fn load(
        &mut self,
        tree: &Tree,
        options: &BrowserOptions,
        loading: &mut dyn FnMut(&Loading<'_>),
    ) {
        self.items.clear();
        self.max_blocks = 1;
        self.max_size = 1;
        self.has_shared = false;
        if self.depth() > 1 {
            self.items.push(None);
        }
        let from_file = self.reads_file();
        for (count, id) in (1..).zip(tree.entries(self.directory)) {
            report_loading(loading, &self.path, from_file, options, count);
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

    /// Open the subdirectory `directory`, `enterSub` in `browser.zig`, and close the window. From
    /// a binary export, the subdirectory is read into a tree that replaces `tree`.
    fn enter_subdirectory(
        &mut self,
        tree: &mut Tree,
        options: &BrowserOptions,
        directory: DirectoryId,
        loading: &mut dyn FnMut(&Loading<'_>),
    ) -> Result<(), BinaryImportError> {
        let mut path = self.path.clone();
        join_path(&mut path, tree.name(tree.directory(directory).entry));
        let entered = match &mut self.source {
            Source::Memory(parents) => {
                parents.push(directory);
                self.directory = directory;
                true
            }
            Source::File {
                reader,
                parents,
                directories,
            } => {
                // Each subdirectory that is listed has its reference, as in ncdu.
                let itemref = directories
                    .iter()
                    .find(|listed| listed.id == directory)
                    .map(|listed| listed.itemref);
                if let Some(itemref) = itemref {
                    let listing = reader
                        .read_directory(itemref, options.extended, &mut |_, count| {
                            report_loading(loading, &path, true, options, count);
                        })?
                        .ok_or(BinaryImportError::Corrupt { itemref })?;
                    *tree = listing.tree;
                    *directories = listing.directories;
                    parents.push(itemref);
                    self.directory = listing.directory;
                }
                itemref.is_some()
            }
        };
        if entered {
            self.path = path;
        }
        self.load_entered(tree, options, loading);
        Ok(())
    }

    /// Open the parent directory, if there is one, `enterParent` in `browser.zig`, and close the
    /// window. From a binary export, the parent directory is read again into a tree that replaces
    /// `tree`.
    fn enter_parent(
        &mut self,
        tree: &mut Tree,
        options: &BrowserOptions,
        loading: &mut dyn FnMut(&Loading<'_>),
    ) -> Result<(), BinaryImportError> {
        if self.depth() < 2 {
            return Ok(());
        }
        // An empty name leaves a path without a separator to cut, for example a root named `foo`
        // with a subdirectory without a name, which a scan never gives. ncdu reaches `unreachable`
        // there.
        let path = dirname(&self.path).map(<[u8]>::to_vec);
        match &mut self.source {
            Source::Memory(parents) => {
                parents.pop();
                let Some(&parent) = parents.last() else {
                    return Ok(());
                };
                self.directory = parent;
            }
            Source::File {
                reader,
                parents,
                directories,
            } => {
                // The open directory leaves `parents` only once its parent is read, so an error
                // leaves the browser where it was.
                let Some(&itemref) = parents
                    .len()
                    .checked_sub(2)
                    .and_then(|index| parents.get(index))
                else {
                    return Ok(());
                };
                let listing = reader
                    .read_directory(itemref, options.extended, &mut |_, count| {
                        let path = path.as_deref().unwrap_or_default();
                        report_loading(loading, path, true, options, count);
                    })?
                    .ok_or(BinaryImportError::Corrupt { itemref })?;
                parents.pop();
                *tree = listing.tree;
                *directories = listing.directories;
                self.directory = listing.directory;
            }
        }
        self.path = path.unwrap_or_else(|| path_of(tree, self.directory));
        self.load_entered(tree, options, loading);
        Ok(())
    }

    /// List the directory that [`Browser::enter_subdirectory`] or [`Browser::enter_parent`]
    /// opened, and close any window. `loading` gets the screen while the entries are listed,
    /// unless a binary export already reported the read as the listing.
    fn load_entered(
        &mut self,
        tree: &Tree,
        options: &BrowserOptions,
        loading: &mut dyn FnMut(&Loading<'_>),
    ) {
        if self.reads_file() {
            self.load(tree, options, &mut |_| {});
        } else {
            self.load(tree, options, loading);
        }
        self.state = State::Main;
    }

    /// Show the info of `item` in `tab`, or close the window when `item` is the row of the parent
    /// directory or `None`, `info.set` in `browser.zig`.
    fn set_info(&mut self, tree: &Tree, item: Option<EntryId>, tab: Tab) {
        if item != self.info.entry {
            self.info = Info::default();
        }
        self.info.entry = item;
        let Some(entry) = item else {
            self.state = State::Main;
            return;
        };
        self.state = State::Info;
        self.info.tab = tab;
        if tab == Tab::Links
            && !self.reads_file()
            && self.info.links.is_none()
            && let EntryKind::HardLink(link) = tree.entry(entry).kind
        {
            let mut links: Vec<LinkId> = tree.links(link).collect();
            links.sort_by_cached_key(|&link| tree.link_path(link, false));
            if let Some(index) = links
                .iter()
                .position(|&link| tree.link(link).entry == entry)
            {
                self.info.links_index = index;
            }
            self.info.links = Some(links);
        }
    }

    /// Handle `key` in the info window, `info.keyInput` in `browser.zig`. Returns whether the key
    /// was handled, or else the listing handles it too.
    fn info_key_input(
        &mut self,
        tree: &Tree,
        options: &BrowserOptions,
        key: Key,
        height: u16,
        loading: &mut dyn FnMut(&Loading<'_>),
    ) -> bool {
        let Some(entry) = self.info.entry else {
            return false;
        };
        if let EntryKind::HardLink(_) = tree.entry(entry).kind
            && let Some(tab) = tab_of_key(key)
        {
            self.set_info(tree, Some(entry), tab);
            return true;
        }
        if self.info.tab == Tab::Links && !self.reads_file() {
            let links = self.info.links.as_deref().unwrap_or_default();
            if select_with_key(key, &mut self.info.links_index, links.len(), 5) {
                return true;
            }
            // Enter opens the directory of the selected link, closes the window, and then reaches
            // the listing as well.
            if key == Key::Enter
                && let Some(&link) = links.get(self.info.links_index)
            {
                let link = tree.link(link);
                self.directory = link.parent;
                self.load(tree, options, loading);
                let name = tree.name(link.entry);
                self.cursor = self
                    .items
                    .iter()
                    .position(|item| item.map(|id| tree.name(id)) == Some(name))
                    .unwrap_or(0);
                self.set_info(tree, None, Tab::Info);
            }
        }
        let page = usize::from(height.saturating_sub(3));
        if select_with_key(key, &mut self.cursor, self.items.len(), page) {
            let item = self.items.get(self.cursor).copied().flatten();
            self.set_info(tree, item, Tab::Info);
            return true;
        }
        match key {
            Key::Char('i' | 'q') => {
                self.set_info(tree, None, Tab::Info);
                true
            }
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
            | Key::Other => false,
        }
    }

    /// Handle `key` in the help window, `help.keyInput` in `browser.zig`. A key that is not one
    /// of the window closes it. The keys tab starts at the top again when the tab changes or the
    /// window closes.
    fn help_key_input(&mut self, key: Key) {
        let help = &mut self.help;
        let tab = help.tab;
        match key {
            Key::Char('1') => help.tab = HelpTab::Keys,
            Key::Char('2') => help.tab = HelpTab::Format,
            Key::Char('3') => help.tab = HelpTab::About,
            Key::Char('h') | Key::Left => {
                help.tab = match help.tab {
                    HelpTab::About => HelpTab::Format,
                    HelpTab::Keys | HelpTab::Format => HelpTab::Keys,
                };
            }
            Key::Char('l') | Key::Right => {
                help.tab = match help.tab {
                    HelpTab::Keys => HelpTab::Format,
                    HelpTab::Format | HelpTab::About => HelpTab::About,
                };
            }
            Key::Char('j' | ' ') | Key::Down | Key::PageDown => {
                let max = match help.tab {
                    HelpTab::Keys => HELP_KEYS.len() - HELP_KEY_LINES,
                    HelpTab::Format | HelpTab::About => 0,
                };
                if help.offset < max {
                    help.offset += 1;
                }
            }
            Key::Char('k') | Key::Up | Key::PageUp => help.offset = help.offset.saturating_sub(1),
            Key::Char(_) | Key::Enter | Key::Backspace | Key::Home | Key::End | Key::Other => {
                self.state = State::Main;
            }
        }
        if help.tab != tab || self.state != State::Help {
            help.offset = 0;
        }
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

    /// The path of the open directory, `dir_path` in `browser.zig`.
    pub fn path(&self) -> &[u8] {
        &self.path
    }

    /// Handle `key` on a terminal with `height` rows, which sets the size of a page, and change
    /// `options` as the display and sort keys say, `keyInput` in `browser.zig`. Returns what the
    /// main loop does next, or the error of a binary export whose directory could not be read,
    /// which ncdu exits with.
    ///
    /// `tree` is the tree that the browser was opened with. From a binary export, the browser
    /// replaces it with each directory that it opens. `loading` gets the screen while a directory
    /// is listed.
    pub fn key_input(
        &mut self,
        tree: &mut Tree,
        options: &mut BrowserOptions,
        key: Key,
        height: u16,
        loading: &mut dyn FnMut(&Loading<'_>),
    ) -> Result<Action, BinaryImportError> {
        let action = self.handle_key(tree, options, key, height, loading);
        self.save_view(tree);
        action
    }

    fn handle_key(
        &mut self,
        tree: &mut Tree,
        options: &mut BrowserOptions,
        key: Key,
        height: u16,
        loading: &mut dyn FnMut(&Loading<'_>),
    ) -> Result<Action, BinaryImportError> {
        if self.message.take().is_some() {
            return Ok(Action::Browse);
        }
        match self.state {
            State::Main => {}
            State::Quit => {
                self.state = State::Main;
                let quit = key == Key::Char('y') || key == Key::Char('Y');
                return Ok(if quit { Action::Quit } else { Action::Browse });
            }
            State::Help => {
                self.help_key_input(key);
                return Ok(Action::Browse);
            }
            State::Info => {
                if self.info_key_input(tree, options, key, height, loading) {
                    return Ok(Action::Browse);
                }
            }
        }

        match key {
            Key::Char('q') => {
                if !options.confirm_quit {
                    return Ok(Action::Quit);
                }
                self.state = State::Quit;
            }
            Key::Char('i') => {
                if let Some(&item) = self.items.get(self.cursor) {
                    self.set_info(tree, item, Tab::Info);
                }
            }
            Key::Char('?') => self.state = State::Help,
            // The actions of refresh and delete are not ported yet.
            Key::Char('r') => {
                if self.reads_file() {
                    self.message =
                        Some(&["Refresh feature is not available when reading from file."]);
                } else if !options.can_refresh && options.imported {
                    self.message = Some(&[
                        "Refresh feature disabled.",
                        "Re-run with --enable-refresh to enable this option.",
                    ]);
                } else if !options.can_refresh {
                    self.message = Some(&["Directory refresh feature disabled."]);
                }
            }
            Key::Char('b') => {
                if !options.can_shell {
                    self.message = Some(&[
                        "Shell feature disabled.",
                        "Re-run with --enable-shell to enable this option.",
                    ]);
                } else {
                    return Ok(Action::Shell);
                }
            }
            Key::Char('d') if self.items.is_empty() => {}
            Key::Char('d') => {
                if self.reads_file() {
                    self.message =
                        Some(&["File deletion is not available when reading from file."]);
                } else if !options.can_delete && options.imported {
                    self.message = Some(&[
                        "File deletion is disabled.",
                        "Re-run with --enable-delete to enable this option.",
                    ]);
                } else if !options.can_delete {
                    self.message = Some(&["File deletion is disabled."]);
                }
            }

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
                self.load(tree, options, loading);
                self.state = State::Main;
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
                        self.enter_subdirectory(tree, options, directory, loading)?;
                    }
                }
                Some(None) => self.enter_parent(tree, options, loading)?,
                None => {}
            },
            Key::Char('h' | '<') | Key::Backspace | Key::Left => {
                self.enter_parent(tree, options, loading)?;
            }

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
            | Key::PageDown
            | Key::Other => {
                select_with_key(
                    key,
                    &mut self.cursor,
                    self.items.len(),
                    usize::from(height.saturating_sub(3)),
                );
            }
        }
        Ok(Action::Browse)
    }

    /// Draw the browser on all of `frame`, which starts at the top left corner of the terminal:
    /// the header, the path, the rows of the listing that fit, and the totals of the directory in
    /// the footer.
    pub fn draw(&mut self, frame: &mut Frame<'_>, tree: &Tree, options: &BrowserOptions) {
        let (width, height) = (frame.area().width, frame.area().height);
        let buffer = frame.buffer_mut();
        let directory = tree.directory(self.directory);
        let directory_entry = tree.entry(directory.entry);
        let scheme = options.color;
        let style = |style| scheme.style(style);

        draw_header(buffer, &self.path, self.reads_file(), options);

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
                bg: if index == self.cursor {
                    Bg::Sel
                } else {
                    Bg::Default
                },
                number_style: false,
            };
            row.draw(buffer);
        }

        let footer = height.saturating_sub(1);
        draw_line(buffer, 0, footer, ' ', width, style(Style::Hd));
        let (disk_usage_style, apparent_size_style) = if options.show_blocks {
            (Style::BoldHd, Style::Hd)
        } else {
            (Style::Hd, Style::BoldHd)
        };
        let x = draw_text(
            buffer,
            0,
            footer,
            if options.show_blocks { "*" } else { " " },
            style(Style::Hd),
        );
        let x = draw_text(
            buffer,
            x,
            footer,
            "Total disk usage: ",
            style(disk_usage_style),
        );
        let x = draw_size(
            buffer,
            x,
            footer,
            blocks_to_size(directory_entry.blocks),
            options.si,
            scheme,
            Bg::Hd,
        );
        let x = draw_text(
            buffer,
            x,
            footer,
            if options.show_blocks { "   " } else { "  *" },
            style(apparent_size_style),
        );
        let x = draw_text(
            buffer,
            x,
            footer,
            "Apparent size: ",
            style(apparent_size_style),
        );
        let x = draw_size(
            buffer,
            x,
            footer,
            directory_entry.size,
            options.si,
            scheme,
            Bg::Hd,
        );
        let x = draw_text(buffer, x, footer, "   Items: ", style(Style::Hd));
        draw_text(
            buffer,
            x,
            footer,
            &format_number(directory.items.into(), &options.thousands_separator),
            style(Bg::Hd.fg(Style::Num)),
        );

        match self.state {
            State::Main => {}
            State::Quit => {
                let window = Window::new(buffer, scheme, 4, 22, "Confirm quit");
                let x = window.text(buffer, 2, 2, "Really quit? (", Style::Default);
                let x = window.text(buffer, 2, x, "y", Style::Key);
                let x = window.text(buffer, 2, x, "/", Style::Default);
                let x = window.text(buffer, 2, x, "N", Style::Key);
                window.text(buffer, 2, x, ")", Style::Default);
            }
            State::Help => self.help.draw(buffer, scheme),
            State::Info => {
                if let Some(&Some(entry)) = self.items.get(self.cursor) {
                    let from_file = self.reads_file();
                    self.info.draw(buffer, tree, options, entry, from_file);
                }
            }
        }
        if let Some(lines) = self.message {
            let height = u16::try_from(lines.len())
                .unwrap_or(u16::MAX)
                .saturating_add(5);
            let window = Window::new(buffer, scheme, height, 60, "Message");
            for (row, line) in (2..).zip(lines) {
                window.text(buffer, row, 2, line, Style::Default);
            }
            window.text(
                buffer,
                height - 2,
                33,
                "Press any key to continue",
                Style::Default,
            );
        }
    }
}

/// The keys of the browser and what they do, on the keys tab of the help window.
const HELP_KEYS: [(&str, &str); 20] = [
    ("up, k", "Move cursor up"),
    ("down, j", "Move cursor down"),
    ("right/enter", "Open selected directory"),
    ("left, <, h", "Open parent directory"),
    ("n", "Sort by name (ascending/descending)"),
    ("s", "Sort by size (ascending/descending)"),
    ("C", "Sort by items (ascending/descending)"),
    ("M", "Sort by mtime (-e flag)"),
    ("d", "Delete selected file or directory"),
    ("t", "Toggle dirs before files when sorting"),
    ("g", "Show percentage and/or graph"),
    ("u", "Show/hide hard link shared sizes"),
    ("a", "Toggle between apparent size and disk usage"),
    ("c", "Toggle display of child item counts"),
    ("m", "Toggle display of latest mtime (-e flag)"),
    ("e", "Show/hide hidden or excluded files"),
    ("i", "Show information about selected item"),
    ("r", "Recalculate the current directory"),
    ("b", "Spawn shell in current directory"),
    ("q", "Quit ncdu"),
];

/// The number of keys that the keys tab shows at a time.
const HELP_KEY_LINES: usize = 10;

/// The flags in front of the entries of the listing, on the format tab of the help window.
const HELP_FLAGS: [(&str, &str); 8] = [
    ("!", "An error occurred while reading this directory"),
    (".", "An error occurred while reading a subdirectory"),
    ("<", "File or directory is excluded from the statistics"),
    ("e", "Empty directory"),
    (">", "Directory was on another filesystem"),
    ("@", "This is not a file nor a dir (symlink, socket, ...)"),
    ("^", "Excluded Linux pseudo-filesystem"),
    ("H", "Same file was already counted (hard link)"),
];

/// The rows of the logo on the about tab, one bit per column, of which bits 28 down to 1 are drawn
/// and a set bit is drawn in reverse video.
const HELP_LOGO: [u32; 5] = [
    0b1_1111_1001_1111_0000_0011_0011_0011,
    0b1_1001_1001_1000_0000_0011_0011_0011,
    0b1_1001_1001_1000_0011_1111_0011_0011,
    0b1_1001_1001_1000_0011_0011_0011_0011,
    0b1_1001_1001_1111_0011_1111_0011_1111,
];

impl Help {
    /// Draw the window over the listing with its current tab.
    fn draw(&self, buffer: &mut Buffer, scheme: ColorScheme) {
        let window = Window::new(buffer, scheme, 15, 60, "ncdu help");
        window.tab(buffer, 30, self.tab == HelpTab::Keys, "1", "Keys");
        window.tab(buffer, 39, self.tab == HelpTab::Format, "2", "Format");
        window.tab(buffer, 50, self.tab == HelpTab::About, "3", "About");

        let x = window.text(buffer, 13, 42, "Press ", Style::Default);
        let x = window.text(buffer, 13, x, "q", Style::Key);
        window.text(buffer, 13, x, " to close", Style::Default);

        match self.tab {
            HelpTab::Keys => self.draw_keys(&window, buffer),
            HelpTab::Format => draw_help_format(&window, buffer),
            HelpTab::About => draw_help_about(&window, buffer),
        }
    }

    /// Draw the keys from `offset`, right aligned, with what they do.
    fn draw_keys(&self, window: &Window, buffer: &mut Buffer) {
        let lines = HELP_KEYS.iter().skip(self.offset).take(HELP_KEY_LINES);
        for (row, &(key, description)) in (2..).zip(lines) {
            let column = 13u16.saturating_sub(u16::try_from(key.len()).unwrap_or(13));
            let x = window.text(buffer, row, column, key, Style::Key);
            window.text(buffer, row, x, &format!(" {description}"), Style::Default);
        }
        if self.offset < HELP_KEYS.len() - HELP_KEY_LINES {
            window.text(buffer, 12, 25, "-- more --", Style::Default);
        }
    }
}

/// Draw the format of a row of the listing, and the flags in front of the entries.
fn draw_help_format(window: &Window, buffer: &mut Buffer) {
    window.text(
        buffer,
        2,
        3,
        "X  [size] [graph] [file or directory]",
        Style::Bold,
    );
    window.text(
        buffer,
        3,
        4,
        "The X is only present in the following cases:",
        Style::Default,
    );
    for (row, &(flag, description)) in (5..).zip(HELP_FLAGS.iter()) {
        let x = window.text(buffer, row, 4, flag, Style::Flag);
        window.text(buffer, row, x, &format!(" {description}"), Style::Default);
    }
}

/// Draw the logo, the name, the version, and the author. The version is that of duit, as in the
/// header of the browser.
fn draw_help_about(window: &Window, buffer: &mut Buffer) {
    for (row, line) in (3..).zip(HELP_LOGO) {
        for (column, bit) in (12..).zip((1..=28).rev()) {
            let style = if line & (1 << bit) > 0 {
                Style::Sel
            } else {
                Style::Default
            };
            window.text(buffer, row, column, " ", style);
        }
    }
    window.text(buffer, 3, 43, "NCurses", Style::Default);
    window.text(buffer, 4, 43, "Disk", Style::Default);
    window.text(buffer, 5, 43, "Usage", Style::Default);
    window.text(buffer, 7, 43, env!("CARGO_PKG_VERSION"), Style::Num);
    window.text(
        buffer,
        9,
        11,
        "Written by Yorhel <projects@yorhel.nl>",
        Style::Default,
    );
    window.text(buffer, 10, 16, "https://dev.yorhel.nl/ncdu", Style::Default);
}

/// The width of the info window.
const INFO_WIDTH: u16 = 60;

impl Info {
    /// Draw the window over the listing with the info of `entry`, the entry under the cursor. The
    /// links of an entry that is read `from_file` are not known.
    fn draw(
        &mut self,
        buffer: &mut Buffer,
        tree: &Tree,
        options: &BrowserOptions,
        entry: EntryId,
        from_file: bool,
    ) {
        let data = tree.entry(entry);
        let link = match data.kind {
            EntryKind::HardLink(link) => Some(link),
            EntryKind::Directory(_)
            | EntryKind::Regular
            | EntryKind::NonRegular
            | EntryKind::Special(_) => None,
        };
        let directory = directory_of(data).map(|id| tree.directory(id));
        // The border, the padding, and the row that says how to close the window, and the rows of
        // the tab.
        let height = 5 + match self.tab {
            Tab::Links if !from_file => 8,
            Tab::Links | Tab::Info => {
                4 + u16::from(tree.extended(entry).is_some())
                    + u16::from(link.is_some())
                    + directory.map_or(0, |directory| {
                        1 + if directory.shared_size > 0 { 2 } else { 0 }
                            + if directory.shared_blocks > 0 { 2 } else { 0 }
                    })
            }
        };
        let window = Window::new(buffer, options.color, height, INFO_WIDTH, "Item info");
        if link.is_some() {
            window.tab(buffer, INFO_WIDTH - 19, self.tab == Tab::Info, "1", "Info");
            window.tab(
                buffer,
                INFO_WIDTH - 10,
                self.tab == Tab::Links,
                "2",
                "Links",
            );
        }

        match self.tab {
            Tab::Info => draw_info_tab(&window, buffer, tree, options, entry),
            Tab::Links if from_file => {
                let message = "This feature is not available when reading from file.";
                window.text(buffer, 2, 2, message, Style::Default);
            }
            Tab::Links => self.draw_links(&window, buffer, tree, height),
        }

        let row = height - 2;
        let x = window.text(buffer, row, INFO_WIDTH - 30, "Press ", Style::Default);
        let x = window.text(buffer, row, x, "i", Style::Key);
        window.text(buffer, row, x, " to close this window", Style::Default);
    }

    /// Draw the paths of the links to the inode on the rows of a window of `height`, the selected
    /// one in [`Style::Sel`] and the one of the entry marked with `*`, and the number of the
    /// selected one.
    fn draw_links(&mut self, window: &Window, buffer: &mut Buffer, tree: &Tree, height: u16) {
        let rows = usize::from(height.saturating_sub(4));
        if self.links_index < self.links_top {
            self.links_top = self.links_index;
        }
        if self.links_index >= self.links_top + rows {
            self.links_top = self.links_index + 1 - rows;
        }
        let links = self.links.as_deref().unwrap_or_default();
        for (row, (index, &link)) in (2..)
            .zip(links.iter().enumerate().skip(self.links_top))
            .take(rows)
        {
            let style = if index == self.links_index {
                Style::Sel
            } else {
                Style::Default
            };
            let mark = if Some(tree.link(link).entry) == self.entry {
                "*"
            } else {
                " "
            };
            let x = window.text(buffer, row, 2, mark, style);
            let path = tree.link_path(link, false);
            let path = shorten(&to_utf8(&path), usize::from(INFO_WIDTH - 5)).into_owned();
            window.text(buffer, row, x, &path, style);
        }
        let position = format!("{:>3}/{}", self.links_index + 1, links.len());
        window.text(buffer, height - 2, 4, &position, Style::Default);
    }
}

/// Draw the info tab of `entry` in `window`: its name, its type or its extended information, its
/// sizes, and its item count or its link count and inode.
fn draw_info_tab(
    window: &Window,
    buffer: &mut Buffer,
    tree: &Tree,
    options: &BrowserOptions,
    entry: EntryId,
) {
    let data = tree.entry(entry);
    let extended = tree.extended(entry);
    let separator = &options.thousands_separator;
    let mut row = 2;
    let x = window.text(buffer, row, 3, "Name: ", Style::Bold);
    let name = shorten(&to_utf8(tree.name(entry)), usize::from(INFO_WIDTH - 11)).into_owned();
    window.text(buffer, row, x, &name, Style::Default);
    row += 1;

    if let Some(extended) = extended {
        let extended = extended.extended;
        let mut x = 3;
        if let Some(mode) = extended.mode {
            x = window.text(buffer, row, x, "Mode: ", Style::Bold);
            x = window.text(buffer, row, x, &format_mode(mode), Style::Default);
        }
        if let Some(uid) = extended.uid {
            x = window.text(buffer, row, x, "  UID: ", Style::Bold);
            x = window.text(buffer, row, x, &format!("{uid:<6}"), Style::Default);
        }
        if let Some(gid) = extended.gid {
            x = window.text(buffer, row, x, " GID: ", Style::Bold);
            window.text(buffer, row, x, &format!("{gid:<6}"), Style::Default);
        }
    } else {
        let x = window.text(buffer, row, 3, "Type: ", Style::Bold);
        let kind = match data.kind {
            EntryKind::Directory(_) => "Directory",
            EntryKind::NonRegular => "Other",
            EntryKind::Regular | EntryKind::HardLink(_) => "File",
            EntryKind::Special(_) => "Excluded",
        };
        window.text(buffer, row, x, kind, Style::Default);
    }
    row += 1;

    if let Some(extended) = extended
        && extended.extended.mtime.is_some()
    {
        let x = window.text(buffer, row, 3, "Last modified: ", Style::Bold);
        if let Some(mtime) = format_timestamp(extended.latest_mtime) {
            window.text(buffer, row, x, &mtime, Style::Num);
        } else {
            window.text(buffer, row, x, "            invalid mtime", Style::Default);
        }
        row += 1;
    }

    // A size in full and in a human-readable form, and its shared and unique parts when it has a
    // shared part.
    let sizes = |buffer: &mut Buffer, row: &mut u16, label: &str, size: u64, shared: u64| {
        let mut size_row = |label: &str, style, size| {
            let x = window.text(buffer, *row, 3, label, style);
            let x = window.size(buffer, *row, x, size, options.si);
            let x = window.text(buffer, *row, x, " (", Style::Default);
            let x = window.number(buffer, *row, x, size, separator);
            window.text(buffer, *row, x, ")", Style::Default);
            *row += 1;
        };
        size_row(label, Style::Bold, size);
        if shared > 0 {
            size_row("     > shared: ", Style::Default, shared);
            size_row(
                "     > unique: ",
                Style::Default,
                size.saturating_sub(shared),
            );
        }
    };
    let directory = directory_of(data).map(|id| tree.directory(id));
    let shared_blocks = directory.map_or(0, |directory| blocks_to_size(directory.shared_blocks));
    let shared_size = directory.map_or(0, |directory| directory.shared_size);
    let disk_usage = blocks_to_size(data.blocks);
    sizes(
        buffer,
        &mut row,
        "   Disk usage: ",
        disk_usage,
        shared_blocks,
    );
    sizes(buffer, &mut row, "Apparent size: ", data.size, shared_size);

    if let Some(directory) = directory {
        let x = window.text(buffer, row, 3, "    Sub items: ", Style::Bold);
        window.number(buffer, row, x, directory.items.into(), separator);
        row += 1;
    }

    if let EntryKind::HardLink(link) = data.kind {
        let link = tree.link(link);
        let x = window.text(buffer, row, 3, "   Link count: ", Style::Bold);
        window.number(buffer, row, x, link.link_count.into(), separator);
        let x = window.text(buffer, row, 23, "  Inode: ", Style::Bold);
        window.text(buffer, row, x, &link.inode.to_string(), Style::Default);
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
    /// [`Bg::Sel`] on the cursor row, and [`Bg::Default`] on the others.
    bg: Bg,
    /// The item count left [`Style::Num`] set, which ncdu draws the next text without its own
    /// style in.
    number_style: bool,
}

impl Row<'_> {
    /// The columns that are left from `x` to the right edge of the screen.
    fn remaining(&self) -> u16 {
        self.width.saturating_sub(self.x)
    }

    /// The colors and attributes of the foreground `style` on the background of the row.
    fn style(&self, style: Style) -> StyleAttr {
        self.options.color.style(self.bg.fg(style))
    }

    fn draw(mut self, buffer: &mut Buffer) {
        match self.bg {
            Bg::Sel => {
                let style = self.style(Style::Default);
                draw_line(buffer, self.x, self.y, ' ', self.width, style);
            }
            Bg::Default | Bg::Hd => {}
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
                } else if self.browser.is_empty(self.tree, id) {
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
        draw_text(buffer, x, self.y, flag, self.style(Style::Flag));
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

        let (scheme, bg) = (options.color, self.bg);
        let x = draw_size(buffer, x, self.y, size, options.si, scheme, bg);
        if shared_column && shared > 0 {
            let label = match options.shared_column {
                SharedColumn::Unique => " U ",
                SharedColumn::Shared | SharedColumn::Off => " S ",
            };
            let x = draw_text(buffer, x, self.y, label, self.style(Style::Flag));
            draw_size(buffer, x, self.y, shared, options.si, scheme, bg);
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

        let mut x = draw_text(buffer, x, self.y, "[", self.style(Style::Default));
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
            x = draw_text(
                buffer,
                x,
                self.y,
                &fmt5dec(permille),
                self.style(Style::Num),
            );
            x = draw_text(buffer, x, self.y, "%", self.style(Style::Default));
        }
        if options.show_graph && options.show_percent {
            x = draw_text(buffer, x, self.y, " ", self.style(Style::Default));
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
            let mut bar = String::new();
            for _ in 0..bar_size {
                let eighths = (number.saturating_mul(8) / per_block).min(8);
                bar.push(
                    match options.graph_style {
                        GraphStyle::Hash => [' ', ' ', ' ', ' ', ' ', ' ', ' ', ' ', '#'],
                        GraphStyle::HalfBlock => [' ', ' ', ' ', ' ', '▌', '▌', '▌', '▌', '█'],
                        GraphStyle::EighthBlock => [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'],
                    }[usize::try_from(eighths).unwrap_or(8)],
                );
                number = number.saturating_sub(per_block);
            }
            x = draw_text(buffer, x, self.y, &bar, self.style(Style::Graph));
        }
        draw_text(buffer, x, self.y, "]", self.style(Style::Default));
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
        let count = self.tree.directory(directory).items;
        let (before, number, unit) = format_item_count(count, &self.options.thousands_separator);
        let x = draw_text(buffer, x, self.y, before, self.style(Style::Default));
        let x = draw_text(buffer, x, self.y, &number, self.style(Style::Num));
        draw_text(buffer, x, self.y, unit, self.style(Style::Default));
        // ncdu sets the style of the number last for a count below 1000.
        self.number_style = count < 1000;
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
        let (text, style) = match self.tree.extended(entry) {
            Some(extended) if extended.extended.mtime.is_some() => {
                match format_timestamp(extended.latest_mtime) {
                    Some(mtime) => (mtime, Style::Num),
                    None => ("            invalid mtime".to_string(), Style::Default),
                }
            }
            Some(_) | None => {
                let style = if self.number_style {
                    Style::Num
                } else {
                    Style::Default
                };
                ("                 no mtime".to_string(), style)
            }
        };
        draw_text(buffer, x, self.y, &text, self.style(style));
    }

    fn name(&self, buffer: &mut Buffer) {
        let Some(item) = self.item else {
            draw_text(buffer, self.x, self.y, "/..", self.style(Style::Dir));
            return;
        };
        let kind = self.tree.entry(item).kind;
        let style = match kind {
            EntryKind::Directory(_) => self.style(Style::Dir),
            EntryKind::HardLink(_)
            | EntryKind::Regular
            | EntryKind::NonRegular
            | EntryKind::Special(_) => self.style(Style::Default),
        };
        let prefix = if is_directory(kind) { "/" } else { " " };
        let x = draw_text(buffer, self.x, self.y, prefix, style);
        let name = to_utf8(self.tree.name(item));
        let width = self.remaining().saturating_sub(1);
        draw_text(buffer, x, self.y, &shorten(&name, width.into()), style);
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
            .map(|count| {
                let (before, number, unit) = format_item_count(count, ",");
                format!("{before}{number}{unit}")
            })
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
