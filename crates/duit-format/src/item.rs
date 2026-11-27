use std::io;

use duit_model::{EntryType, Sink, Special, Stat};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ItemType {
    Stat(EntryType),
    Special(Special),
}

pub(crate) fn add_entry<S: Sink>(
    sink: &mut S,
    parent: &mut S::Directory,
    kind: ItemType,
    name: &[u8],
    stat: &Stat,
    read_error: bool,
) -> io::Result<Option<S::Directory>> {
    match kind {
        ItemType::Stat(EntryType::Directory) => {
            let stat = Stat {
                entry_type: EntryType::Directory,
                ..*stat
            };
            let mut child = sink.add_directory(parent, name, &stat)?;
            if read_error {
                sink.set_read_error(&mut child)?;
            }
            Ok(Some(child))
        }
        ItemType::Stat(
            entry_type @ (EntryType::Regular | EntryType::NonRegular | EntryType::HardLink),
        ) => {
            let stat = Stat {
                entry_type,
                ..*stat
            };
            sink.add_stat(parent, name, &stat)?;
            Ok(None)
        }
        ItemType::Special(special) => {
            sink.add_special(parent, name, special)?;
            Ok(None)
        }
    }
}
