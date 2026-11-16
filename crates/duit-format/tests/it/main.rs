//! Integration tests for duit's import and export formats.

#[cfg(feature = "test-ncdu")]
use std::io::Write;
#[cfg(feature = "test-ncdu")]
use std::process::{Command, Output, Stdio};
#[cfg(feature = "test-ncdu")]
use std::thread;

mod json_export;
mod json_import;

/// Run `ncdu -f- -o-` with `arguments` and `input` on standard input.
#[cfg(feature = "test-ncdu")]
fn ncdu_import(input: &[u8], arguments: &[&str]) -> Output {
    let mut child = Command::new("ncdu")
        .args(["--ignore-config", "-f-", "-o-"])
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("ncdu 2.9.2 is on PATH");
    let mut stdin = child.stdin.take().expect("stdin is piped");
    // A thread writes the input, so that ncdu never waits on a full output pipe. ncdu exits early
    // on malformed input, so the write may fail.
    thread::scope(|scope| {
        scope.spawn(move || {
            let _ = stdin.write_all(input);
        });
        child.wait_with_output().expect("ncdu exits")
    })
}
