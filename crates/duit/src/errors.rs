//! ncdu's descriptions of system errors.
//!
//! ncdu turns an `errno` into a Zig error, and prints the Zig error with `ui.errorString`, which
//! describes some errors in words and prints the name of the others. The Zig errors of an `errno`
//! differ between system calls, so there is one function per call. The mappings are those of
//! Zig 0.15.

use std::io;

/// The description of an error of `fstatat`, which ncdu maps itself (`scan.zig`, `statAt`).
pub fn stat_error(error: &io::Error) -> &'static str {
    match error.raw_os_error() {
        Some(libc::ENOENT) => "No such file or directory",
        Some(libc::ENAMETOOLONG) => "Filename too long",
        Some(libc::ENOMEM) => "Out of memory",
        Some(libc::EACCES) => "Access denied",
        _ => "Unexpected",
    }
}

/// The description of an error of `openat` (`std.posix.openatZ`).
pub fn open_error(error: &io::Error) -> &'static str {
    match error.raw_os_error() {
        Some(libc::EINVAL) => "BadPathName",
        Some(libc::EACCES) => "Access denied",
        Some(libc::EFBIG | libc::EOVERFLOW) => "File too big",
        Some(libc::EISDIR) => "Is a directory",
        Some(libc::ELOOP) => "SymLinkLoop",
        Some(libc::EMFILE) => "Process file descriptor limit exceeded",
        Some(libc::ENAMETOOLONG) => "Filename too long",
        Some(libc::ENFILE) => "System file descriptor limit exceeded",
        Some(libc::ENODEV | libc::ENXIO) => "NoDevice",
        Some(libc::ENOENT) => "No such file or directory",
        Some(libc::ESRCH) => "ProcessNotFound",
        Some(libc::ENOMEM) => "Out of memory",
        Some(libc::ENOSPC) => "No space left on device",
        Some(libc::ENOTDIR) => "Not a directory",
        Some(libc::EPERM) => "PermissionDenied",
        Some(libc::EEXIST) => "PathAlreadyExists",
        Some(libc::EBUSY) => "DeviceBusy",
        Some(libc::EOPNOTSUPP) => "FileLocksNotSupported",
        Some(libc::EAGAIN) => "WouldBlock",
        Some(libc::ETXTBSY) => "File is busy",
        _ => "Unexpected",
    }
}

/// The description of an error of a seek to the end (`std.posix.lseek_END`).
pub fn seek_error(error: &io::Error) -> &'static str {
    match error.raw_os_error() {
        Some(libc::EINVAL | libc::EOVERFLOW | libc::ESPIPE | libc::ENXIO) => "Unseekable",
        _ => "Unexpected",
    }
}

/// The description of an error of `write` (`std.posix.write`).
pub fn write_error(error: &io::Error) -> &'static str {
    match error.raw_os_error() {
        Some(libc::EINVAL) => "InvalidArgument",
        Some(libc::ESRCH) => "ProcessNotFound",
        Some(libc::EAGAIN) => "WouldBlock",
        Some(libc::EBADF) => "NotOpenForWriting",
        Some(libc::EDQUOT) => "Disk quota exceeded",
        Some(libc::EFBIG) => "File too big",
        Some(libc::EIO) => "I/O error",
        Some(libc::ENOSPC) => "No space left on device",
        Some(libc::EACCES) => "Access denied",
        Some(libc::EPERM) => "PermissionDenied",
        Some(libc::EPIPE) => "BrokenPipe",
        Some(libc::ECONNRESET) => "ConnectionResetByPeer",
        Some(libc::EBUSY) => "DeviceBusy",
        Some(libc::ENXIO) => "NoDevice",
        Some(libc::EMSGSIZE) => "MessageTooBig",
        _ => "Unexpected",
    }
}

/// The description of an error of `read` (`std.posix.read`).
pub fn read_error(error: &io::Error) -> &'static str {
    match error.raw_os_error() {
        Some(libc::ESRCH) => "ProcessNotFound",
        Some(libc::EAGAIN) => "WouldBlock",
        Some(libc::ECANCELED) => "Canceled",
        Some(libc::EBADF) => "NotOpenForReading",
        Some(libc::EIO) => "I/O error",
        Some(libc::EISDIR) => "Is a directory",
        Some(libc::ENOBUFS | libc::ENOMEM) => "Out of memory",
        Some(libc::ENOTCONN) => "SocketNotConnected",
        Some(libc::ECONNRESET) => "ConnectionResetByPeer",
        Some(libc::ETIMEDOUT) => "ConnectionTimedOut",
        _ => "Unexpected",
    }
}
