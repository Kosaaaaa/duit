use std::io;

use crate::tree::{DirectoryId, EntryExtended, EntryId, EntryKind, Tree};
use crate::{EntryType, Extended, Sink, Stat};

/// Give `directory` of `tree` as the root to `sink`, and everything below it depth-first, `run`
/// in `mem_src.zig`. The entries of a directory are given in the order of the tree, the last
/// added first. The read error of `directory` itself is not given, as in ncdu.
///
/// A tree of [`crate::MemorySink::without_stats`] gives each entry the sizes it was added with,
/// as a JSON export of a scan with several threads needs.
pub fn replay<S: Sink>(tree: &Tree, directory: DirectoryId, sink: &mut S) -> io::Result<()> {
    let mut path = Vec::new();
    tree.format_path(directory, true, &mut path);
    let stat = to_stat(tree, tree.directory(directory).entry, EntryType::Directory);
    let root = sink.create_root(&path, &stat)?;
    let mut directories = vec![(root, tree.entries(directory))];
    while let Some((parent, entries)) = directories.last_mut() {
        let Some(id) = entries.next() else {
            if let Some((finished, _)) = directories.pop() {
                let parent = directories.last_mut().map(|(parent, _)| parent);
                sink.finish_directory(finished, parent)?;
            }
            continue;
        };
        let name = tree.name(id);
        let entry_type = match tree.entry(id).kind {
            EntryKind::Directory(child) => {
                let stat = to_stat(tree, id, EntryType::Directory);
                let mut handle = sink.add_directory(parent, name, &stat)?;
                if tree.directory(child).read_error {
                    sink.set_read_error(&mut handle)?;
                }
                directories.push((handle, tree.entries(child)));
                continue;
            }
            EntryKind::Special(special) => {
                sink.add_special(parent, name, special)?;
                continue;
            }
            EntryKind::HardLink(_) => EntryType::HardLink,
            EntryKind::Regular => EntryType::Regular,
            EntryKind::NonRegular => EntryType::NonRegular,
        };
        sink.add_stat(parent, name, &to_stat(tree, id, entry_type))?;
    }
    Ok(())
}

fn to_stat(tree: &Tree, id: EntryId, entry_type: EntryType) -> Stat {
    let entry = tree.entry(id);
    let (device, inode, link_count) = match entry.kind {
        EntryKind::Directory(directory) => (tree.device(tree.directory(directory).device), 0, 1),
        EntryKind::HardLink(link) => {
            let link = tree.link(link);
            let device = tree.device(tree.directory(link.parent).device);
            (device, link.inode(), link.link_count())
        }
        EntryKind::Regular | EntryKind::NonRegular | EntryKind::Special(_) => (0, 0, 1),
    };
    let extended = tree.extended(id).map(with_latest_mtime).unwrap_or_default();
    Stat {
        entry_type,
        blocks: entry.blocks,
        size: entry.size,
        device,
        inode,
        link_count,
        extended,
    }
}

fn with_latest_mtime(entry_extended: &EntryExtended) -> Extended {
    Extended {
        mtime: entry_extended
            .extended
            .mtime
            .map(|_| entry_extended.latest_mtime),
        ..entry_extended.extended
    }
}
