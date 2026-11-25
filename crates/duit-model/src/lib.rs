//! Entry types, the `Sink` trait, and the in-memory tree.

pub use mem_sink::{MemoryDirectory, MemorySink};
pub use mem_src::replay;
pub use progress::{Producer, Progress, ProgressSink};
pub use sink::Sink;
pub use stat::{EntryType, Extended, Special, Stat};
pub use tree::{
    BLOCKS_MAX, DeviceId, Directory, DirectoryId, Entry, EntryExtended, EntryId, EntryKind,
    FileKind, Link, LinkId, Tree, TreeError,
};

mod mem_sink;
mod mem_src;
mod progress;
mod sink;
mod stat;
mod tree;
