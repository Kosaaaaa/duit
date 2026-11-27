#![no_main]

use std::collections::HashSet;
use std::io::Cursor;

use duit_format::BinaryReader;
use duit_model::MemorySink;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(mut reader) = BinaryReader::open(Cursor::new(data)) else {
        return;
    };
    let mut sink = MemorySink::new(true);
    if reader.import(&mut sink).is_ok() {
        sink.finish(&mut |_, _| {});
    }
    let mut directories = vec![reader.root()];
    let mut visited = HashSet::new();
    while let Some(itemref) = directories.pop() {
        if !visited.insert(itemref) {
            continue;
        }
        if let Ok(Some(listing)) = reader.read_directory(itemref, true, &mut |_, _| {}) {
            directories.extend(listing.directories.iter().map(|listed| listed.itemref));
        }
    }
});
