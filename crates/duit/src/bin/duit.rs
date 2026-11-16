//! The `duit` command.

use std::env;
use std::ffi::{CString, OsStr};
use std::fs::{self, File};
use std::io::{self, IsTerminal, Read, Write};
use std::num::NonZero;
use std::os::fd::AsFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::process::ExitCode;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use duit::{Arg, Args, Config, OptionError, open_error, read_error, stat_error, write_error};
use duit_format::{ImportError, JsonExport, JsonExportError, import};
use duit_scan::{ScanError, ScanOptions, scan};

/// The terminal sequence that clears from the cursor to the end of the screen. ncdu writes it to
/// standard error before an error message and at the end of a scan, to clear a progress line.
const CLEAR_TO_END: &[u8] = b"\x1b[J";

/// The first bytes of a binary export.
const BINARY_SIGNATURE: &[u8; 8] = b"\xbfncduEX1";

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
        Ok(()) => ExitCode::SUCCESS,
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

/// Write `output` to standard output, ignoring errors as ncdu does for the help and the version.
fn print(output: &[u8]) {
    let _ = io::stdout().write_all(output);
}

/// Apply the config files if `read_config`, then parse `arguments`, which follow the program
/// name, and run the mode they select. An error is the message to print before exiting with
/// status 1.
fn run(arguments: &[Vec<u8>], read_config: bool) -> Result<(), Vec<u8>> {
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
                return Ok(());
            }
            Arg::Short(b'v' | b'V') | Arg::Long(b"version") => {
                print(format!("duit {}\n", env!("CARGO_PKG_VERSION")).as_bytes());
                return Ok(());
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

    // The modes that are not ported yet.
    if export_binary.is_some() {
        return Err(b"The -O flag is not supported yet.\n".to_vec());
    }
    let Some(export_path) = export_json else {
        return Err(b"Only an export with -o is supported yet.\n".to_vec());
    };
    if import_file.is_none() && config.threads > 1 {
        return Err(b"A scan with more than one thread is not supported yet.\n".to_vec());
    }

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
    // As in ncdu, the export file is created before the import is read.
    let import_input = import_file.map(open_import).transpose()?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs());
    let mut export = JsonExport::new(
        out,
        timestamp,
        config.extended,
        config.compress.then_some(config.compress_level),
    )
    .map_err(|error| writing(&error))?;

    if let Some((input, head)) = import_input {
        match import(&mut export, input, &head) {
            Ok(()) => {}
            Err(ImportError::Input {
                line,
                byte,
                message,
            }) => {
                export.discard();
                return Err(
                    format!("Error importing file on line {line}:{byte}: {message}.\n")
                        .into_bytes(),
                );
            }
            Err(ImportError::Sink(error)) => {
                // As in ncdu, which stops without flushing its buffer, a failed export writes
                // nothing more.
                export.discard();
                return Err(writing(&error));
            }
        }
        export.finish().map_err(|error| writing(&error))?;
        let _ = io::stderr().write_all(CLEAR_TO_END);
        return Ok(());
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
    let root_error = match scan(&mut export, &path, options, &config.exclude) {
        Ok(()) => None,
        Err(ScanError::Stat(error)) => Some(stat_error(&error)),
        Err(ScanError::Open(error)) => Some(open_error(&error)),
        Err(ScanError::Sink(error)) => return Err(writing(&error)),
    };
    export.finish().map_err(|error| writing(&error))?;
    if let Some(description) = root_error {
        return Err(opening_directory(description));
    }
    let _ = io::stderr().write_all(CLEAR_TO_END);
    Ok(())
}

/// Open the import file at `path`, or standard input for `-`, and read its first 8 bytes, which
/// tell the formats apart.
fn open_import(path: &[u8]) -> Result<(File, [u8; 8]), Vec<u8>> {
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
    let mut head = [0; 8];
    file.read_exact(&mut head).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            reading("Unexpected end of file")
        } else {
            reading(read_error(&error))
        }
    })?;
    if &head == BINARY_SIGNATURE {
        return Err(b"Importing a binary export is not supported yet.\n".to_vec());
    }
    Ok((file, head))
}

fn opening_directory(description: &str) -> Vec<u8> {
    format!("Error opening directory: {description}.\n").into_bytes()
}

fn writing(error: &io::Error) -> Vec<u8> {
    match error
        .get_ref()
        .and_then(|error| error.downcast_ref::<JsonExportError>())
    {
        Some(JsonExportError::PathTooLong) => {
            b"Error writing JSON export: path too long.\n".to_vec()
        }
        None => format!("Error writing to file: {}.\n", write_error(error)).into_bytes(),
    }
}
