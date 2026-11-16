use std::io;

use duit_model::{EntryType, Sink, Special, Stat};

/// The type of an imported item as its fields set it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ItemType {
    Stat(EntryType),
    Special(Special),
}

/// Add the imported item `name` of type `kind` to `parent` in `sink`, and return it if it is a
/// directory, whose entries the caller reads next. The type is copied into `stat`, which the
/// sink reads it from.
pub(crate) fn add_entry<S: Sink>(
    sink: &mut S,
    parent: &mut S::Directory,
    kind: ItemType,
    name: &[u8],
    stat: &mut Stat,
    read_error: bool,
) -> io::Result<Option<S::Directory>> {
    match kind {
        ItemType::Stat(EntryType::Directory) => {
            stat.entry_type = EntryType::Directory;
            let mut child = sink.add_directory(parent, name, stat)?;
            if read_error {
                sink.set_read_error(&mut child)?;
            }
            Ok(Some(child))
        }
        ItemType::Stat(
            entry_type @ (EntryType::Regular | EntryType::NonRegular | EntryType::HardLink),
        ) => {
            stat.entry_type = entry_type;
            sink.add_stat(parent, name, stat)?;
            Ok(None)
        }
        ItemType::Special(special) => {
            sink.add_special(parent, name, special)?;
            Ok(None)
        }
    }
}
