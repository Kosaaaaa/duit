//! Import arbitrary bytes as a JSON export, compressed with zstd when they start with its magic
//! number.

#![no_main]

use duit_format::import;
use duit_model::MemorySink;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // duit reads the first 8 bytes to tell a JSON export from a binary one.
    let Some((head, rest)) = data.split_first_chunk::<8>() else {
        return;
    };
    let mut sink = MemorySink::new(true);
    if import(&mut sink, rest, head).is_ok() {
        sink.finish(&mut |_, _| {});
    }
});
