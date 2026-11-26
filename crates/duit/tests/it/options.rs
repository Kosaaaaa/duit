use std::fs;

use insta::assert_snapshot;
use tempfile::TempDir;

#[cfg(feature = "test-ncdu")]
use crate::assert_ncdu;
use crate::{duit, render, table};

/// A directory with the files that the cases of [`errors`] name: a regular file, an exclude file
/// with the longest line that ncdu reads, and one with a line that is one byte longer.
fn fixture() -> TempDir {
    let directory = TempDir::new().expect("temporary directory is created");
    fs::write(directory.path().join("file"), b"x").expect("file is written");
    let mut longest = vec![b'a'; 4095];
    longest.push(b'\n');
    fs::write(directory.path().join("longest"), longest).expect("file is written");
    fs::write(directory.path().join("long"), [b'a'; 4096]).expect("file is written");
    directory
}

/// Every flag of ncdu, with each accepted form of its value, ends at the unknown last option.
const ALL_FLAGS: &[&str] = &[
    "-q",
    "--slow-ui-updates",
    "--fast-ui-updates",
    "-x",
    "--one-file-system",
    "--cross-file-system",
    "-e",
    "--extended",
    "--no-extended",
    "-r",
    "-rr",
    "--enable-shell",
    "--disable-shell",
    "--enable-delete",
    "--disable-delete",
    "--enable-refresh",
    "--disable-refresh",
    "--show-hidden",
    "--hide-hidden",
    "--show-itemcount",
    "--hide-itemcount",
    "--show-mtime",
    "--hide-mtime",
    "--show-graph",
    "--hide-graph",
    "--show-percent",
    "--hide-percent",
    "--group-directories-first",
    "--no-group-directories-first",
    "--enable-natsort",
    "--disable-natsort",
    "--graph-style",
    "hash",
    "--graph-style=half-block",
    "--graph-style",
    "eighth-block",
    "--graph-style",
    "eigth-block",
    "--sort",
    "name",
    "--sort",
    "disk-usage-asc",
    "--sort",
    "apparent-size-desc",
    "--sort",
    "itemcount",
    "--sort",
    "mtime-desc",
    "--shared-column",
    "off",
    "--shared-column",
    "shared",
    "--shared-column",
    "unique",
    "--apparent-size",
    "--disk-usage",
    "-0",
    "-1",
    "-2",
    "--si",
    "--no-si",
    "-L",
    "--follow-symlinks",
    "--no-follow-symlinks",
    "--exclude",
    "*.log",
    "-X",
    "longest",
    "--exclude-from=longest",
    "--exclude-caches",
    "--include-caches",
    "--exclude-kernfs",
    "--include-kernfs",
    "-c",
    "--compress",
    "--no-compress",
    "--compress-level",
    "20",
    "--compress-level=+1",
    "--export-block-size",
    "16_000",
    "--confirm-quit",
    "--no-confirm-quit",
    "--confirm-delete",
    "--no-confirm-delete",
    "--delete-command",
    "rm -f",
    "--color",
    "off",
    "--color",
    "dark",
    "--color=dark-bg",
    "-t",
    "+8",
    "--threads",
    "1_0",
    "--threads=-0",
    "-xet1",
    "-o-",
    "--quit-after-scan",
    "--ignore-config",
    "--bogus",
];

/// Arguments that ncdu rejects. ncdu checks the arguments in order and stops at the first error.
const ERRORS: &[&[&str]] = &[
    &["-"],
    &["--", "-", "-o-", "file"],
    &["-o-", "a", "b"],
    &["--=x"],
    &["--extended=1"],
    &["--ignore-config=x"],
    &["-e=x"],
    &["-o"],
    &["--exclude"],
    &["--bogus"],
    &["--bogus=1"],
    &["-z"],
    &["-xz"],
    &["-é"],
    &["-X=missing"],
    &["-Xmissing"],
    &["-o", "a", "-o", "b"],
    &["-o", "a", "-O", "b"],
    &["-O", "a", "-o", "b"],
    &["-f", "a", "-f", "b"],
    &["--sort", "foo"],
    &["--sort", "name-asc-desc"],
    &["--sort", "-asc"],
    &["--graph-style", "x"],
    &["--shared-column", "x"],
    &["--color", "x"],
    &["--compress-level", "0"],
    &["--compress-level", "21"],
    &["--compress-level", "-0"],
    &["--compress-level", "x"],
    &["--export-block-size", "3"],
    &["--export-block-size", "16001"],
    &["--export-block-size", "16384"],
    &["-t", "256"],
    &["-t", ""],
    &["-t", "+"],
    &["-t", "_1"],
    &["-t", "1_"],
    &["-t", "-1"],
    &["-t", "-+1"],
    &["-t", " 1"],
    &["-X", "missing"],
    &["-X", "missing", "--bogus"],
    &["-X", "."],
    &["-X", "long"],
    &["-o", ""],
    &["-o", "."],
    &["-o-", "missing"],
    &["-o-", ""],
    &["-o-", "--", "--arg"],
    &["-o-", "file"],
    &["-o-", "file/x"],
    &["."],
    ALL_FLAGS,
];

#[test]
fn errors() {
    let directory = fixture();
    #[cfg(feature = "test-ncdu")]
    for arguments in ERRORS {
        assert_ncdu(directory.path(), arguments);
    }
    assert_snapshot!(table(directory.path(), ERRORS), @r#"
        $ duit -
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JInvalid option '-'.

        $ duit -- - -o- file
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JMultiple directories given, see duit -h for help.

        $ duit -o- a b
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JMultiple directories given, see duit -h for help.

        $ duit --=x
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JInvalid option '--=x'.

        $ duit --extended=1
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JOption '--extended' does not expect an argument.

        $ duit --ignore-config=x
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JOption '--ignore-config' does not expect an argument.

        $ duit -e=x
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnrecognized option '-='.

        $ duit -o
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JOption '-o' requires an argument.

        $ duit --exclude
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JOption '--exclude' requires an argument.

        $ duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnrecognized option '--bogus'.

        $ duit --bogus=1
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnrecognized option '--bogus'.

        $ duit -z
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnrecognized option '-z'.

        $ duit -xz
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnrecognized option '-z'.

        $ duit -é
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnrecognized option '-�'.

        $ duit -X=missing
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from =missing: No such file or directory.

        $ duit -Xmissing
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from missing: No such file or directory.

        $ duit -o a -o b
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JThe -o flag can only be given once.

        $ duit -o a -O b
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JThe -O flag can only be given once.

        $ duit -O a -o b
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JThe -o flag can only be given once.

        $ duit -f a -f b
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JThe -f flag can only be given once.

        $ duit --sort foo
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnknown --sort option: foo.

        $ duit --sort name-asc-desc
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnknown --sort option: name-asc.

        $ duit --sort -asc
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnknown --sort option: .

        $ duit --graph-style x
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnknown --graph-style option: x.

        $ duit --shared-column x
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnknown --shared-column option: x.

        $ duit --color x
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnknown --color option: x.

        $ duit --compress-level 0
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JInvalid number for --compress-level: 0.

        $ duit --compress-level 21
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JInvalid number for --compress-level: 21.

        $ duit --compress-level -0
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JInvalid number for --compress-level: -0.

        $ duit --compress-level x
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JInvalid number for --compress-level: x.

        $ duit --export-block-size 3
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JInvalid number for --export-block-size: 3.

        $ duit --export-block-size 16001
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JInvalid number for --export-block-size: 16001.

        $ duit --export-block-size 16384
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JInvalid number for --export-block-size: 16384.

        $ duit -t 256
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JInvalid number of --threads: 256.

        $ duit -t ''
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JInvalid number of --threads: .

        $ duit -t +
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JInvalid number of --threads: +.

        $ duit -t _1
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JInvalid number of --threads: _1.

        $ duit -t 1_
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JInvalid number of --threads: 1_.

        $ duit -t -1
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JInvalid number of --threads: -1.

        $ duit -t -+1
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JInvalid number of --threads: -+1.

        $ duit -t ' 1'
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JInvalid number of --threads:  1.

        $ duit -X missing
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from missing: No such file or directory.

        $ duit -X missing --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from missing: No such file or directory.

        $ duit -X .
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from .: ReadFailed.

        $ duit -X long
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from long: StreamTooLong.

        $ duit -o ''
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError opening export file: No such file or directory.

        $ duit -o .
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError opening export file: Is a directory.

        $ duit -o- missing
        exit_code: 1
        ----- stdout -----
        [1,2,{[METADATA]}]

        ----- stderr -----
        ^[[JError opening directory: No such file or directory.

        $ duit -o- ''
        exit_code: 1
        ----- stdout -----
        [1,2,{[METADATA]}]

        ----- stderr -----
        ^[[JError opening directory: No such file or directory.

        $ duit -o- -- --arg
        exit_code: 1
        ----- stdout -----
        [1,2,{[METADATA]}]

        ----- stderr -----
        ^[[JError opening directory: No such file or directory.

        $ duit -o- file
        exit_code: 1
        ----- stdout -----
        [1,2,{[METADATA]}]

        ----- stderr -----
        ^[[JError opening directory: Not a directory.

        $ duit -o- file/x
        exit_code: 1
        ----- stdout -----
        [1,2,{[METADATA]}]

        ----- stderr -----
        ^[[JError opening directory: Unexpected.

        $ duit .
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JStandard input is not a TTY. Did you mean to import a file using '-f -'?

        $ duit -q --slow-ui-updates --fast-ui-updates -x --one-file-system --cross-file-system -e --extended --no-extended -r -rr --enable-shell --disable-shell --enable-delete --disable-delete --enable-refresh --disable-refresh --show-hidden --hide-hidden --show-itemcount --hide-itemcount --show-mtime --hide-mtime --show-graph --hide-graph --show-percent --hide-percent --group-directories-first --no-group-directories-first --enable-natsort --disable-natsort --graph-style hash --graph-style=half-block --graph-style eighth-block --graph-style eigth-block --sort name --sort disk-usage-asc --sort apparent-size-desc --sort itemcount --sort mtime-desc --shared-column off --shared-column shared --shared-column unique --apparent-size --disk-usage -0 -1 -2 --si --no-si -L --follow-symlinks --no-follow-symlinks --exclude *.log -X longest --exclude-from=longest --exclude-caches --include-caches --exclude-kernfs --include-kernfs -c --compress --no-compress --compress-level 20 --compress-level=+1 --export-block-size 16_000 --confirm-quit --no-confirm-quit --confirm-delete --no-confirm-delete --delete-command 'rm -f' --color off --color dark --color=dark-bg -t +8 --threads 1_0 --threads=-0 -xet1 -o- --quit-after-scan --ignore-config --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnrecognized option '--bogus'.
    "#);
}

/// ncdu does not support pseudo filesystems outside Linux.
#[cfg(not(target_os = "linux"))]
#[test]
fn exclude_kernfs() {
    let directory = TempDir::new().expect("temporary directory is created");
    let arguments: &[&str] = &["--exclude-kernfs", "-o-", "."];
    #[cfg(feature = "test-ncdu")]
    assert_ncdu(directory.path(), arguments);
    assert_snapshot!(table(directory.path(), &[arguments]), @r#"
        $ duit --exclude-kernfs -o- .
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JThe --exclude-kernfs flag is currently only supported on Linux.
    "#);
}

/// The help ends the parsing, so an argument after it is not checked.
#[test]
fn help() {
    let directory = TempDir::new().expect("temporary directory is created");
    let help = render(&duit(directory.path(), &["-h"]));
    for arguments in [&["-?"][..], &["--help"], &["--help=x"], &["-h", "--bogus"]] {
        #[cfg(feature = "test-ncdu")]
        assert_ncdu(directory.path(), arguments);
        assert_eq!(render(&duit(directory.path(), arguments)), help);
    }
    assert_snapshot!(help, @r#"
        exit_code: 0
        ----- stdout -----
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
          -q, --slow-ui-updates      "Quiet" mode, refresh interval 2 seconds
          --enable-shell             Enable/disable shell spawning feature
          --enable-delete            Enable/disable file deletion feature
          --enable-refresh           Enable/disable directory refresh feature
          -r                         Read only (--disable-delete)
          -rr                        Read only++ (--disable-delete & --disable-shell)
          --si                       Use base 10 (SI) prefixes instead of base 2
          --apparent-size            Show apparent size instead of disk usage by default
          --hide-hidden              Hide "hidden" or excluded files by default
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

        ----- stderr -----
    "#);
}

#[test]
fn version() {
    let directory = TempDir::new().expect("temporary directory is created");
    let version = render(&duit(directory.path(), &["-v"]));
    for arguments in [&["-V"][..], &["--version"], &["--version", "--bogus"]] {
        assert_eq!(render(&duit(directory.path(), arguments)), version);
    }
    assert_snapshot!(version.replace(env!("CARGO_PKG_VERSION"), "[VERSION]"), @r#"
        exit_code: 0
        ----- stdout -----
        duit [VERSION]

        ----- stderr -----
    "#);
}
