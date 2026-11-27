use std::ffi::CStr;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsFd, BorrowedFd};
use std::sync::Arc;

use duit_model::{
    BLOCKS_MAX, DirectoryPath, EntryType, Extended, Sink, Special, Stat, ThreadDirectories,
};
use rustix::fs::{AtFlags, CWD, Dir, DirEntry, FileType, Mode, OFlags, openat, statat};

use crate::{DirectoryPatterns, ExcludePatterns, Excluded, threads};

#[derive(Debug, Default, Clone, Copy)]
pub struct ScanOptions {
    /// `-x`: Report directories on another filesystem than their parent as excluded.
    pub same_filesystem: bool,
    /// `-L`: Report the target of a symlink, unless it is a directory.
    pub follow_symlinks: bool,
    /// `--exclude-kernfs`: Report directories on Linux pseudo filesystems as excluded. Other
    /// platforms ignore it.
    pub exclude_kernfs: bool,
    /// `--exclude-caches`: Report directories with a valid `CACHEDIR.TAG` as excluded.
    pub exclude_caches: bool,
    /// `-t`: The number of threads that read directories. With 0 or 1, the calling thread reads
    /// them.
    pub threads: usize,
}

/// Why a [`scan`] failed. ncdu describes the `errno` of each step differently, so the caller needs
/// to know which step failed.
#[derive(Debug)]
pub enum ScanError {
    /// The root could not be stat'ed.
    Stat(io::Error),
    /// The root could not be opened as a directory.
    Open(io::Error),
    /// A thread to read directories could not be started.
    Spawn(io::Error),
    /// The sink failed. An error from `dirfd`, which does not happen for an open directory, ends
    /// up here too.
    Sink(io::Error),
}

pub(crate) struct Frame<'a, D> {
    pub(crate) entries: Dir,
    pub(crate) device: u64,
    pub(crate) patterns: DirectoryPatterns<'a>,
    pub(crate) sink: D,
}

/// Scan the directory at `path` and report its tree to `sink`, in the order of `readdir`. The path
/// is reported as given, so a caller that wants an absolute path resolves it first.
///
/// With one thread, the tree is reported depth-first. With more, as `-t` in ncdu, threads read
/// directories while the calling thread reports them, so the entries of a directory follow in the
/// order of `readdir`, in parts, while other directories that are not finished yet may get entries
/// in between. Each directory is finished once its entries and its subdirectories are. Only a sink that keeps every
/// directory apart, such as the tree in memory, takes such a scan.
///
/// Fails if the root cannot be read, before `sink` sees anything, if a thread cannot be started,
/// or if `sink` fails. An entry that cannot be read is reported as such.
pub fn scan<S: Sink>(
    sink: &mut S,
    path: &CStr,
    options: ScanOptions,
    exclude: &ExcludePatterns,
) -> Result<(), ScanError> {
    let (stat, _) = stat_at(CWD, path, true).map_err(ScanError::Stat)?;
    let entries = openat(
        CWD,
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .and_then(Dir::new)
    .map_err(|error| ScanError::Open(error.into()))?;
    let threads = (options.threads > 1).then(|| {
        let threads = Arc::new(ThreadDirectories::new(options.threads));
        sink.threads(Arc::clone(&threads));
        threads
    });
    let root = sink
        .create_root(path.to_bytes(), &stat)
        .map_err(ScanError::Sink)?;
    let frame = Frame {
        entries,
        device: stat.device,
        patterns: exclude.for_path(path),
        sink: root,
    };
    if let Some(threads) = threads {
        let root = DirectoryPath::root(path.to_bytes());
        return threads::walk(sink, frame, root, &threads, options);
    }
    walk(sink, vec![frame], options).map_err(ScanError::Sink)
}

fn walk<S: Sink>(
    sink: &mut S,
    mut stack: Vec<Frame<'_, S::Directory>>,
    options: ScanOptions,
) -> io::Result<()> {
    while let Some(frame) = stack.last_mut() {
        if let Some(entry) = next_entry_or_mark_directory(sink, frame)? {
            let name = entry.file_name();
            if name != c"."
                && name != c".."
                && let Some(child) = scan_entry(sink, frame, name, options)?
            {
                stack.push(child);
            }
        } else if let Some(frame) = stack.pop() {
            sink.finish_directory(frame.sink, stack.last_mut().map(|parent| &mut parent.sink))?;
        }
    }
    Ok(())
}

fn next_entry_or_mark_directory<S: Sink>(
    sink: &mut S,
    directory: &mut Frame<'_, S::Directory>,
) -> io::Result<Option<DirEntry>> {
    match directory.entries.read() {
        Some(Ok(entry)) => Ok(Some(entry)),
        Some(Err(_)) => {
            sink.set_read_error(&mut directory.sink)?;
            Ok(None)
        }
        None => Ok(None),
    }
}

fn scan_entry<'a, S: Sink>(
    sink: &mut S,
    parent: &mut Frame<'a, S::Directory>,
    name: &CStr,
    options: ScanOptions,
) -> io::Result<Option<Frame<'a, S::Directory>>> {
    let bytes = name.to_bytes();
    let parent_fd = parent.entries.fd()?;
    match classify(parent_fd, parent.device, &parent.patterns, name, options)? {
        Entry::Special(special) => sink.add_special(&mut parent.sink, bytes, special)?,
        Entry::Stat(stat) => sink.add_stat(&mut parent.sink, bytes, &stat)?,
        Entry::Unreadable(stat) => add_unreadable(sink, &mut parent.sink, bytes, &stat)?,
        Entry::Directory {
            stat,
            patterns,
            entries,
        } => match entries.map_or_else(|| open_directory(parent_fd, name), Ok) {
            Ok(entries) => {
                let directory = sink.add_directory(&mut parent.sink, bytes, &stat)?;
                return Ok(Some(Frame {
                    entries,
                    device: stat.device,
                    patterns,
                    sink: directory,
                }));
            }
            Err(_) => add_unreadable(sink, &mut parent.sink, bytes, &stat)?,
        },
    }
    Ok(None)
}

pub(crate) enum Entry<'a> {
    Special(Special),
    Stat(Stat),
    Unreadable(Stat),
    Directory {
        stat: Stat,
        patterns: DirectoryPatterns<'a>,
        entries: Option<Dir>,
    },
}

pub(crate) fn classify<'a>(
    parent: BorrowedFd<'_>,
    device: u64,
    patterns: &DirectoryPatterns<'a>,
    name: &CStr,
    options: ScanOptions,
) -> io::Result<Entry<'a>> {
    let excluded = patterns.matches(name);
    if excluded == Excluded::Always {
        return Ok(Entry::Special(Special::Pattern));
    }

    let Ok((mut stat, is_symlink)) = stat_at(parent, name, false) else {
        return Ok(Entry::Special(Special::ReadError));
    };
    if options.follow_symlinks
        && is_symlink
        && let Ok((target, _)) = stat_at(parent, name, true)
        && target.entry_type != EntryType::Directory
    {
        stat = without_hard_link_across_devices(target, device);
    }

    if options.same_filesystem && stat.device != device {
        return Ok(Entry::Special(Special::OtherFilesystem));
    }
    if stat.entry_type != EntryType::Directory {
        return Ok(Entry::Stat(stat));
    }
    if excluded == Excluded::IfDirectory {
        return Ok(Entry::Special(Special::Pattern));
    }

    let check_kernfs = options.exclude_kernfs && stat.device != device;
    let entries = if check_kernfs || options.exclude_caches {
        let Ok(entries) = open_directory(parent, name) else {
            return Ok(Entry::Unreadable(stat));
        };
        let fd = entries.fd()?;
        if check_kernfs && is_kernel_filesystem(fd) {
            return Ok(Entry::Special(Special::KernelFilesystem));
        }
        if options.exclude_caches && is_cache_directory(fd) {
            return Ok(Entry::Special(Special::Pattern));
        }
        Some(entries)
    } else {
        None
    };
    Ok(Entry::Directory {
        stat,
        patterns: patterns.enter(name),
        entries,
    })
}

fn without_hard_link_across_devices(mut target: Stat, parent_device: u64) -> Stat {
    if target.entry_type == EntryType::HardLink && target.device != parent_device {
        target.entry_type = EntryType::Regular;
        target.link_count = 1;
    }
    target
}

pub(crate) fn open_directory(parent: BorrowedFd<'_>, name: &CStr) -> rustix::io::Result<Dir> {
    openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .and_then(Dir::new)
}

pub(crate) fn add_unreadable<S: Sink>(
    sink: &mut S,
    parent: &mut S::Directory,
    name: &[u8],
    stat: &Stat,
) -> io::Result<()> {
    let mut directory = sink.add_directory(parent, name, stat)?;
    sink.set_read_error(&mut directory)?;
    sink.finish_directory(directory, Some(parent))
}

/// The largest link count that ncdu stores, in 31 bits.
const LINK_COUNT_MAX: u32 = (1 << 31) - 1;

/// The [`Stat`] of the entry `name` in `directory`, and whether the entry is a symlink, `statAt` in
/// `scan.zig`. With `follow`, a symlink is followed.
pub fn stat_at(directory: impl AsFd, name: &CStr, follow: bool) -> io::Result<(Stat, bool)> {
    let flags = if follow {
        AtFlags::empty()
    } else {
        AtFlags::SYMLINK_NOFOLLOW
    };
    let stat = statat(directory, name, flags)?;
    let file_type = FileType::from_raw_mode(stat.st_mode);
    let link_count = clamp(stat.st_nlink, u64::from(LINK_COUNT_MAX));
    let entry_type = if file_type == FileType::Directory {
        EntryType::Directory
    } else if link_count > 1 {
        EntryType::HardLink
    } else if file_type == FileType::RegularFile {
        EntryType::Regular
    } else {
        EntryType::NonRegular
    };
    let [mode_low, mode_high, ..] = stat.st_mode.to_le_bytes();
    let reported = Stat {
        entry_type,
        blocks: clamp(stat.st_blocks, BLOCKS_MAX),
        size: clamp(stat.st_size, u64::MAX),
        device: device(&stat),
        inode: stat.st_ino,
        link_count: u32::try_from(link_count).unwrap_or(LINK_COUNT_MAX),
        extended: Extended {
            mtime: Some(clamp(stat.st_mtime, u64::MAX)),
            uid: Some(stat.st_uid),
            gid: Some(stat.st_gid),
            mode: Some(u16::from_le_bytes([mode_low, mode_high])),
        },
    };
    Ok((reported, file_type == FileType::Symlink))
}

/// Limit a `stat` field to `0..=max`. The field types differ between platforms.
fn clamp(value: impl TryInto<u64>, max: u64) -> u64 {
    value.try_into().map_or(0, |value| value.min(max))
}

/// The device of `stat`, with the bits of `st_dev` read as unsigned. `dev_t` is a signed 32-bit
/// integer on macOS, which ncdu does not sign-extend.
fn device(stat: &rustix::fs::Stat) -> u64 {
    let raw = stat.st_dev.to_le_bytes();
    let mut bytes = [0; 8];
    bytes[..raw.len()].copy_from_slice(&raw);
    u64::from_le_bytes(bytes)
}

/// Whether `directory` is on a Linux pseudo filesystem, by the magic numbers that `statfs(2)`
/// documents.
#[cfg(target_os = "linux")]
fn is_kernel_filesystem(directory: BorrowedFd<'_>) -> bool {
    const BINFMTFS_MAGIC: u32 = 0x4249_4e4d;
    const BPF_FS_MAGIC: u32 = 0xcafe_4a11;
    const CGROUP_SUPER_MAGIC: u32 = 0x0027_e0eb;
    const CGROUP2_SUPER_MAGIC: u32 = 0x6367_7270;
    const DEBUGFS_MAGIC: u32 = 0x6462_6720;
    const DEVPTS_SUPER_MAGIC: u32 = 0x1cd1;
    const PROC_SUPER_MAGIC: u32 = 0x9fa0;
    const PSTOREFS_MAGIC: u32 = 0x6165_676c;
    const SECURITYFS_MAGIC: u32 = 0x7363_6673;
    const SELINUX_MAGIC: u32 = 0xf97c_ff8c;
    const SYSFS_MAGIC: u32 = 0x6265_6572;
    const TRACEFS_MAGIC: u32 = 0x7472_6163;

    let Ok(statfs) = rustix::fs::fstatfs(directory) else {
        return false;
    };
    let [byte0, byte1, byte2, byte3, ..] = statfs.f_type.to_le_bytes();
    matches!(
        u32::from_le_bytes([byte0, byte1, byte2, byte3]),
        BINFMTFS_MAGIC
            | BPF_FS_MAGIC
            | CGROUP_SUPER_MAGIC
            | CGROUP2_SUPER_MAGIC
            | DEBUGFS_MAGIC
            | DEVPTS_SUPER_MAGIC
            | PROC_SUPER_MAGIC
            | PSTOREFS_MAGIC
            | SECURITYFS_MAGIC
            | SELINUX_MAGIC
            | SYSFS_MAGIC
            | TRACEFS_MAGIC
    )
}

#[cfg(not(target_os = "linux"))]
fn is_kernel_filesystem(_directory: BorrowedFd<'_>) -> bool {
    false
}

/// The start of a `CACHEDIR.TAG`, from <https://bford.info/cachedir/>.
const CACHEDIR_SIGNATURE: &[u8; 43] = b"Signature: 8a477f597d28d172789f06886806bc55";

fn is_cache_directory(directory: BorrowedFd<'_>) -> bool {
    let Ok(tag) = openat(
        directory,
        c"CACHEDIR.TAG",
        OFlags::RDONLY | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    ) else {
        return false;
    };
    let mut buffer = [0; CACHEDIR_SIGNATURE.len()];
    File::from(tag).read_exact(&mut buffer).is_ok() && &buffer == CACHEDIR_SIGNATURE
}
