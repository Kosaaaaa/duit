use std::env;
use std::ffi::{CStr, CString, OsStr};
use std::fs::{self, File};
use std::io::{self, IsTerminal, Read, Write};
use std::mem;
use std::num::NonZero;
use std::os::fd::AsFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::process::{Command, ExitCode};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use duit::{
    Arg, Args, Config, OptionError, ScanUi, open_error, read_error, seek_error, spawn_error,
    stat_error, strerror, write_error,
};
use duit_format::{
    BINARY_SIGNATURE, BinaryExport, BinaryExportError, BinaryImportError, BinaryOpenError,
    BinaryReader, ImportError, JsonExport, JsonExportError, import,
};
use duit_model::{
    DirectoryId, MemorySink, Producer, Progress, ProgressSink, Sink, Tree, TreeError, replay,
};
use duit_scan::{ScanError, ScanOptions, scan, stat_at};
use duit_tui::{
    Action, Browser, BrowserOptions, CLEAR_TO_END, Console, Deletion, InitError, Input, Loading,
    ScanKey, ScanScreen, ScanState, Ui,
};
use rustix::fs::CWD;

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

Refer to `man duit` for more information.
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

    let scan_ui = config
        .scan_ui
        .unwrap_or(match export_json.or(export_binary) {
            Some(path) if path != b"-" && io::stdout().is_terminal() => ScanUi::Line,
            Some(_) => ScanUi::Off,
            None => ScanUi::Full,
        });
    let open_tty = !io::stdin().is_terminal()
        || export_json
            .or(export_binary)
            .is_some_and(|path| path == b"-");
    if let Some(separator) = thousands_separator() {
        config.browser.thousands_separator = separator;
    }
    let mut progress = ScanProgress::new(scan_ui, config.update_delay, &config.browser, open_tty);

    let Some(export_path) = export_json.or(export_binary) else {
        let import_input = import_file.map(open_import).transpose()?;
        let imported = import_input.is_some();
        let mut options = config.browser.clone();
        options.imported = imported;
        options.can_shell = config.can_shell.unwrap_or(!imported);
        options.can_delete = config.can_delete.unwrap_or(!imported);
        options.can_refresh = config.can_refresh.unwrap_or(!imported);
        options.extended = config.extended;
        if let Some(ImportInput::Binary(reader)) = import_input {
            // ncdu opens a binary export without reading it, because the browser reads it from
            // the file, and writes the clear as it returns or as `ui.init` takes over the
            // terminal.
            let _ = io::stderr().write_all(CLEAR_TO_END);
            if quit_after_scan {
                return Ok(ExitCode::SUCCESS);
            }
            let screen = ScanScreen::new(config.update_delay);
            return browse(None, screen, options, &config, |options, loading| {
                Browser::open(reader, options, loading)
            });
        }
        let memory = MemorySink::new(config.extended);
        let tree = match fill(memory, import_input, scan_directory, &config, &mut progress) {
            Ok(tree) => tree,
            Err(exit) => return exit,
        };
        if quit_after_scan {
            return Ok(ExitCode::SUCCESS);
        }
        let Some(root) = tree.root() else {
            return Ok(ExitCode::SUCCESS);
        };
        let screen = mem::replace(&mut progress.screen, ScanScreen::new(config.update_delay));
        return browse(
            progress.ui.take(),
            screen,
            options,
            &config,
            |options, loading| Ok(Some((Browser::new(&tree, root, options, loading), tree))),
        );
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
        return fill(export, import_input, scan_directory, &config, &mut progress)
            .map_or_else(|exit| exit, |()| Ok(ExitCode::SUCCESS));
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
    let exported = if import_input.is_none() && config.threads > 1 {
        export_from_memory(export, scan_directory, &config, &mut progress)
    } else {
        fill(export, import_input, scan_directory, &config, &mut progress)
    };
    exported.map_or_else(|exit| exit, |()| Ok(ExitCode::SUCCESS))
}

fn browse(
    ui: Option<Ui>,
    mut screen: ScanScreen,
    mut options: BrowserOptions,
    config: &Config,
    open: impl FnOnce(
        &BrowserOptions,
        &mut dyn FnMut(&Loading<'_>),
    ) -> Result<Option<(Browser, Tree)>, BinaryImportError>,
) -> Result<ExitCode, Vec<u8>> {
    let mut ui = match ui {
        Some(ui) => ui,
        None => Ui::init(!io::stdin().is_terminal()).map_err(init_error)?,
    };
    let update_delay = config.update_delay;
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
    let mut drawn = Instant::now();
    loop {
        if draw {
            ui.draw(|frame| {
                height = frame.area().height;
                browser.draw(frame, &tree, &options);
            });
            drawn = Instant::now();
            draw = false;
        }
        match ui.getch(block) {
            Ok(Some(Input::Key(key))) => {
                let mut load = Load::new(&mut ui, update_delay);
                let action =
                    browser.key_input(&mut tree, &mut options, key, height, &mut |loading| {
                        load.report(loading);
                    });
                if let Some(exit) = load.finish() {
                    ui.deinit();
                    return exit;
                }
                match action {
                    Ok(Action::Browse) => block = false,
                    Ok(Action::Quit) => {
                        ui.deinit();
                        return Ok(ExitCode::SUCCESS);
                    }
                    Ok(Action::Shell) => {
                        let shell = env::var_os("NCDU_SHELL")
                            .or_else(|| env::var_os("SHELL"))
                            .unwrap_or_else(|| "/bin/sh".into());
                        let mut command = Command::new(shell);
                        command.current_dir(OsStr::from_bytes(browser.path()));
                        ui.run_command(&mut command, false).map_err(init_error)?;
                        draw = true;
                        block = true;
                    }
                    Ok(Action::Refresh) => {
                        let directory = browser.directory();
                        ui = match refresh(
                            ui,
                            &mut screen,
                            &mut tree,
                            directory,
                            &options,
                            config,
                            drawn,
                        )
                        .and_then(|ui| reload(ui, &mut browser, &tree, &options, update_delay))
                        {
                            Ok(ui) => ui,
                            Err(exit) => return exit,
                        };
                        draw = true;
                        block = true;
                    }
                    Ok(Action::Delete) if !options.delete_command.is_empty() => {
                        let mut failed = None;
                        let deleted =
                            browser.delete_command(&mut tree, &options, &mut |command, path| {
                                if let Err(error) = ui.run_command(command, true) {
                                    failed = Some(error);
                                }
                                stat_at(CWD, path, false).ok().map(|(stat, _)| stat)
                            });
                        if let Some(error) = failed {
                            return Err(init_error(error));
                        }
                        let directory = match deleted {
                            Ok(directory) => directory,
                            Err(error) => {
                                ui.deinit();
                                return Err(writing(&error.into()));
                            }
                        };
                        if let Some(directory) = directory {
                            ui = match refresh(
                                ui,
                                &mut screen,
                                &mut tree,
                                directory,
                                &options,
                                config,
                                drawn,
                            ) {
                                Ok(ui) => ui,
                                Err(exit) => return exit,
                            };
                        }
                        ui = match reload(ui, &mut browser, &tree, &options, update_delay) {
                            Ok(ui) => ui,
                            Err(exit) => return exit,
                        };
                        draw = true;
                        block = true;
                    }
                    Ok(Action::Delete) => {
                        let mut delete = Delete {
                            ui: &mut ui,
                            options: &options,
                            update_delay,
                            drawn,
                            exit: None,
                        };
                        browser.delete(&mut tree, &mut |deletion, tree, block| {
                            delete.handle_event(deletion, tree, block);
                        });
                        if let Some(exit) = delete.exit {
                            ui.deinit();
                            return exit;
                        }
                        ui = match reload(ui, &mut browser, &tree, &options, update_delay) {
                            Ok(ui) => ui,
                            Err(exit) => return exit,
                        };
                        draw = true;
                        block = true;
                    }
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

fn reload(
    mut ui: Ui,
    browser: &mut Browser,
    tree: &Tree,
    options: &BrowserOptions,
    update_delay: Duration,
) -> Result<Ui, Result<ExitCode, Vec<u8>>> {
    let mut load = Load::new(&mut ui, update_delay);
    browser.load_directory(tree, options, &mut |loading| load.report(loading));
    if let Some(exit) = load.finish() {
        ui.deinit();
        return Err(exit);
    }
    Ok(ui)
}

fn refresh(
    ui: Ui,
    screen: &mut ScanScreen,
    tree: &mut Tree,
    directory: DirectoryId,
    options: &BrowserOptions,
    config: &Config,
    drawn: Instant,
) -> Result<Ui, Result<ExitCode, Vec<u8>>> {
    let mut path = Vec::new();
    tree.format_path(directory, true, &mut path);
    path.push(0);
    let path = CStr::from_bytes_until_nul(&path).unwrap_or_default();
    let mut progress = ScanProgress::new(ScanUi::Full, config.update_delay, options, false);
    progress.ui = Some(ui);
    progress.drawn = drawn;
    mem::swap(&mut progress.screen, screen);
    let memory = MemorySink::refresh(mem::take(tree), directory, config.extended);
    let mut report = |state: &Progress| progress.report(state);
    let mut sink = ProgressSink::new(memory, Producer::Scan, &mut report);
    let scanned = scan(&mut sink, path, scan_options(config), &config.exclude);
    let memory = sink.into_inner();
    if let Some(exit) = progress.exit.take() {
        return Err(exit);
    }
    match &scanned {
        Err(ScanError::Sink(error)) => return Err(Err(writing(error))),
        Err(ScanError::Spawn(error)) => return Err(Err(spawning_thread(error))),
        Ok(()) | Err(ScanError::Stat(_) | ScanError::Open(_)) => {}
    }
    *tree = progress.finish(memory)?;
    if scanned.is_err() {
        progress.wait(&ScanState::Error {
            path: path.to_bytes(),
        })?;
    }
    mem::swap(&mut progress.screen, screen);
    progress.ui.take().ok_or(Ok(ExitCode::FAILURE))
}

fn init_error(error: InitError) -> Vec<u8> {
    match error {
        InitError::OpenTty(error) => {
            format!("Error opening /dev/tty: {}.\n", strerror(&error)).into_bytes()
        }
        InitError::Terminal(_) => b"Error initializing the terminal.\n".to_vec(),
    }
}

fn input_error(error: &io::Error) -> Vec<u8> {
    format!(
        "Error reading keyboard input, assuming TTY has been lost.\n\
         (Potentially nonsensical error message: {})\n",
        strerror(error)
    )
    .into_bytes()
}

struct Load<'a> {
    ui: &'a mut Ui,
    update_delay: Duration,
    drawn: Instant,
    listed: bool,
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

    fn report(&mut self, loading: &Loading<'_>) {
        self.listed = true;
        if self.drawn.elapsed() > self.update_delay {
            self.ui.draw(|frame| loading.draw(frame));
            self.drawn = Instant::now();
            self.drop_keys();
        }
    }

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

    fn finish(mut self) -> Option<Result<ExitCode, Vec<u8>>> {
        if self.listed {
            self.drop_keys();
        }
        self.exit
    }
}

struct Delete<'a> {
    ui: &'a mut Ui,
    options: &'a BrowserOptions,
    update_delay: Duration,
    drawn: Instant,
    exit: Option<Result<ExitCode, Vec<u8>>>,
}

impl Delete<'_> {
    fn handle_event(&mut self, deletion: &mut Deletion, tree: &Tree, block: bool) {
        if !block && self.drawn.elapsed() <= self.update_delay {
            return;
        }
        self.draw(deletion, tree);
        let mut block = block;
        while deletion.is_active() {
            match self.ui.getch(block) {
                Ok(Some(Input::Key(key))) => deletion.key_input(key),
                Ok(Some(Input::Resize)) => {
                    self.draw(deletion, tree);
                    continue;
                }
                Ok(Some(Input::Interrupt)) => {
                    self.exit = Some(Ok(ExitCode::FAILURE));
                    deletion.abort();
                }
                Ok(None) => break,
                Err(error) => {
                    self.exit = Some(Err(input_error(&error)));
                    deletion.abort();
                }
            }
            block = false;
        }
    }

    fn draw(&mut self, deletion: &Deletion, tree: &Tree) {
        self.ui
            .draw(|frame| deletion.draw(frame, tree, self.options));
        self.drawn = Instant::now();
    }
}

trait Consumer: Sink {
    type Output;

    fn counted_inodes(&self) -> Option<usize> {
        None
    }

    fn finish(self, progress: &mut dyn FnMut(usize, usize)) -> io::Result<Self::Output>;

    fn discard(self);
}

impl<W: Write> Consumer for JsonExport<W> {
    type Output = ();

    fn finish(self, _: &mut dyn FnMut(usize, usize)) -> io::Result<()> {
        Self::finish(self).map(drop)
    }

    fn discard(self) {
        Self::discard(self);
    }
}

impl<W: Write> Consumer for BinaryExport<W> {
    type Output = ();

    fn finish(self, _: &mut dyn FnMut(usize, usize)) -> io::Result<()> {
        Self::finish(self).map(drop)
    }

    fn discard(self) {}
}

impl Consumer for MemorySink {
    type Output = Tree;

    fn counted_inodes(&self) -> Option<usize> {
        Some(Self::counted_inodes(self))
    }

    fn finish(self, progress: &mut dyn FnMut(usize, usize)) -> io::Result<Tree> {
        Ok(Self::finish(self, progress))
    }

    fn discard(self) {}
}

enum ImportInput {
    Json(File, [u8; 8]),
    Binary(BinaryReader<File>),
}

fn fill<C: Consumer>(
    consumer: C,
    import_input: Option<ImportInput>,
    scan_directory: Option<&[u8]>,
    config: &Config,
    progress: &mut ScanProgress,
) -> Result<C::Output, Result<ExitCode, Vec<u8>>> {
    let mut report = |state: &Progress| progress.report(state);
    if let Some(import_input) = import_input {
        let mut sink = ProgressSink::new(consumer, Producer::Import, &mut report);
        let failure = match import_input {
            ImportInput::Json(input, head) => match import(&mut sink, input, &head) {
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
                reader.import(&mut sink).err().map(binary_import_error)
            }
        };
        let consumer = sink.into_inner();
        // As in ncdu, which stops without flushing its buffer, a failed or aborted export writes
        // nothing more.
        if let Some(exit) = progress.exit.take() {
            consumer.discard();
            return Err(exit);
        }
        if let Some(message) = failure {
            consumer.discard();
            return Err(Err(message));
        }
        let output = progress.finish(consumer)?;
        progress.clear();
        return Ok(output);
    }

    let Scanned {
        consumer,
        root_error,
    } = scan_into(consumer, scan_directory, config, progress)?;
    let output = progress.finish(consumer)?;
    if let Some(description) = root_error {
        return Err(Err(opening_directory(description)));
    }
    progress.clear();
    Ok(output)
}

struct Scanned<C> {
    consumer: C,
    root_error: Option<&'static str>,
}

fn scan_into<C: Consumer>(
    consumer: C,
    scan_directory: Option<&[u8]>,
    config: &Config,
    progress: &mut ScanProgress,
) -> Result<Scanned<C>, Result<ExitCode, Vec<u8>>> {
    let directory = scan_directory.unwrap_or(b".");
    let path = fs::canonicalize(OsStr::from_bytes(directory)).map_or_else(
        |_| directory.to_vec(),
        |path| path.into_os_string().into_vec(),
    );
    let path = CString::new(path).map_err(|_| Err(opening_directory("Unexpected")))?;
    let mut report = |state: &Progress| progress.report(state);
    let mut sink = ProgressSink::new(consumer, Producer::Scan, &mut report);
    let scanned = scan(&mut sink, &path, scan_options(config), &config.exclude);
    let consumer = sink.into_inner();
    if let Some(exit) = progress.exit.take() {
        consumer.discard();
        return Err(exit);
    }
    let root_error = match scanned {
        Ok(()) => None,
        Err(ScanError::Stat(error)) => Some(stat_error(&error)),
        Err(ScanError::Open(error)) => Some(open_error(&error)),
        // As `ui.die` in ncdu, which writes nothing more.
        Err(ScanError::Spawn(error)) => {
            consumer.discard();
            return Err(Err(spawning_thread(&error)));
        }
        Err(ScanError::Sink(error)) => return Err(Err(writing(&error))),
    };
    Ok(Scanned {
        consumer,
        root_error,
    })
}

fn export_from_memory<W: Write>(
    export: JsonExport<W>,
    scan_directory: Option<&[u8]>,
    config: &Config,
    progress: &mut ScanProgress,
) -> Result<(), Result<ExitCode, Vec<u8>>> {
    let memory = MemorySink::without_stats(config.extended);
    let Scanned {
        consumer: memory,
        root_error,
    } = match scan_into(memory, scan_directory, config, progress) {
        Ok(scanned) => scanned,
        Err(exit) => {
            export.discard();
            return Err(exit);
        }
    };
    let tree = memory.finish(&mut |_, _| {});
    let mut export = export;
    if let Some(root) = tree.root() {
        let mut report = |state: &Progress| progress.report(state);
        let mut sink = ProgressSink::new(export, Producer::Replay, &mut report);
        let replayed = replay(&tree, root, &mut sink);
        export = sink.into_inner();
        if let Some(exit) = progress.exit.take() {
            export.discard();
            return Err(exit);
        }
        replayed.map_err(|error| Err(writing(&error)))?;
    }
    progress.finish(export)?;
    if let Some(description) = root_error {
        return Err(Err(opening_directory(description)));
    }
    progress.clear();
    Ok(())
}

fn scan_options(config: &Config) -> ScanOptions {
    ScanOptions {
        same_filesystem: config.same_filesystem,
        follow_symlinks: config.follow_symlinks,
        exclude_kernfs: config.exclude_kernfs,
        exclude_caches: config.exclude_caches,
        threads: config.threads,
    }
}

struct ScanProgress {
    scan_ui: ScanUi,
    update_delay: Duration,
    options: BrowserOptions,
    drawn: Instant,
    console: Console,
    screen: ScanScreen,
    open_tty: bool,
    ui: Option<Ui>,
    exit: Option<Result<ExitCode, Vec<u8>>>,
}

impl ScanProgress {
    fn new(
        scan_ui: ScanUi,
        update_delay: Duration,
        options: &BrowserOptions,
        open_tty: bool,
    ) -> Self {
        // As Zig's `File.supportsAnsiEscapeCodes` decides for standard error.
        let ansi =
            io::stderr().is_terminal() && env::var_os("TERM").is_none_or(|term| term != "dumb");
        Self {
            scan_ui,
            update_delay,
            options: options.clone(),
            drawn: Instant::now(),
            console: Console::new(ansi),
            screen: ScanScreen::new(update_delay),
            open_tty,
            ui: None,
            exit: None,
        }
    }

    fn report(&mut self, progress: &Progress) -> io::Result<()> {
        if progress.zeroing() {
            self.handle_event(&ScanState::Zeroing, true);
        } else if self.drawn.elapsed() > self.update_delay {
            let directories = progress.directories();
            let directories: Vec<Option<&[u8]>> =
                directories.iter().map(Option::as_deref).collect();
            let state = ScanState::Running {
                files: progress.files(),
                bytes: progress.bytes(),
                directories: &directories,
                last_error: progress.last_error(),
            };
            self.handle_event(&state, false);
        }
        match self.exit {
            Some(_) => Err(io::Error::other("duit ends")),
            None => Ok(()),
        }
    }

    fn handle_event(&mut self, state: &ScanState<'_>, force: bool) {
        if !force && self.drawn.elapsed() <= self.update_delay {
            return;
        }
        match self.scan_ui {
            ScanUi::Line => {
                let _ = io::stderr().write_all(&self.console.draw(state, self.options.si));
            }
            ScanUi::Full => self.draw_full(state),
            ScanUi::Off => {}
        }
        self.drawn = Instant::now();
    }

    fn draw_full(&mut self, state: &ScanState<'_>) {
        let Self {
            options,
            screen,
            open_tty,
            ui,
            exit,
            ..
        } = self;
        let ui = if let Some(ui) = ui {
            ui
        } else {
            // The clear of `ui.init`, which ncdu writes before it opens the terminal.
            let _ = io::stderr().write_all(CLEAR_TO_END);
            match Ui::init(*open_tty) {
                Ok(new) => ui.insert(new),
                Err(error) => {
                    *exit = Some(Err(init_error(error)));
                    return;
                }
            }
        };
        ui.draw(|frame| screen.draw(frame, state, options));
        while exit.is_none() {
            match ui.getch(false) {
                Ok(Some(Input::Key(key))) => match screen.key_input(state, key, options) {
                    Some(ScanKey::Quit) => *exit = Some(Ok(ExitCode::SUCCESS)),
                    Some(ScanKey::Close) | None => {}
                },
                Ok(Some(Input::Resize)) => ui.draw(|frame| screen.draw(frame, state, options)),
                Ok(Some(Input::Interrupt)) => *exit = Some(Ok(ExitCode::FAILURE)),
                Ok(None) => break,
                Err(error) => *exit = Some(Err(input_error(&error))),
            }
        }
    }

    fn wait(&mut self, state: &ScanState<'_>) -> Result<(), Result<ExitCode, Vec<u8>>> {
        let Self {
            options,
            screen,
            ui: Some(ui),
            ..
        } = self
        else {
            return Ok(());
        };
        loop {
            ui.draw(|frame| screen.draw(frame, state, options));
            match ui.getch(true) {
                Ok(Some(Input::Key(key))) => match screen.key_input(state, key, options) {
                    Some(ScanKey::Close) => return Ok(()),
                    Some(ScanKey::Quit) | None => {}
                },
                Ok(Some(Input::Resize) | None) => {}
                Ok(Some(Input::Interrupt)) => return Err(Ok(ExitCode::FAILURE)),
                Err(error) => return Err(Err(input_error(&error))),
            }
        }
    }

    fn finish<C: Consumer>(&mut self, consumer: C) -> Result<C::Output, Result<ExitCode, Vec<u8>>> {
        if let Some(total) = consumer.counted_inodes() {
            self.handle_event(&ScanState::CountingHardLinks { done: total, total }, true);
            if let Some(exit) = self.exit.take() {
                consumer.discard();
                return Err(exit);
            }
        }
        let output = consumer
            .finish(&mut |done, total| {
                self.handle_event(&ScanState::CountingHardLinks { done, total }, false);
            })
            .map_err(|error| Err(writing(&error)))?;
        if let Some(exit) = self.exit.take() {
            return Err(exit);
        }
        match self.scan_ui {
            ScanUi::Line => self.handle_event(&ScanState::Done, true),
            ScanUi::Off | ScanUi::Full => {}
        }
        Ok(output)
    }

    fn clear(&self) {
        if self.ui.is_none() {
            let _ = io::stderr().write_all(CLEAR_TO_END);
        }
    }
}

impl Drop for ScanProgress {
    fn drop(&mut self) {
        if let Some(ui) = self.ui.take() {
            ui.deinit();
        }
    }
}

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

fn spawning_thread(error: &io::Error) -> Vec<u8> {
    format!("Error spawning thread: {}\n", spawn_error(error)).into_bytes()
}

fn writing(error: &io::Error) -> Vec<u8> {
    let inner = error.get_ref();
    if let Some(error) = inner.and_then(|error| error.downcast_ref::<TreeError>()) {
        return match error {
            TreeError::TooManyDevices => {
                b"Maximum number of device identifiers exceeded.\n".to_vec()
            }
            TreeError::TooManyEntries => b"Maximum number of entries exceeded.\n".to_vec(),
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
