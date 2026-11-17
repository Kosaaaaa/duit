use std::ffi::CStr;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsFd, BorrowedFd};

use duit_model::{BLOCKS_MAX, EntryType, Extended, Sink, Special, Stat};
use rustix::fs::{AtFlags, CWD, Dir, FileType, Mode, OFlags, openat, statat};

use crate::{DirectoryPatterns, ExcludePatterns, Excluded};

/// The options that change what a scan reports.
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
}

/// Why a [`scan`] failed. ncdu describes the `errno` of each step differently, so the caller needs
/// to know which step failed.
#[derive(Debug)]
pub enum ScanError {
    /// The root could not be stat'ed.
    Stat(io::Error),
    /// The root could not be opened as a directory.
    Open(io::Error),
    /// The sink failed. An error from `dirfd`, which does not happen for an open directory, ends
    /// up here too.
    Sink(io::Error),
}

/// A directory whose entries are being read.
struct Frame<'a, D> {
    entries: Dir,
    device: u64,
    patterns: DirectoryPatterns<'a>,
    sink: D,
}

/// Scan the directory at `path` and report its tree to `sink`, depth-first in the order of
/// `readdir`. The path is reported as given, so a caller that wants an absolute path resolves it
/// first.
///
/// Fails if the root cannot be read, before `sink` sees anything, or if `sink` fails. An entry that
/// cannot be read is reported as such.
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
    let root = sink
        .create_root(path.to_bytes(), &stat)
        .map_err(ScanError::Sink)?;
    let stack = vec![Frame {
        entries,
        device: stat.device,
        patterns: exclude.for_path(path),
        sink: root,
    }];
    walk(sink, stack, options).map_err(ScanError::Sink)
}

/// Read the directories on `stack` depth-first until it is empty.
fn walk<S: Sink>(
    sink: &mut S,
    mut stack: Vec<Frame<'_, S::Directory>>,
    options: ScanOptions,
) -> io::Result<()> {
    while let Some(frame) = stack.last_mut() {
        let entry = match frame.entries.read() {
            Some(Ok(entry)) => Some(entry),
            Some(Err(_)) => {
                // Like ncdu, a directory that fails to read ends at the error. Unlike ncdu, the
                // error marks that directory: ncdu marks the directory taken from the scan queue,
                // which with one thread is always the root.
                sink.set_read_error(&mut frame.sink)?;
                None
            }
            None => None,
        };
        if let Some(entry) = entry {
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

/// Report the entry `name` of `parent`, and return the frame of a directory to read next.
fn scan_entry<'a, S: Sink>(
    sink: &mut S,
    parent: &mut Frame<'a, S::Directory>,
    name: &CStr,
    options: ScanOptions,
) -> io::Result<Option<Frame<'a, S::Directory>>> {
    let bytes = name.to_bytes();
    let excluded = parent.patterns.matches(name);
    // A pattern that matches any entry excludes it before `stat`.
    if excluded == Excluded::Always {
        sink.add_special(&mut parent.sink, bytes, Special::Pattern)?;
        return Ok(None);
    }

    let parent_fd = parent.entries.fd()?;
    let Ok((mut stat, is_symlink)) = stat_at(parent_fd, name, false) else {
        sink.add_special(&mut parent.sink, bytes, Special::ReadError)?;
        return Ok(None);
    };
    if options.follow_symlinks
        && is_symlink
        && let Ok((target, _)) = stat_at(parent_fd, name, true)
        && target.entry_type != EntryType::Directory
    {
        stat = target;
        // Hard links are counted by device and inode, which a target on another filesystem
        // breaks.
        if stat.entry_type == EntryType::HardLink && stat.device != parent.device {
            stat.entry_type = EntryType::Regular;
            stat.link_count = 1;
        }
    }

    if options.same_filesystem && stat.device != parent.device {
        sink.add_special(&mut parent.sink, bytes, Special::OtherFilesystem)?;
        return Ok(None);
    }
    if stat.entry_type != EntryType::Directory {
        sink.add_stat(&mut parent.sink, bytes, &stat)?;
        return Ok(None);
    }
    if excluded == Excluded::IfDirectory {
        sink.add_special(&mut parent.sink, bytes, Special::Pattern)?;
        return Ok(None);
    }

    let Ok(entries) = openat(
        parent_fd,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .and_then(Dir::new) else {
        let mut directory = sink.add_directory(&mut parent.sink, bytes, &stat)?;
        sink.set_read_error(&mut directory)?;
        sink.finish_directory(directory, Some(&mut parent.sink))?;
        return Ok(None);
    };
    let fd = entries.fd()?;
    if options.exclude_kernfs && stat.device != parent.device && is_kernel_filesystem(fd) {
        sink.add_special(&mut parent.sink, bytes, Special::KernelFilesystem)?;
        return Ok(None);
    }
    if options.exclude_caches && is_cache_directory(fd) {
        sink.add_special(&mut parent.sink, bytes, Special::Pattern)?;
        return Ok(None);
    }

    let directory = sink.add_directory(&mut parent.sink, bytes, &stat)?;
    Ok(Some(Frame {
        entries,
        device: stat.device,
        patterns: parent.patterns.enter(name),
        sink: directory,
    }))
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
    let Ok(statfs) = rustix::fs::fstatfs(directory) else {
        return false;
    };
    let [byte0, byte1, byte2, byte3, ..] = statfs.f_type.to_le_bytes();
    matches!(
        u32::from_le_bytes([byte0, byte1, byte2, byte3]),
        0x4249_4e4d // BINFMTFS_MAGIC
            | 0xcafe_4a11 // BPF_FS_MAGIC
            | 0x0027_e0eb // CGROUP_SUPER_MAGIC
            | 0x6367_7270 // CGROUP2_SUPER_MAGIC
            | 0x6462_6720 // DEBUGFS_MAGIC
            | 0x1cd1 // DEVPTS_SUPER_MAGIC
            | 0x9fa0 // PROC_SUPER_MAGIC
            | 0x6165_676c // PSTOREFS_MAGIC
            | 0x7363_6673 // SECURITYFS_MAGIC
            | 0xf97c_ff8c // SELINUX_MAGIC
            | 0x6265_6572 // SYSFS_MAGIC
            | 0x7472_6163 // TRACEFS_MAGIC
    )
}

/// Pseudo filesystems are only detected on Linux, as in ncdu.
#[cfg(not(target_os = "linux"))]
fn is_kernel_filesystem(_directory: BorrowedFd<'_>) -> bool {
    false
}

/// The start of a `CACHEDIR.TAG`, from <https://bford.info/cachedir/>.
const CACHEDIR_SIGNATURE: &[u8; 43] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// Whether `directory` holds a `CACHEDIR.TAG` that starts with the signature.
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
