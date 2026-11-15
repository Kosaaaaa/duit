/// The type of an entry that has [`Stat`] data.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    Directory,
    /// A regular file with a link count of one.
    #[default]
    Regular,
    /// Anything that is neither a directory nor a regular file: a device, socket, or FIFO. A
    /// symlink that is not followed is one too.
    NonRegular,
    /// An entry that is not a directory and has a link count above one. Its size is shared with
    /// the other links to the same inode.
    HardLink,
}

/// An entry that is reported without [`Stat`] data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Special {
    /// The entry could not be read.
    ReadError,
    /// The entry matches an exclude pattern, or it is a cache directory that is excluded.
    Pattern,
    /// The entry is a directory on another filesystem, excluded with `-x`.
    OtherFilesystem,
    /// The entry is a directory on a pseudo filesystem such as `/proc`, excluded with
    /// `--exclude-kernfs`.
    KernelFilesystem,
}

/// The extended information that `-e` reads. Each field is `None` when it is unknown, for
/// example when an import does not contain it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Extended {
    /// The modification time in seconds since the Unix epoch.
    pub mtime: Option<u64>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    /// The file type and permission bits of `st_mode`.
    pub mode: Option<u16>,
}

/// The fields of an entry that a [`crate::Sink`] receives from a scan or an import.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    pub entry_type: EntryType,
    /// The disk usage in 512-byte blocks.
    pub blocks: u64,
    /// The apparent size in bytes.
    pub size: u64,
    pub device: u64,
    pub inode: u64,
    pub link_count: u32,
    pub extended: Extended,
}
