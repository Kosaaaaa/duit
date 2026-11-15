use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::ffi::OsStrExt;
use std::time::Duration;

use duit_scan::ExcludePatterns;

use crate::{Arg, Args, open_error};

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

/// The interface during a scan: `-0`, `-1`, or `-2`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanUi {
    Off,
    Line,
    Full,
}

/// The color scheme, `--color`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Off,
    Dark,
    DarkBackground,
}

/// Why [`Config::apply`] rejected an option.
#[derive(Debug)]
pub enum OptionError {
    /// The option is not a configuration option.
    Unknown,
    /// The option or its value is invalid. Holds the message that ncdu prints.
    Invalid(Vec<u8>),
}

impl From<Vec<u8>> for OptionError {
    fn from(message: Vec<u8>) -> Self {
        Self::Invalid(message)
    }
}

/// The options that can be given on the command line and in config files, with ncdu's defaults.
pub struct Config {
    pub update_delay: Duration,
    pub same_filesystem: bool,
    pub extended: bool,
    pub follow_symlinks: bool,
    pub exclude: ExcludePatterns,
    pub exclude_caches: bool,
    pub exclude_kernfs: bool,
    /// The number of scan threads, where 0 means one per CPU.
    pub threads: usize,
    pub compress: bool,
    pub compress_level: u8,
    /// The block size of a binary export in bytes.
    pub export_block_size: Option<usize>,
    pub scan_ui: Option<ScanUi>,
    pub si: bool,
    pub color: Color,
    pub show_hidden: bool,
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
    pub can_delete: Option<bool>,
    pub can_shell: Option<bool>,
    pub can_refresh: Option<bool>,
    pub confirm_quit: bool,
    pub confirm_delete: bool,
    pub delete_command: Vec<u8>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            update_delay: Duration::from_millis(100),
            same_filesystem: false,
            extended: false,
            follow_symlinks: false,
            exclude: ExcludePatterns::default(),
            exclude_caches: false,
            exclude_kernfs: false,
            threads: 1,
            compress: false,
            compress_level: 4,
            export_block_size: None,
            scan_ui: None,
            si: false,
            color: Color::Off,
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
            can_delete: None,
            can_shell: None,
            can_refresh: None,
            confirm_quit: false,
            confirm_delete: true,
            delete_command: Vec::new(),
        }
    }
}

impl Config {
    /// Apply `option`, and read its value from `args` if it takes one. Of each pair of options
    /// that turn a setting on and off, the last one wins.
    pub fn apply(&mut self, args: &mut Args<'_>, option: Arg<'_>) -> Result<(), OptionError> {
        match option {
            Arg::Short(b'q') | Arg::Long(b"slow-ui-updates") => {
                self.update_delay = Duration::from_secs(2);
            }
            Arg::Long(b"fast-ui-updates") => self.update_delay = Duration::from_millis(100),
            Arg::Short(b'x') | Arg::Long(b"one-file-system") => self.same_filesystem = true,
            Arg::Long(b"cross-file-system") => self.same_filesystem = false,
            Arg::Short(b'e') | Arg::Long(b"extended") => self.extended = true,
            Arg::Long(b"no-extended") => self.extended = false,
            // A second `-r` also disables the shell.
            Arg::Short(b'r') if self.can_delete == Some(false) => self.can_shell = Some(false),
            Arg::Short(b'r') => self.can_delete = Some(false),
            Arg::Long(b"enable-shell") => self.can_shell = Some(true),
            Arg::Long(b"disable-shell") => self.can_shell = Some(false),
            Arg::Long(b"enable-delete") => self.can_delete = Some(true),
            Arg::Long(b"disable-delete") => self.can_delete = Some(false),
            Arg::Long(b"enable-refresh") => self.can_refresh = Some(true),
            Arg::Long(b"disable-refresh") => self.can_refresh = Some(false),
            Arg::Long(b"show-hidden") => self.show_hidden = true,
            Arg::Long(b"hide-hidden") => self.show_hidden = false,
            Arg::Long(b"show-itemcount") => self.show_item_count = true,
            Arg::Long(b"hide-itemcount") => self.show_item_count = false,
            Arg::Long(b"show-mtime") => self.show_mtime = true,
            Arg::Long(b"hide-mtime") => self.show_mtime = false,
            Arg::Long(b"show-graph") => self.show_graph = true,
            Arg::Long(b"hide-graph") => self.show_graph = false,
            Arg::Long(b"show-percent") => self.show_percent = true,
            Arg::Long(b"hide-percent") => self.show_percent = false,
            Arg::Long(b"group-directories-first") => self.sort_directories_first = true,
            Arg::Long(b"no-group-directories-first") => self.sort_directories_first = false,
            Arg::Long(b"enable-natsort") => self.sort_natural = true,
            Arg::Long(b"disable-natsort") => self.sort_natural = false,
            Arg::Long(b"graph-style") => {
                self.graph_style = match args.value()? {
                    b"hash" => GraphStyle::Hash,
                    b"half-block" => GraphStyle::HalfBlock,
                    // ncdu also accepts the misspelling.
                    b"eighth-block" | b"eigth-block" => GraphStyle::EighthBlock,
                    value => return Err(unknown_value(b"--graph-style", value)),
                };
            }
            Arg::Long(b"sort") => {
                let value = args.value()?;
                let (column, order) = if let Some(column) = value.strip_suffix(b"-asc") {
                    (column, Some(SortOrder::Ascending))
                } else if let Some(column) = value.strip_suffix(b"-desc") {
                    (column, Some(SortOrder::Descending))
                } else {
                    (value, None)
                };
                let (column, default_order) = match column {
                    b"name" => (SortColumn::Name, SortOrder::Ascending),
                    b"disk-usage" => (SortColumn::Blocks, SortOrder::Descending),
                    b"apparent-size" => (SortColumn::Size, SortOrder::Descending),
                    b"itemcount" => (SortColumn::Items, SortOrder::Descending),
                    b"mtime" => (SortColumn::Mtime, SortOrder::Ascending),
                    column => return Err(unknown_value(b"--sort", column)),
                };
                self.sort_column = column;
                self.sort_order = order.unwrap_or(default_order);
            }
            Arg::Long(b"shared-column") => {
                self.shared_column = match args.value()? {
                    b"off" => SharedColumn::Off,
                    b"shared" => SharedColumn::Shared,
                    b"unique" => SharedColumn::Unique,
                    value => return Err(unknown_value(b"--shared-column", value)),
                };
            }
            Arg::Long(b"apparent-size") => self.show_blocks = false,
            Arg::Long(b"disk-usage") => self.show_blocks = true,
            Arg::Short(b'0') => self.scan_ui = Some(ScanUi::Off),
            Arg::Short(b'1') => self.scan_ui = Some(ScanUi::Line),
            Arg::Short(b'2') => self.scan_ui = Some(ScanUi::Full),
            Arg::Long(b"si") => self.si = true,
            Arg::Long(b"no-si") => self.si = false,
            Arg::Short(b'L') | Arg::Long(b"follow-symlinks") => self.follow_symlinks = true,
            Arg::Long(b"no-follow-symlinks") => self.follow_symlinks = false,
            Arg::Long(b"exclude") => self.exclude.add(args.value()?),
            Arg::Short(b'X') | Arg::Long(b"exclude-from") => {
                let path = args.value()?;
                read_exclude_file(path, &mut self.exclude).map_err(|description| {
                    [
                        b"Error reading excludes from ",
                        path,
                        b": ",
                        description.as_bytes(),
                        b".\n",
                    ]
                    .concat()
                })?;
            }
            Arg::Long(b"exclude-caches") => self.exclude_caches = true,
            Arg::Long(b"include-caches") => self.exclude_caches = false,
            Arg::Long(b"exclude-kernfs") => self.exclude_kernfs = true,
            Arg::Long(b"include-kernfs") => self.exclude_kernfs = false,
            Arg::Short(b'c') | Arg::Long(b"compress") => self.compress = true,
            Arg::Long(b"no-compress") => self.compress = false,
            Arg::Long(b"compress-level") => {
                let value = args.value()?;
                self.compress_level = parse_number(value, u8::MAX.into())
                    .and_then(|level| u8::try_from(level).ok())
                    .filter(|level| (1..=20).contains(level))
                    .ok_or_else(|| invalid_number(b"for --compress-level", value))?;
            }
            Arg::Long(b"export-block-size") => {
                let value = args.value()?;
                // ncdu parses the value into 14 bits.
                let kibibytes = parse_number(value, (1 << 14) - 1)
                    .and_then(|kibibytes| usize::try_from(kibibytes).ok())
                    .filter(|kibibytes| (4..=16000).contains(kibibytes))
                    .ok_or_else(|| invalid_number(b"for --export-block-size", value))?;
                self.export_block_size = Some(kibibytes * 1024);
            }
            Arg::Long(b"confirm-quit") => self.confirm_quit = true,
            Arg::Long(b"no-confirm-quit") => self.confirm_quit = false,
            Arg::Long(b"confirm-delete") => self.confirm_delete = true,
            Arg::Long(b"no-confirm-delete") => self.confirm_delete = false,
            Arg::Long(b"delete-command") => self.delete_command = args.value()?.to_vec(),
            Arg::Long(b"color") => {
                self.color = match args.value()? {
                    b"off" => Color::Off,
                    b"dark" => Color::Dark,
                    b"dark-bg" => Color::DarkBackground,
                    value => return Err(unknown_value(b"--color", value)),
                };
            }
            Arg::Short(b't') | Arg::Long(b"threads") => {
                let value = args.value()?;
                self.threads = parse_number(value, u8::MAX.into())
                    .and_then(|threads| usize::try_from(threads).ok())
                    .ok_or_else(|| invalid_number(b"of --threads", value))?;
            }
            Arg::Short(_) | Arg::Long(_) | Arg::Value(_) => return Err(OptionError::Unknown),
        }
        Ok(())
    }
}

fn unknown_value(option: &[u8], value: &[u8]) -> OptionError {
    OptionError::Invalid([b"Unknown ", option, b" option: ", value, b".\n"].concat())
}

fn invalid_number(option: &[u8], value: &[u8]) -> OptionError {
    OptionError::Invalid([b"Invalid number ", option, b": ", value, b".\n"].concat())
}

/// Parse a decimal number of at most `max` the way Zig's `std.fmt.parseInt` parses an unsigned
/// integer: an optional sign, then digits that may be separated by underscores. A negative number
/// is valid only if it is zero.
fn parse_number(value: &[u8], max: u64) -> Option<u64> {
    let (negative, digits) = match value {
        [b'-', digits @ ..] => (true, digits),
        [b'+', digits @ ..] => (false, digits),
        digits => (false, digits),
    };
    if digits.first().is_none_or(|&byte| byte == b'_') || digits.last() == Some(&b'_') {
        return None;
    }
    let mut number: u64 = 0;
    for &byte in digits {
        if byte == b'_' {
            continue;
        }
        let digit = char::from(byte).to_digit(10)?;
        number = number.checked_mul(10)?.checked_add(digit.into())?;
        if number > max {
            return None;
        }
    }
    (!negative || number == 0).then_some(number)
}

/// The size of the buffer that ncdu reads a line of an exclude file into, newline included.
const LINE_BUFFER: usize = 4096;

/// Add the patterns in the file at `path`, one per line, to `exclude`. Empty lines are skipped.
/// Fails with ncdu's description of the error, also for a line that does not fit into the line
/// buffer of ncdu.
fn read_exclude_file(path: &[u8], exclude: &mut ExcludePatterns) -> Result<(), &'static str> {
    let file = File::open(OsStr::from_bytes(path)).map_err(|error| open_error(&error))?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        // Zig's reader reports any read error as `ReadFailed`.
        (&mut reader)
            .take(LINE_BUFFER as u64)
            .read_until(b'\n', &mut line)
            .map_err(|_| "ReadFailed")?;
        if line.is_empty() {
            return Ok(());
        }
        if line.pop_if(|byte| *byte == b'\n').is_none() && line.len() == LINE_BUFFER {
            return Err("StreamTooLong");
        }
        exclude.add(&line);
    }
}
