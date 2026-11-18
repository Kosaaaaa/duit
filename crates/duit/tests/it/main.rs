//! Integration tests for the `duit` binary.

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, Output};

mod browse;
mod config;
mod convert;
mod export;
mod import;
mod options;

/// The first bytes of a binary export.
const BINARY_SIGNATURE: &[u8] = b"\xbfncduEX1";

/// Run duit with `arguments` in `directory`. Standard input is not a terminal.
fn duit(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_duit"))
        .current_dir(directory)
        .arg("--ignore-config")
        .args(arguments)
        .output()
        .expect("duit runs")
}

/// Run `program` with `arguments` in `directory`, with `HOME` and `XDG_CONFIG_HOME` set as in
/// `environment` and unset otherwise, so that it reads the user config file that `environment`
/// selects. Standard input is not a terminal.
///
/// The system config files are read from fixed paths that a test cannot redirect, so a test fails
/// up front if one of them exists rather than producing a different snapshot.
fn with_config(
    program: &OsStr,
    directory: &Path,
    environment: &[(&str, &str)],
    arguments: &[&str],
) -> Output {
    for system in ["/etc/duit.conf", "/etc/ncdu.conf"] {
        assert!(
            !Path::new(system).exists(),
            "{system} exists and changes every run, move it aside to run the config tests"
        );
    }
    Command::new(program)
        .current_dir(directory)
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .envs(environment.iter().copied())
        .args(arguments)
        .output()
        .expect("program runs")
}

/// Run ncdu with `arguments` in `directory`, like [`duit`].
#[cfg(feature = "test-ncdu")]
fn ncdu(directory: &Path, arguments: &[&str]) -> Output {
    Command::new("ncdu")
        .current_dir(directory)
        .arg("--ignore-config")
        .args(arguments)
        .output()
        .expect("ncdu 2.9.2 is on PATH")
}

/// Assert that ncdu exits like duit and writes the same output, apart from the program name.
#[cfg(feature = "test-ncdu")]
fn assert_ncdu(directory: &Path, arguments: &[&str]) {
    assert_eq!(
        render_as_duit(&ncdu(directory, arguments)),
        render(&duit(directory, arguments)),
        "arguments: {arguments:?}"
    );
}

/// A run of ncdu as [`render`] renders it, with the program name in messages replaced by duit's.
#[cfg(feature = "test-ncdu")]
fn render_as_duit(output: &Output) -> String {
    render(output)
        .replace("ncdu <options>", "duit <options>")
        .replace("quitting ncdu", "quitting duit")
        .replace("see ncdu -h", "see duit -h")
}

/// The bytes of an output as text, with control characters other than newline and tab shown as
/// `cat -v` shows them, such as `^[` for the escape character, and the metadata block of an export
/// replaced, because it holds the program name, its version, and the time.
fn text(bytes: &[u8]) -> String {
    let mut text = String::new();
    for character in String::from_utf8_lossy(bytes).chars() {
        match u8::try_from(character) {
            Ok(byte @ (0..=0x08 | 0x0b..=0x1f)) => {
                text.push('^');
                text.push(char::from(byte + 0x40));
            }
            Ok(_) | Err(_) => text.push(character),
        }
    }
    if text.starts_with("[1,2,{")
        && let Some(end) = text.find('}')
    {
        text.replace_range(..=end, "[1,2,{[METADATA]}");
    }
    text
}

/// An export with the fields that differ between filesystems, users, and runs replaced. `dsize`
/// is removed, because an export leaves it out when it is 0, as it is for a directory on APFS.
fn redact(export: &str, root: &Path) -> String {
    let mut redacted = export.replace(&root.display().to_string(), "[ROOT]");
    for key in [
        "asize", "dsize", "dev", "ino", "mtime", "uid", "gid", "mode",
    ] {
        let pattern = format!(",\"{key}\":");
        let mut rest = redacted.as_str();
        let mut output = String::new();
        while let Some(start) = rest.find(&pattern) {
            output.push_str(&rest[..start]);
            if key != "dsize" {
                output.push_str(&pattern);
                output.push_str("[N]");
            }
            rest = rest[start + pattern.len()..]
                .trim_start_matches(|character: char| character.is_ascii_digit());
        }
        output.push_str(rest);
        redacted = output;
    }
    redacted
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

/// A binary export with one data block per entry of `blocks`, each holding the CBOR items of that
/// block, and an index block that ends with the reference of the root.
fn binary_file(blocks: &[&[u8]], root: u64) -> Vec<u8> {
    let frames: Vec<Vec<u8>> = blocks
        .iter()
        .map(|block| zstd::bulk::compress(block, 3).expect("block is compressed"))
        .collect();
    let frames: Vec<&[u8]> = frames.iter().map(Vec::as_slice).collect();
    binary_frames(&frames, root)
}

/// A binary export of data blocks that hold `frames`, as [`binary_file`] writes it.
fn binary_frames(frames: &[&[u8]], root: u64) -> Vec<u8> {
    let mut export = b"\xbfncduEX1".to_vec();
    let mut index = Vec::new();
    for (number, frame) in frames.iter().enumerate() {
        let header = u32::try_from(12 + frame.len())
            .expect("block fits")
            .to_be_bytes();
        let number = u32::try_from(number).expect("number fits").to_be_bytes();
        let block = [&header[..], &number, frame, &header].concat();
        let position = (u64::try_from(export.len()).expect("offset fits") << 24)
            | u64::try_from(block.len()).expect("length fits");
        index.extend_from_slice(&position.to_be_bytes());
        export.extend_from_slice(&block);
    }
    index.extend_from_slice(&root.to_be_bytes());
    [export, index_block(&index)].concat()
}

/// An index block that holds `entries`, the positions of the data blocks and the root reference.
fn index_block(entries: &[u8]) -> Vec<u8> {
    let length = u32::try_from(8 + entries.len()).expect("index fits");
    let header = (0x1000_0000 | length).to_be_bytes();
    [&header[..], entries, &header].concat()
}

/// A CBOR map of an item with `fields`, of indefinite length as ncdu writes it.
fn item(fields: &[u8]) -> Vec<u8> {
    [b"\xbf", fields, b"\xff"].concat()
}

/// A field `key` that refers to the first item of block `number`, as an unsigned integer.
fn reference(key: u8, number: u8) -> [u8; 6] {
    [key, 0x1a, number, 0, 0, 0]
}

/// The reference of the first item of block `number`.
fn block_start(number: u8) -> u64 {
    u64::from(number) << 24
}
