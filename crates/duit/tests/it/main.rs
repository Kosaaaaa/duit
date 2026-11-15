//! Integration tests for the `duit` binary.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Output;

use assert_cmd::Command;

mod export;
mod options;

/// Run duit with `arguments` in `directory`. Standard input is not a terminal.
fn duit(directory: &Path, arguments: &[&str]) -> Output {
    Command::cargo_bin("duit")
        .expect("duit is built")
        .current_dir(directory)
        .arg("--ignore-config")
        .args(arguments)
        .output()
        .expect("duit runs")
}

/// Run ncdu with `arguments` in `directory`, like [`duit`].
#[cfg(feature = "test-ncdu")]
fn ncdu(directory: &Path, arguments: &[&str]) -> Output {
    std::process::Command::new("ncdu")
        .current_dir(directory)
        .arg("--ignore-config")
        .args(arguments)
        .output()
        .expect("ncdu 2.9.2 is on PATH")
}

/// Assert that ncdu exits like duit and writes the same output, apart from the program name.
#[cfg(feature = "test-ncdu")]
fn assert_ncdu(directory: &Path, arguments: &[&str]) {
    let expected = render(&ncdu(directory, arguments))
        .replace("ncdu <options>", "duit <options>")
        .replace("quitting ncdu", "quitting duit")
        .replace("see ncdu -h", "see duit -h");
    assert_eq!(
        expected,
        render(&duit(directory, arguments)),
        "arguments: {arguments:?}"
    );
}

/// The bytes of an output as text, with the escape character shown as `^[`, as `cat -v` does,
/// and the metadata block of an export replaced, because it holds the program name, its version,
/// and the time.
fn text(bytes: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(bytes).replace('\x1b', "^[");
    if text.starts_with("[1,2,{")
        && let Some(end) = text.find('}')
    {
        text.replace_range(..=end, "[1,2,{[METADATA]}");
    }
    text
}

/// The exit code, standard output, and standard error of a run, as one string.
fn render(output: &Output) -> String {
    let mut rendered = String::new();
    let _ = writeln!(
        rendered,
        "exit_code: {}",
        output.status.code().unwrap_or(-1)
    );
    let _ = writeln!(rendered, "----- stdout -----\n{}", text(&output.stdout));
    let _ = write!(rendered, "----- stderr -----\n{}", text(&output.stderr));
    rendered
}

/// Run duit once per entry of `cases` in `directory` and render each run under its command line.
fn table(directory: &Path, cases: &[&[&str]]) -> String {
    let mut rendered = String::new();
    for arguments in cases {
        let quoted: Vec<String> = arguments
            .iter()
            .map(|argument| {
                if argument.is_empty() || argument.contains(' ') {
                    format!("'{argument}'")
                } else {
                    (*argument).to_string()
                }
            })
            .collect();
        let _ = writeln!(
            rendered,
            "$ duit {}\n{}\n",
            quoted.join(" "),
            render(&duit(directory, arguments)).trim_end()
        );
    }
    rendered
}
