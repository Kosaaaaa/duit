#![no_main]

use duit_format::{BINARY_SIGNATURE, import};
use duit_model::MemorySink;
use libfuzzer_sys::fuzz_target;

const HEAD_LENGTH: usize = BINARY_SIGNATURE.len();

fuzz_target!(|data: &[u8]| {
    let Some((head, rest)) = data.split_first_chunk::<HEAD_LENGTH>() else {
        return;
    };
    let mut sink = MemorySink::new(true);
    if import(&mut sink, rest, head).is_ok() {
        sink.finish(&mut |_, _| {});
    }
});
