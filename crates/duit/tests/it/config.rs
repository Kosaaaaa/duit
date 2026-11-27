use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use insta::assert_snapshot;
use tempfile::TempDir;

#[cfg(feature = "test-ncdu")]
use crate::render_as_duit;
use crate::{redact, render, text, with_config};

const XDG: &[(&str, &str)] = &[("HOME", "home"), ("XDG_CONFIG_HOME", "xdg")];

fn duit_program() -> &'static OsStr {
    OsStr::new(env!("CARGO_BIN_EXE_duit"))
}

fn write(directory: &Path, path: &str, contents: &[u8]) {
    let path = directory.join(path);
    fs::create_dir_all(path.parent().expect("path has a parent")).expect("directory is created");
    fs::write(path, contents).expect("file is written");
}

fn run(directory: &Path, environment: &[(&str, &str)], arguments: &[&str]) -> String {
    let command: Vec<String> = environment
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .chain(["duit".to_string()])
        .chain(arguments.iter().map(|argument| (*argument).to_string()))
        .collect();
    let output = with_config(duit_program(), directory, environment, arguments);
    format!(
        "$ {}\n{}\n\n",
        command.join(" "),
        render(&output).trim_end()
    )
}

fn compare(directory: &Path, environment: &[(&str, &str)], arguments: &[&str]) -> String {
    #[cfg(feature = "test-ncdu")]
    assert_eq!(
        render_as_duit(&with_config(
            OsStr::new("ncdu"),
            directory,
            environment,
            arguments
        )),
        render(&with_config(
            duit_program(),
            directory,
            environment,
            arguments
        )),
        "environment: {environment:?}, arguments: {arguments:?}"
    );
    run(directory, environment, arguments)
}

fn compare_config(
    directory: &Path,
    config: &str,
    environment: &[(&str, &str)],
    arguments: &[&str],
) -> String {
    write(directory, "xdg/ncdu/config", config.as_bytes());
    let mut lines = String::new();
    for line in text(config.as_bytes()).replace('\t', "^I").lines() {
        let _ = writeln!(lines, "{line}$");
    }
    format!(
        "$ cat -A xdg/ncdu/config\n{lines}{}",
        compare(directory, environment, arguments)
    )
}

#[test]
fn lines() {
    let directory = TempDir::new().expect("temporary directory is created");
    let configs = [
        "# -X missing\n\n \t \n@\n@# -X missing\n",
        "  -X   missing  \n",
        "-X=missing\n",
        "-X\t \tmissing\n",
        "\x0b-X \x0c\x0bmissing\r\n",
        "-X missing extra\n",
        "-X missing\0extra\n",
        "-x foo\n",
        "-o-\n",
        "--ignore-config\n",
        "--color foo\n",
        "--exclude\n",
        "-x --si=1\n",
        "-\n",
        "@--bogus\n@--color foo\n@--exclude\n@-x --si=1\n@-\n@-X missing\n-X other\n",
    ];
    let mut rendered = String::new();
    for config in configs {
        rendered.push_str(&compare_config(directory.path(), config, XDG, &["--bogus"]));
    }
    assert_snapshot!(rendered, @r#"
        $ cat -A xdg/ncdu/config
        # -X missing$
        $
         ^I $
        @$
        @# -X missing$
        $ HOME=home XDG_CONFIG_HOME=xdg duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnrecognized option '--bogus'.

        $ cat -A xdg/ncdu/config
          -X   missing  $
        $ HOME=home XDG_CONFIG_HOME=xdg duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from missing: No such file or directory.

        $ cat -A xdg/ncdu/config
        -X=missing$
        $ HOME=home XDG_CONFIG_HOME=xdg duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from missing: No such file or directory.

        $ cat -A xdg/ncdu/config
        -X^I ^Imissing$
        $ HOME=home XDG_CONFIG_HOME=xdg duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from missing: No such file or directory.

        $ cat -A xdg/ncdu/config
        ^K-X ^L^Kmissing^M$
        $ HOME=home XDG_CONFIG_HOME=xdg duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from missing: No such file or directory.

        $ cat -A xdg/ncdu/config
        -X missing extra$
        $ HOME=home XDG_CONFIG_HOME=xdg duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from missing extra: No such file or directory.

        $ cat -A xdg/ncdu/config
        -X missing^@extra$
        $ HOME=home XDG_CONFIG_HOME=xdg duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from missing^@extra: No such file or directory.

        $ cat -A xdg/ncdu/config
        -x foo$
        $ HOME=home XDG_CONFIG_HOME=xdg duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnrecognized option in config file 'xdg/ncdu/config': foo.
        Run with --ignore-config to skip reading config files.

        $ cat -A xdg/ncdu/config
        -o-$
        $ HOME=home XDG_CONFIG_HOME=xdg duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnrecognized option in config file 'xdg/ncdu/config': -o.
        Run with --ignore-config to skip reading config files.

        $ cat -A xdg/ncdu/config
        --ignore-config$
        $ HOME=home XDG_CONFIG_HOME=xdg duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnrecognized option in config file 'xdg/ncdu/config': --ignore-config.
        Run with --ignore-config to skip reading config files.

        $ cat -A xdg/ncdu/config
        --color foo$
        $ HOME=home XDG_CONFIG_HOME=xdg duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnknown --color option: foo.

        $ cat -A xdg/ncdu/config
        --exclude$
        $ HOME=home XDG_CONFIG_HOME=xdg duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JOption '--exclude' requires an argument.

        $ cat -A xdg/ncdu/config
        -x --si=1$
        $ HOME=home XDG_CONFIG_HOME=xdg duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JOption '--si' does not expect an argument.

        $ cat -A xdg/ncdu/config
        -$
        $ HOME=home XDG_CONFIG_HOME=xdg duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JInvalid option '-'.

        $ cat -A xdg/ncdu/config
        @--bogus$
        @--color foo$
        @--exclude$
        @-x --si=1$
        @-$
        @-X missing$
        -X other$
        $ HOME=home XDG_CONFIG_HOME=xdg duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from other: No such file or directory.
    "#);
}

#[test]
fn environment() {
    let directory = TempDir::new().expect("temporary directory is created");
    let root = directory.path();
    write(root, "xdg/ncdu/config", b"-X from-xdg\n");
    write(root, "home/.config/ncdu/config", b"-X from-home\n");
    write(root, "ncdu/config", b"-X from-empty-xdg\n");
    write(root, ".config/ncdu/config", b"-X from-empty-home\n");
    write(root, "file", b"");
    let mut rendered = String::new();
    for environment in [
        XDG,
        &[("HOME", "home")],
        &[("HOME", "home"), ("XDG_CONFIG_HOME", "")],
        &[("HOME", "")],
        &[],
        &[("HOME", "home"), ("XDG_CONFIG_HOME", "file")],
    ] {
        rendered.push_str(&compare(root, environment, &["--bogus"]));
    }
    assert_snapshot!(rendered, @r#"
        $ HOME=home XDG_CONFIG_HOME=xdg duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from from-xdg: No such file or directory.

        $ HOME=home duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from from-home: No such file or directory.

        $ HOME=home XDG_CONFIG_HOME= duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from from-empty-xdg: No such file or directory.

        $ HOME= duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from from-empty-home: No such file or directory.

        $ duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnrecognized option '--bogus'.

        $ HOME=home XDG_CONFIG_HOME=file duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnrecognized option '--bogus'.
    "#);
}

#[test]
fn duit_files() {
    let directory = TempDir::new().expect("temporary directory is created");
    let root = directory.path();
    write(root, "both/duit/config", b"-X duit\n");
    write(root, "both/ncdu/config", b"-X ncdu\n");
    write(root, "home/.config/duit/config", b"-X home-duit\n");
    write(root, "home/.config/ncdu/config", b"-X home-ncdu\n");
    write(root, "empty/duit/config", b"");
    write(root, "empty/ncdu/config", b"-X ncdu\n");
    write(root, "not-directory/duit", b"");
    write(root, "not-directory/ncdu/config", b"-X ncdu\n");
    let mut rendered = String::new();
    for environment in [
        &[("XDG_CONFIG_HOME", "both")],
        &[("HOME", "home")],
        &[("XDG_CONFIG_HOME", "empty")],
        &[("XDG_CONFIG_HOME", "not-directory")],
    ] {
        rendered.push_str(&run(root, environment, &["--bogus"]));
    }
    assert_snapshot!(rendered, @r#"
        $ XDG_CONFIG_HOME=both duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from duit: No such file or directory.

        $ HOME=home duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from home-duit: No such file or directory.

        $ XDG_CONFIG_HOME=empty duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnrecognized option '--bogus'.

        $ XDG_CONFIG_HOME=not-directory duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from ncdu: No such file or directory.
    "#);
}

/// A config file that cannot be opened or read ends the run. A file without permissions can be
/// opened by root, so this test fails when it runs as root.
#[test]
fn file_errors() {
    let directory = TempDir::new().expect("temporary directory is created");
    let root = directory.path();
    fs::create_dir_all(root.join("directory/ncdu/config")).expect("directory is created");
    let mut longest = b"#".repeat(4095);
    longest.push(b'\n');
    write(root, "longest/ncdu/config", &longest);
    write(root, "long/ncdu/config", &b"#".repeat(4096));
    write(root, "denied/ncdu/config", b"");
    fs::set_permissions(
        root.join("denied/ncdu/config"),
        fs::Permissions::from_mode(0o000),
    )
    .expect("permissions are set");
    let mut rendered = String::new();
    for xdg in ["directory", "longest", "long", "denied"] {
        rendered.push_str(&compare(root, &[("XDG_CONFIG_HOME", xdg)], &["--bogus"]));
    }
    assert_snapshot!(rendered, @r#"
        $ XDG_CONFIG_HOME=directory duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading from directory/ncdu/config: ReadFailed
        Run with --ignore-config to skip reading config files.

        $ XDG_CONFIG_HOME=longest duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnrecognized option '--bogus'.

        $ XDG_CONFIG_HOME=long duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading from long/ncdu/config: StreamTooLong
        Run with --ignore-config to skip reading config files.

        $ XDG_CONFIG_HOME=denied duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError opening denied/ncdu/config: Access denied
        Run with --ignore-config to skip reading config files.
    "#);
}

#[test]
fn ignore_config() {
    let directory = TempDir::new().expect("temporary directory is created");
    let mut rendered = String::new();
    for arguments in [
        &["--bogus"][..],
        &["--bogus", "--ignore-config"],
        &["-o", "--ignore-config", "--bogus"],
        &["--", "--ignore-config", "other"],
        &["--ignore-config=x"],
    ] {
        rendered.push_str(&compare_config(
            directory.path(),
            "--color foo\n",
            XDG,
            arguments,
        ));
    }
    assert_snapshot!(rendered, @r#"
        $ cat -A xdg/ncdu/config
        --color foo$
        $ HOME=home XDG_CONFIG_HOME=xdg duit --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnknown --color option: foo.

        $ cat -A xdg/ncdu/config
        --color foo$
        $ HOME=home XDG_CONFIG_HOME=xdg duit --bogus --ignore-config
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnrecognized option '--bogus'.

        $ cat -A xdg/ncdu/config
        --color foo$
        $ HOME=home XDG_CONFIG_HOME=xdg duit -o --ignore-config --bogus
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnrecognized option '--bogus'.

        $ cat -A xdg/ncdu/config
        --color foo$
        $ HOME=home XDG_CONFIG_HOME=xdg duit -- --ignore-config other
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JMultiple directories given, see duit -h for help.

        $ cat -A xdg/ncdu/config
        --color foo$
        $ HOME=home XDG_CONFIG_HOME=xdg duit --ignore-config=x
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JUnknown --color option: foo.
    "#);
}

#[test]
fn home() {
    let directory = TempDir::new().expect("temporary directory is created");
    let root = directory.path();
    let mut rendered = String::new();
    for (home, path) in [
        ("home/", "~"),
        ("home//", "~/x"),
        ("/", "~"),
        ("/", "~/missing"),
        ("", "~"),
        ("", "~/missing"),
        ("home", "~missing-user/x"),
        ("home", "x/~/x"),
    ] {
        rendered.push_str(&compare_config(
            root,
            &format!("-X {path}\n"),
            &[("HOME", home), ("XDG_CONFIG_HOME", "xdg")],
            &[],
        ));
    }
    write(root, "xdg/ncdu/config", b"");
    rendered.push_str(&compare(root, XDG, &["-X", "~/x"]));
    assert_snapshot!(rendered, @r#"
        $ cat -A xdg/ncdu/config
        -X ~$
        $ HOME=home/ XDG_CONFIG_HOME=xdg duit
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from home: No such file or directory.

        $ cat -A xdg/ncdu/config
        -X ~/x$
        $ HOME=home// XDG_CONFIG_HOME=xdg duit
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from home/x: No such file or directory.

        $ cat -A xdg/ncdu/config
        -X ~$
        $ HOME=/ XDG_CONFIG_HOME=xdg duit
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from /: ReadFailed.

        $ cat -A xdg/ncdu/config
        -X ~/missing$
        $ HOME=/ XDG_CONFIG_HOME=xdg duit
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from /missing: No such file or directory.

        $ cat -A xdg/ncdu/config
        -X ~$
        $ HOME= XDG_CONFIG_HOME=xdg duit
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from /: ReadFailed.

        $ cat -A xdg/ncdu/config
        -X ~/missing$
        $ HOME= XDG_CONFIG_HOME=xdg duit
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from /missing: No such file or directory.

        $ cat -A xdg/ncdu/config
        -X ~missing-user/x$
        $ HOME=home XDG_CONFIG_HOME=xdg duit
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from ~missing-user/x: No such file or directory.

        $ cat -A xdg/ncdu/config
        -X x/~/x$
        $ HOME=home XDG_CONFIG_HOME=xdg duit
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from x/~/x: No such file or directory.

        $ HOME=home XDG_CONFIG_HOME=xdg duit -X ~/x
        exit_code: 1
        ----- stdout -----

        ----- stderr -----
        ^[[JError reading excludes from ~/x: No such file or directory.
    "#);
}

#[test]
fn home_of_user() {
    let directory = TempDir::new().expect("temporary directory is created");
    let root = directory.path();
    let output = Command::new("id").arg("-un").output().expect("id runs");
    let user = String::from_utf8(output.stdout).expect("user name is UTF-8");
    let run = |environment: &[(&str, &str)], path: &str| {
        write(root, "xdg/ncdu/config", format!("-X {path}\n").as_bytes());
        #[cfg(feature = "test-ncdu")]
        compare(root, environment, &[]);
        render(&with_config(duit_program(), root, environment, &[]))
    };
    let current = run(&[("XDG_CONFIG_HOME", "xdg")], "~/missing");
    let named = run(XDG, &format!("~{}/missing", user.trim_end()));
    assert_eq!(current, named);
    assert_ne!(current, run(XDG, "~/missing"));
}

#[test]
fn exclude() {
    let directory = TempDir::new().expect("temporary directory is created");
    let root = directory.path();
    fs::create_dir_all(root.join("tree/home/x")).expect("directory is created");
    write(root, "xdg/ncdu/config", b"--exclude ~/x\n@--bogus -e\n");
    let output = compare(root, XDG, &["-o-", "tree"]);
    let root = root.canonicalize().expect("path is resolved");
    assert_snapshot!(redact(&output, &root.join("tree")), @r#"
        $ HOME=home XDG_CONFIG_HOME=xdg duit -o- tree
        exit_code: 0
        ----- stdout -----
        [1,2,{[METADATA]},
        [{"name":"[ROOT]","asize":[N],"dev":[N]},
        [{"name":"home","asize":[N]},
        {"name":"x","excluded":"pattern"}]]]

        ----- stderr -----
        ^[[J
    "#);
}
