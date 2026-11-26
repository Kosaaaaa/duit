//! The `duit` command.

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
    Action, Browser, BrowserOptions, Console, Deletion, InitError, Input, Loading, ScanKey,
    ScanScreen, ScanState, Ui,
};
use rustix::fs::CWD;

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

    let scan_ui = config
        .scan_ui
        .unwrap_or(match export_json.or(export_binary) {
            Some(path) if path != b"-" && io::stdout().is_terminal() => ScanUi::Line,
            Some(_) => ScanUi::Off,
            None => ScanUi::Full,
        });
    // `nc_tty` in `main.zig`: the full-screen interface draws on `/dev/tty` when standard input or
    // standard output is not for the terminal.
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
        // `fill` ends with the clear that ncdu writes when it returns after the scan, or when
        // `ui.init` takes over the terminal.
        let tree = match fill(memory, import_input, scan_directory, &config, &mut progress) {
            Ok(tree) => tree,
            Err(exit) => return exit,
        };
        if quit_after_scan {
            return Ok(ExitCode::SUCCESS);
        }
        // A scan or an import that succeeds has created the root.
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

/// Run the browser that `open` opens, with the tree it reads, until the user quits, the loop at
/// the end of `main` in `main.zig` with `handleEvent`. Like ncdu, it handles every key that is
/// waiting before it draws again. As in ncdu, the browser is opened once the terminal is, so that
/// the root of a binary export is read then, and `open` gives `None` for a root that is not a
/// directory. While the browser lists a directory, [`Load`] draws the loading screen after
/// `update_delay`. The browser draws on `ui` if the progress of `-2` took over the terminal. A
/// refresh scans with the options of `config`, and draws its progress on `screen`, the screen of
/// the scan before the browser if there was one, as ncdu keeps the confirmation of
/// `--confirm-quit` and the step of the animation in globals that no scan resets.
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

/// List the open directory of `browser` again after `tree` changed, the `browser.loadDir` call
/// that follows the `.refresh` and `.delete` states of the loop in `main.zig`, and return `ui`, or
/// how duit ends once `ui` is closed. [`Load`] draws the loading screen after `update_delay`.
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

/// Scan `directory` of `tree` again into the tree, the `.refresh` state of the loop in `main.zig`,
/// and return `ui`, or how duit ends. The progress is drawn on the full screen of `ui` whatever
/// `-0` and `-1` say, as ncdu sets `scan_ui` to `.full` before it starts the browser. A directory
/// that cannot be opened leaves its entries as they were, and shows an error until a key is
/// pressed. The update delay runs from `drawn`, when the browser was drawn, as ncdu keeps one timer
/// for every screen. The progress is drawn on `screen`, which keeps its state for the next scan.
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
    // A name with a NUL byte, which only an import has, ends the path there, as for ncdu.
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
    // duit ends after an abort, and the tree with it.
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

/// The message that ncdu exits with when it cannot take over the terminal.
fn init_error(error: InitError) -> Vec<u8> {
    match error {
        InitError::OpenTty(error) => {
            format!("Error opening /dev/tty: {}.\n", strerror(&error)).into_bytes()
        }
        InitError::Terminal(_) => b"Error initializing the terminal.\n".to_vec(),
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

/// A deletion that the browser runs, as `delete.zig` calls `handleEvent` in `main.zig` before each
/// entry that it deletes and while it waits for a key in its error window. The window is drawn
/// when the deletion waits, and else once `update_delay` has passed since the screen was drawn.
/// The keys that wait then go to the deletion while it runs. Ctrl-C aborts it, and duit ends.
struct Delete<'a> {
    ui: &'a mut Ui,
    options: &'a BrowserOptions,
    update_delay: Duration,
    drawn: Instant,
    /// How duit ends once the deletion is aborted, after Ctrl-C or an error reading the keys.
    exit: Option<Result<ExitCode, Vec<u8>>>,
}

impl Delete<'_> {
    fn handle_event(&mut self, deletion: &mut Deletion, tree: &Tree, block: bool) {
        // shortcut: keys are read only when the window is drawn, because `getch` without `block`
        // waits up to 1 ms. Read them before each entry once it returns at once.
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

/// An export or the in-memory tree, which an import or a scan writes into.
trait Consumer: Sink {
    /// What the consumer gives when it is complete.
    type Output;

    /// How many inodes the last count of the hard links went through, if [`Consumer::finish`]
    /// adds the sizes of the hard links, which ncdu shows as a step of the scan, as `done` in
    /// `mem_sink.zig` does. The first frame of the step shows this count, which ncdu keeps from
    /// the last count until the next one starts.
    fn counted_inodes(&self) -> Option<usize> {
        None
    }

    /// Write the end of the export and flush it, or complete the tree, and give `progress` the
    /// progress of the count of the hard links.
    fn finish(self, progress: &mut dyn FnMut(usize, usize)) -> io::Result<Self::Output>;

    /// Drop the export without writing what is still buffered, as ncdu does when it stops on an
    /// error.
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

    /// The blocks that are full are written already, and the current one is dropped.
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

/// An import file, opened by [`open_import`].
enum ImportInput {
    /// A JSON export, whose first 8 bytes are read.
    Json(File, [u8; 8]),
    /// A binary export, whose index block is read.
    Binary(BinaryReader<File>),
}

/// Write the import of `import_input`, or else the scan of `scan_directory`, into `consumer`, and
/// show its `progress`. An error is how duit ends instead: with a status once the user aborts, or
/// with a message.
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

    let (consumer, root_error) = scan_into(consumer, scan_directory, config, progress)?;
    let output = progress.finish(consumer)?;
    if let Some(description) = root_error {
        return Err(Err(opening_directory(description)));
    }
    progress.clear();
    Ok(output)
}

/// A consumer after a scan, with the description of the error if the root could not be read.
type Scanned<C> = (C, Option<&'static str>);

/// Write the scan of `scan_directory` into `consumer`, and show its `progress`. Give back the
/// consumer, with the description of the error if the root could not be read, which still ends an
/// export, as in ncdu. An error is how duit ends instead.
fn scan_into<C: Consumer>(
    consumer: C,
    scan_directory: Option<&[u8]>,
    config: &Config,
    progress: &mut ScanProgress,
) -> Result<Scanned<C>, Result<ExitCode, Vec<u8>>> {
    // ncdu scans the absolute path if it can resolve one.
    let directory = scan_directory.unwrap_or(b".");
    let path = fs::canonicalize(OsStr::from_bytes(directory)).map_or_else(
        |_| directory.to_vec(),
        |path| path.into_os_string().into_vec(),
    );
    // Arguments are C strings and cannot hold a NUL byte.
    let path = CString::new(path).map_err(|_| Err(opening_directory("Unexpected")))?;
    let mut report = |state: &Progress| progress.report(state);
    let mut sink = ProgressSink::new(consumer, Producer::Scan, &mut report);
    let scanned = scan(&mut sink, &path, scan_options(config), &config.exclude);
    let consumer = sink.into_inner();
    if let Some(exit) = progress.exit.take() {
        consumer.discard();
        return Err(exit);
    }
    match scanned {
        Ok(()) => Ok((consumer, None)),
        Err(ScanError::Stat(error)) => Ok((consumer, Some(stat_error(&error)))),
        Err(ScanError::Open(error)) => Ok((consumer, Some(open_error(&error)))),
        // As `ui.die` in ncdu, which writes nothing more.
        Err(ScanError::Spawn(error)) => {
            consumer.discard();
            Err(Err(spawning_thread(&error)))
        }
        Err(ScanError::Sink(error)) => Err(Err(writing(&error))),
    }
}

/// Scan `scan_directory` with several threads into memory, and write the tree into `export` once
/// the scan is done, as `createThreads` and `done` in `sink.zig` do, because a JSON export is
/// written depth-first. The progress counts again from 0 for the export. Unlike ncdu, which
/// crashes there, a root that cannot be read ends the export as with one thread.
fn export_from_memory<W: Write>(
    export: JsonExport<W>,
    scan_directory: Option<&[u8]>,
    config: &Config,
    progress: &mut ScanProgress,
) -> Result<(), Result<ExitCode, Vec<u8>>> {
    let memory = MemorySink::without_stats(config.extended);
    let (memory, root_error) = match scan_into(memory, scan_directory, config, progress) {
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

/// The options of a scan, from `config`.
fn scan_options(config: &Config) -> ScanOptions {
    ScanOptions {
        same_filesystem: config.same_filesystem,
        follow_symlinks: config.follow_symlinks,
        exclude_kernfs: config.exclude_kernfs,
        exclude_caches: config.exclude_caches,
        threads: config.threads,
    }
}

/// The progress of a scan or an import, which `handleEvent` in `main.zig` draws with `draw` in
/// `sink.zig` once `update_delay` has passed since it last drew, or when a draw is forced. The
/// timer starts before the export file is opened, as in ncdu.
///
/// With `-2`, the first draw takes over the terminal, as `ui.init` in `ui.zig` does, and the
/// browser keeps it. Dropping the progress restores a terminal that the browser did not take.
struct ScanProgress {
    scan_ui: ScanUi,
    update_delay: Duration,
    /// The options that the screen of `-2` draws with, and that `-1` takes `--si` from.
    options: BrowserOptions,
    drawn: Instant,
    console: Console,
    screen: ScanScreen,
    /// Whether the screen of `-2` draws on `/dev/tty`, as [`Ui::init`] takes it.
    open_tty: bool,
    ui: Option<Ui>,
    /// How duit ends once the user aborted the scan or the import, after Ctrl-C, or after the
    /// screen of `-2` failed.
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

    /// Handle an event of the scan or the import, which has reported `progress`, and fail to stop
    /// it once duit is to end.
    fn report(&mut self, progress: &Progress) -> io::Result<()> {
        if progress.zeroing() {
            self.handle_event(&ScanState::Zeroing, true);
        } else if self.drawn.elapsed() > self.update_delay {
            // The paths are joined only for a draw, as the scan reports an event for each entry.
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

    /// Draw `state` on the full screen, taking over the terminal first, and handle the keys that
    /// wait. `q` aborts the scan or the import with status 0, as `ui.quit` in `ui.zig` does.
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
        // shortcut: keys are read only when the screen is drawn, because `getch` without `block`
        // waits up to 1 ms. Read them at each event once it returns at once.
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

    /// Draw `state` on the full screen that the progress took over, and wait for the key that
    /// closes it, as `main.zig` calls `handleEvent` until the error of a refresh is closed. Fails
    /// with how duit ends after Ctrl-C or an error reading the keys.
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

    /// Complete `consumer` once the scan or the import is done, and erase the progress of `-1`, as
    /// `done` in `sink.zig` does. Ctrl-C or an error reading the keys while the hard links are
    /// counted ends duit once they are counted.
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

    /// Write the clear that ncdu writes at the end of a scan, as `ui.init` takes over the
    /// terminal or `ui.deinit` gives it back, when the progress did not take it over.
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
