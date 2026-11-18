//! The `duit` command.

use std::env;
use std::ffi::{CStr, CString, OsStr};
use std::fs::{self, File};
use std::io::{self, IsTerminal, Read, Write};
use std::num::NonZero;
use std::os::fd::AsFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use duit::{
    Arg, Args, Config, OptionError, open_error, read_error, seek_error, stat_error, strerror,
    write_error,
};
use duit_format::{
    BINARY_SIGNATURE, BinaryExport, BinaryExportError, BinaryImportError, BinaryOpenError,
    BinaryReader, ImportError, JsonExport, JsonExportError, import,
};
use duit_model::{MemorySink, Sink, Tree, TreeError};
use duit_scan::{ScanError, ScanOptions, scan};
use duit_tui::{Browser, BrowserOptions, InitError, Input, Loading, Ui};

/// The terminal sequence that clears from the cursor to the end of the screen. ncdu writes it to
/// standard error before an error message and at the end of a scan, to clear a progress line.
const CLEAR_TO_END: &[u8] = b"\x1b[J";

const HELP: &str = "\
duit <options> <directory>

Mode selection:
  -h, --help                 This help message
  -v, -V, --version          Print version
  -f FILE                    Import scanned directory from FILE
  -o FILE                    Export scanned directory to FILE in JSON format
  -O FILE                    Export scanned directory to FILE in binary format
  -e, --extended             Enable extended information
  --ignore-config            Don't load config files

Scan options:
  -x, --one-file-system      Stay on the same filesystem
  --exclude PATTERN          Exclude files that match PATTERN
  -X, --exclude-from FILE    Exclude files that match any pattern in FILE
  --exclude-caches           Exclude directories containing CACHEDIR.TAG
  -L, --follow-symlinks      Follow symbolic links (excluding directories)
  --exclude-kernfs           Exclude Linux pseudo filesystems (procfs,sysfs,cgroup,...)
  -t NUM                     Scan with NUM threads

Export options:
  -c, --compress             Use Zstandard compression with `-o`
  --compress-level NUM       Set compression level
  --export-block-size KIB    Set export block size with `-O`

Interface options:
  -0, -1, -2                 UI to use when scanning (0=none,2=full ncurses)
  -q, --slow-ui-updates      \"Quiet\" mode, refresh interval 2 seconds
  --enable-shell             Enable/disable shell spawning feature
  --enable-delete            Enable/disable file deletion feature
  --enable-refresh           Enable/disable directory refresh feature
  -r                         Read only (--disable-delete)
  -rr                        Read only++ (--disable-delete & --disable-shell)
  --si                       Use base 10 (SI) prefixes instead of base 2
  --apparent-size            Show apparent size instead of disk usage by default
  --hide-hidden              Hide \"hidden\" or excluded files by default
  --show-itemcount           Show item count column by default
  --show-mtime               Show mtime column by default (requires `-e`)
  --show-graph               Show graph column by default
  --show-percent             Show percent column by default
  --graph-style STYLE        hash / half-block / eighth-block
  --shared-column            off / shared / unique
  --sort COLUMN-(asc/desc)   disk-usage / name / apparent-size / itemcount / mtime
  --enable-natsort           Use natural order when sorting by name
  --group-directories-first  Sort directories before files
  --confirm-quit             Ask confirmation before quitting duit
  --no-confirm-delete        Don't ask confirmation before deletion
  --delete-command CMD       Command to run for file deletion
  --color SCHEME             off / dark / dark-bg

Refer to `man ncdu` for more information.
";

fn main() -> ExitCode {
    set_locale();
    // ncdu looks for `--ignore-config` in every argument, the program name included, before it
    // parses them.
    let read_config = env::args_os().all(|argument| argument != "--ignore-config");
    let arguments: Vec<Vec<u8>> = env::args_os().skip(1).map(OsStringExt::into_vec).collect();
    match run(&arguments, read_config) {
        Ok(code) => code,
        Err(message) => {
            let _ = io::stderr().write_all(&[CLEAR_TO_END, &message].concat());
            ExitCode::FAILURE
        }
    }
}

/// Use the locale of the environment, which `fnmatch` needs to match multibyte characters in
/// exclude patterns.
#[expect(unsafe_code)]
fn set_locale() {
    // SAFETY: The argument is a valid C string, and no other thread exists yet that could use the
    // locale at the same time.
    unsafe {
        libc::setlocale(libc::LC_ALL, c"".as_ptr());
    }
}

/// The separator between groups of thousands in the locale, if it has one.
#[expect(unsafe_code)]
fn thousands_separator() -> Option<String> {
    // SAFETY: `localeconv` returns null or a pointer to a structure that only `setlocale` and
    // `localeconv` change, and no other thread calls them. Its `thousands_sep` is null or a C
    // string, which is copied before the pointer is dropped.
    let separator = unsafe {
        let locale = libc::localeconv();
        if locale.is_null() || (*locale).thousands_sep.is_null() {
            return None;
        }
        CStr::from_ptr((*locale).thousands_sep).to_bytes().to_vec()
    };
    (!separator.is_empty()).then(|| String::from_utf8_lossy(&separator).into_owned())
}

/// Write `output` to standard output, ignoring errors as ncdu does for the help and the version.
fn print(output: &[u8]) {
    let _ = io::stdout().write_all(output);
}

/// Apply the config files if `read_config`, then parse `arguments`, which follow the program
/// name, and run the mode they select. An error is the message to print before exiting with
/// status 1.
fn run(arguments: &[Vec<u8>], read_config: bool) -> Result<ExitCode, Vec<u8>> {
    let mut config = Config::default();
    if read_config {
        config.read_config_files()?;
    }
    let mut scan_directory = None;
    let mut import_file = None;
    let mut export_json = None;
    let mut export_binary = None;
    let mut quit_after_scan = false;
    let mut args = Args::new(arguments);
    while let Some(argument) = args.next_argument()? {
        match argument {
            Arg::Value(value) => {
                if scan_directory.is_some() {
                    return Err(b"Multiple directories given, see duit -h for help.\n".to_vec());
                }
                scan_directory = Some(value);
            }
            Arg::Short(b'h' | b'?') | Arg::Long(b"help") => {
                print(HELP.as_bytes());
                return Ok(ExitCode::SUCCESS);
            }
            Arg::Short(b'v' | b'V') | Arg::Long(b"version") => {
                print(format!("duit {}\n", env!("CARGO_PKG_VERSION")).as_bytes());
                return Ok(ExitCode::SUCCESS);
            }
            Arg::Short(b'o') if export_json.is_some() || export_binary.is_some() => {
                return Err(b"The -o flag can only be given once.\n".to_vec());
            }
            Arg::Short(b'o') => export_json = Some(args.value()?),
            Arg::Short(b'O') if export_json.is_some() || export_binary.is_some() => {
                return Err(b"The -O flag can only be given once.\n".to_vec());
            }
            Arg::Short(b'O') => export_binary = Some(args.value()?),
            Arg::Short(b'f') if import_file.is_some() => {
                return Err(b"The -f flag can only be given once.\n".to_vec());
            }
            Arg::Short(b'f') => import_file = Some(args.value()?),
            Arg::Long(b"ignore-config") => {}
            // An undocumented option of ncdu, to benchmark a scan or an import.
            Arg::Long(b"quit-after-scan") => quit_after_scan = true,
            option @ (Arg::Short(_) | Arg::Long(_)) => match config.apply(&mut args, option, false)
            {
                Ok(()) => {}
                Err(OptionError::Unknown) => {
                    return Err(
                        [b"Unrecognized option '", &option.to_bytes()[..], b"'.\n"].concat()
                    );
                }
                Err(OptionError::Invalid(message)) => return Err(message),
            },
        }
    }

    if config.threads == 0 {
        config.threads = thread::available_parallelism().map_or(1, NonZero::get);
    }
    if cfg!(not(target_os = "linux")) && config.exclude_kernfs {
        return Err(b"The --exclude-kernfs flag is currently only supported on Linux.\n".to_vec());
    }
    if !io::stdin().is_terminal()
        && import_file.is_none()
        && export_json.is_none()
        && export_binary.is_none()
        && !quit_after_scan
    {
        return Err(
            b"Standard input is not a TTY. Did you mean to import a file using '-f -'?\n".to_vec(),
        );
    }

    if import_file.is_none() && config.threads > 1 {
        return Err(b"A scan with more than one thread is not supported yet.\n".to_vec());
    }

    let Some(export_path) = export_json.or(export_binary) else {
        let import_input = import_file.map(open_import).transpose()?;
        let imported = import_input.is_some();
        let mut options = config.browser.clone();
        options.imported = imported;
        options.can_shell = config.can_shell.unwrap_or(!imported);
        options.can_delete = config.can_delete.unwrap_or(!imported);
        options.can_refresh = config.can_refresh.unwrap_or(!imported);
        options.extended = config.extended;
        if let Some(separator) = thousands_separator() {
            options.thousands_separator = separator;
        }
        if let Some(ImportInput::Binary(reader)) = import_input {
            // ncdu opens a binary export without reading it, because the browser reads it from
            // the file, and writes the clear as it returns or as `ui.init` takes over the
            // terminal.
            let _ = io::stderr().write_all(CLEAR_TO_END);
            if quit_after_scan {
                return Ok(ExitCode::SUCCESS);
            }
            return browse(options, config.update_delay, |options, loading| {
                Browser::open(reader, options, loading)
            });
        }
        let memory = MemorySink::new(config.extended);
        // `fill` ends with the clear that ncdu writes when it returns after the scan, or when
        // `ui.init` takes over the terminal.
        let tree = fill(memory, import_input, scan_directory, &config)?;
        if quit_after_scan {
            return Ok(ExitCode::SUCCESS);
        }
        // A scan or an import that succeeds has created the root.
        let Some(root) = tree.root() else {
            return Ok(ExitCode::SUCCESS);
        };
        return browse(options, config.update_delay, |options, loading| {
            Ok(Some((Browser::new(&tree, root, options, loading), tree)))
        });
    };

    let out: Box<dyn Write> = if export_path == b"-" {
        Box::new(io::stdout().lock())
    } else {
        Box::new(
            File::create(OsStr::from_bytes(export_path)).map_err(|error| {
                [
                    b"Error opening export file: ",
                    open_error(&error).as_bytes(),
                    b".\n",
                ]
                .concat()
            })?,
        )
    };
    if export_binary.is_some() {
        // ncdu writes the signature before it reads the import.
        let export = BinaryExport::new(
            out,
            config.extended,
            config.compress_level,
            config.export_block_size,
        )
        .map_err(|error| writing(&error))?;
        let import_input = import_file.map(open_import).transpose()?;
        return fill(export, import_input, scan_directory, &config).map(|()| ExitCode::SUCCESS);
    }
    // As in ncdu, the export file is created before the import is read.
    let import_input = import_file.map(open_import).transpose()?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs());
    let export = JsonExport::new(
        out,
        timestamp,
        config.extended,
        config.compress.then_some(config.compress_level),
    )
    .map_err(|error| writing(&error))?;
    fill(export, import_input, scan_directory, &config).map(|()| ExitCode::SUCCESS)
}

/// Run the browser that `open` opens, with the tree it reads, until the user quits, the loop at
/// the end of `main` in `main.zig` with `handleEvent`. Like ncdu, it handles every key that is
/// waiting before it draws again. As in ncdu, the browser is opened once the terminal is, so that
/// the root of a binary export is read then, and `open` gives `None` for a root that is not a
/// directory. While the browser lists a directory, [`Load`] draws the loading screen after
/// `update_delay`.
fn browse(
    mut options: BrowserOptions,
    update_delay: Duration,
    open: impl FnOnce(
        &BrowserOptions,
        &mut dyn FnMut(&Loading<'_>),
    ) -> Result<Option<(Browser, Tree)>, BinaryImportError>,
) -> Result<ExitCode, Vec<u8>> {
    let mut ui = Ui::init(!io::stdin().is_terminal()).map_err(|error| match error {
        InitError::OpenTty(error) => {
            format!("Error opening /dev/tty: {}.\n", strerror(&error)).into_bytes()
        }
        InitError::Terminal(_) => b"Error initializing the terminal.\n".to_vec(),
    })?;
    let mut load = Load::new(&mut ui, update_delay);
    let opened = open(&options, &mut |loading| load.report(loading));
    if let Some(exit) = load.finish() {
        ui.deinit();
        return exit;
    }
    let (mut browser, mut tree) = match opened {
        Ok(Some(opened)) => opened,
        Ok(None) => {
            ui.deinit();
            return Err(b"Invalid import\n".to_vec());
        }
        Err(error) => {
            ui.deinit();
            return Err(binary_import_error(error));
        }
    };
    let mut height = 0;
    let mut draw = true;
    let mut block = true;
    loop {
        if draw {
            ui.draw(|frame| {
                height = frame.area().height;
                browser.draw(frame, &tree, &options);
            });
            draw = false;
        }
        match ui.getch(block) {
            Ok(Some(Input::Key(key))) => {
                let mut load = Load::new(&mut ui, update_delay);
                let quit =
                    browser.key_input(&mut tree, &mut options, key, height, &mut |loading| {
                        load.report(loading);
                    });
                if let Some(exit) = load.finish() {
                    ui.deinit();
                    return exit;
                }
                match quit {
                    Ok(true) => {
                        ui.deinit();
                        return Ok(ExitCode::SUCCESS);
                    }
                    Ok(false) => block = false,
                    Err(error) => {
                        ui.deinit();
                        return Err(binary_import_error(error));
                    }
                }
            }
            Ok(Some(Input::Resize)) => draw = true,
            Ok(Some(Input::Interrupt)) => {
                ui.deinit();
                return Ok(ExitCode::FAILURE);
            }
            Ok(None) => {
                draw = true;
                block = true;
            }
            Err(error) => {
                ui.deinit();
                return Err(input_error(&error));
            }
        }
    }
}

/// The message that ncdu exits with when it cannot read the keys.
fn input_error(error: &io::Error) -> Vec<u8> {
    format!(
        "Error reading keyboard input, assuming TTY has been lost.\n\
         (Potentially nonsensical error message: {})\n",
        strerror(error)
    )
    .into_bytes()
}

/// A directory that the browser lists, as `loadDir` in `browser.zig` calls `handleEvent` in
/// `main.zig` while it lists one: once `update_delay` has passed since the listing started or
/// since the loading screen was drawn, the loading screen is drawn again, and the keys that wait
/// are dropped, as the browser ignores keys while it lists.
struct Load<'a> {
    ui: &'a mut Ui,
    update_delay: Duration,
    drawn: Instant,
    /// Whether the browser reported a directory that it lists, which it does once it has listed 2
    /// entries.
    listed: bool,
    /// How duit ends once the listing is done, after an error reading the keys.
    exit: Option<Result<ExitCode, Vec<u8>>>,
}

impl<'a> Load<'a> {
    fn new(ui: &'a mut Ui, update_delay: Duration) -> Self {
        Self {
            ui,
            update_delay,
            drawn: Instant::now(),
            listed: false,
            exit: None,
        }
    }

    /// Handle the screen `loading` that the browser reports.
    fn report(&mut self, loading: &Loading<'_>) {
        self.listed = true;
        if self.drawn.elapsed() > self.update_delay {
            self.ui.draw(|frame| loading.draw(frame));
            self.drawn = Instant::now();
            self.drop_keys();
        }
    }

    /// Read the input that waits and drop it. Ctrl-C ends duit at the next check of the keys
    /// during the listing, where ncurses raises `SIGINT` at once. A resize needs no redraw, because
    /// the next frame is drawn at the new size.
    fn drop_keys(&mut self) {
        while self.exit.is_none() {
            match self.ui.getch(false) {
                Ok(Some(Input::Key(_) | Input::Resize)) => {}
                Ok(Some(Input::Interrupt)) => self.ui.interrupt(),
                Ok(None) => break,
                Err(error) => self.exit = Some(Err(input_error(&error))),
            }
        }
    }

    /// End the listing, dropping the keys that wait if a directory was listed, and return how
    /// duit ends, if it does.
    fn finish(mut self) -> Option<Result<ExitCode, Vec<u8>>> {
        if self.listed {
            self.drop_keys();
        }
        self.exit
    }
}

/// An export or the in-memory tree, which an import or a scan writes into.
trait Consumer: Sink {
    /// What the consumer gives when it is complete.
    type Output;

    /// Write the end of the export and flush it, or complete the tree.
    fn finish(self) -> io::Result<Self::Output>;

    /// Drop the export without writing what is still buffered, as ncdu does when it stops on an
    /// error.
    fn discard(self);
}

impl<W: Write> Consumer for JsonExport<W> {
    type Output = ();

    fn finish(self) -> io::Result<()> {
        Self::finish(self).map(drop)
    }

    fn discard(self) {
        Self::discard(self);
    }
}

impl<W: Write> Consumer for BinaryExport<W> {
    type Output = ();

    fn finish(self) -> io::Result<()> {
        Self::finish(self).map(drop)
    }

    /// The blocks that are full are written already, and the current one is dropped.
    fn discard(self) {}
}

impl Consumer for MemorySink {
    type Output = Tree;

    fn finish(self) -> io::Result<Tree> {
        Ok(Self::finish(self))
    }

    fn discard(self) {}
}

/// An import file, opened by [`open_import`].
enum ImportInput {
    /// A JSON export, whose first 8 bytes are read.
    Json(File, [u8; 8]),
    /// A binary export, whose index block is read.
    Binary(BinaryReader<File>),
}

/// Write the import of `import_input`, or else the scan of `scan_directory`, into `consumer`.
fn fill<C: Consumer>(
    mut consumer: C,
    import_input: Option<ImportInput>,
    scan_directory: Option<&[u8]>,
    config: &Config,
) -> Result<C::Output, Vec<u8>> {
    if let Some(import_input) = import_input {
        let failure = match import_input {
            ImportInput::Json(input, head) => match import(&mut consumer, input, &head) {
                Ok(()) => None,
                Err(ImportError::Input {
                    line,
                    byte,
                    message,
                }) => Some(
                    format!("Error importing file on line {line}:{byte}: {message}.\n")
                        .into_bytes(),
                ),
                Err(ImportError::Sink(error)) => Some(writing(&error)),
            },
            ImportInput::Binary(mut reader) => {
                reader.import(&mut consumer).err().map(binary_import_error)
            }
        };
        // As in ncdu, which stops without flushing its buffer, a failed export writes nothing more.
        if let Some(message) = failure {
            consumer.discard();
            return Err(message);
        }
        let output = consumer.finish().map_err(|error| writing(&error))?;
        let _ = io::stderr().write_all(CLEAR_TO_END);
        return Ok(output);
    }

    // ncdu scans the absolute path if it can resolve one.
    let directory = scan_directory.unwrap_or(b".");
    let path = fs::canonicalize(OsStr::from_bytes(directory)).map_or_else(
        |_| directory.to_vec(),
        |path| path.into_os_string().into_vec(),
    );
    // Arguments are C strings and cannot hold a NUL byte.
    let path = CString::new(path).map_err(|_| opening_directory("Unexpected"))?;
    let options = ScanOptions {
        same_filesystem: config.same_filesystem,
        follow_symlinks: config.follow_symlinks,
        exclude_kernfs: config.exclude_kernfs,
        exclude_caches: config.exclude_caches,
    };
    // A failure at the root still ends the export, as in ncdu.
    let root_error = match scan(&mut consumer, &path, options, &config.exclude) {
        Ok(()) => None,
        Err(ScanError::Stat(error)) => Some(stat_error(&error)),
        Err(ScanError::Open(error)) => Some(open_error(&error)),
        Err(ScanError::Sink(error)) => return Err(writing(&error)),
    };
    let output = consumer.finish().map_err(|error| writing(&error))?;
    if let Some(description) = root_error {
        return Err(opening_directory(description));
    }
    let _ = io::stderr().write_all(CLEAR_TO_END);
    Ok(output)
}

/// Open the import file at `path`, or standard input for `-`, and read its first 8 bytes, which
/// tell the formats apart. A binary export is read with random access, from its index block at the
/// end, so it must be a file that can seek.
fn open_import(path: &[u8]) -> Result<ImportInput, Vec<u8>> {
    let reading = |description: &str| {
        [
            b"Error reading file '",
            path,
            b"': ",
            description.as_bytes(),
            b".\n",
        ]
        .concat()
    };
    let mut file = if path == b"-" {
        // A read of the duplicated descriptor fails as a read of standard input would.
        io::stdin()
            .as_fd()
            .try_clone_to_owned()
            .map(File::from)
            .map_err(|error| reading(read_error(&error)))?
    } else {
        File::open(OsStr::from_bytes(path)).map_err(|error| reading(open_error(&error)))?
    };
    let read_failed = |error: &io::Error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            reading("Unexpected end of file")
        } else {
            reading(read_error(error))
        }
    };
    let mut head = [0; 8];
    file.read_exact(&mut head)
        .map_err(|error| read_failed(&error))?;
    if &head != BINARY_SIGNATURE {
        return Ok(ImportInput::Json(file, head));
    }
    BinaryReader::open(file)
        .map(ImportInput::Binary)
        .map_err(|error| match error {
            BinaryOpenError::Seek(error) => reading(seek_error(&error)),
            BinaryOpenError::Read(error) => read_failed(&error),
            BinaryOpenError::Corrupt => b"Error reading from file\n".to_vec(),
        })
}

/// The message that ncdu exits with when it cannot read a binary export.
fn binary_import_error(error: BinaryImportError) -> Vec<u8> {
    match error {
        BinaryImportError::Corrupt { itemref } => {
            format!("Error reading item {itemref:x} from file\n").into_bytes()
        }
        BinaryImportError::Decompress {
            block,
            expected,
            result,
        } => format!("Error decompressing block {block} (expected {expected} got {result})\n")
            .into_bytes(),
        BinaryImportError::Read(error) => {
            format!("Error reading from file: {}\n", read_error(&error)).into_bytes()
        }
        BinaryImportError::Sink(error) => writing(&error),
    }
}

fn opening_directory(description: &str) -> Vec<u8> {
    format!("Error opening directory: {description}.\n").into_bytes()
}

fn writing(error: &io::Error) -> Vec<u8> {
    let inner = error.get_ref();
    if let Some(error) = inner.and_then(|error| error.downcast_ref::<TreeError>()) {
        return match error {
            TreeError::TooManyDevices => {
                b"Maximum number of device identifiers exceeded.\n".to_vec()
            }
        };
    }
    if let Some(error) = inner.and_then(|error| error.downcast_ref::<JsonExportError>()) {
        return match error {
            JsonExportError::PathTooLong => b"Error writing JSON export: path too long.\n".to_vec(),
        };
    }
    match inner.and_then(|error| error.downcast_ref::<BinaryExportError>()) {
        Some(BinaryExportError::PathTooLong) => b"Error writing data: path too long.\n".to_vec(),
        Some(BinaryExportError::FileTooLarge) => {
            b"Export data file has grown too large, please report a bug.\n".to_vec()
        }
        Some(BinaryExportError::TooManyBlocks) => {
            b"Too many data blocks, please report a bug.\n".to_vec()
        }
        None => format!("Error writing to file: {}.\n", write_error(error)).into_bytes(),
    }
}
