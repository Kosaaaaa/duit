use std::ffi::{CStr, CString, OsStr};
use std::num::NonZeroU8;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::process::Command;

use duit_model::{
    DirectoryId, EntryExtended, EntryId, EntryKind, EntryType, Stat, Tree, TreeError,
};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use rustix::fs::{AtFlags, CWD, FileType, Mode, OFlags, openat, statat, unlinkat};
use rustix::io::Errno;

use crate::browser::{Action, BrowserOptions};
use crate::terminal::Key;
use crate::ui::{ColorScheme, Style, Window, shorten, to_utf8};

const DELETE_PATH_VARIABLE: &str = "NCDU_DELETE_PATH";

fn until_nul(bytes: &[u8]) -> CString {
    let bytes: Vec<NonZeroU8> = bytes.iter().copied().map_while(NonZeroU8::new).collect();
    CString::from(bytes)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Closed,
    Confirm,
    Busy,
    Error,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Confirm {
    Yes,
    #[default]
    No,
    Ignore,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum ErrorOption {
    #[default]
    Abort,
    Ignore,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Deleted,
    Kept,
    Aborted,
}

/// The deletion of the browser, `delete.zig`. The selected button of the error window and "ignore
/// all" stay as they are for the next deletion, as in ncdu.
#[derive(Debug, Default)]
pub struct Deletion {
    state: State,
    parent: Option<DirectoryId>,
    entry: Option<EntryId>,
    next: Option<EntryId>,
    confirm: Confirm,
    error_option: ErrorOption,
    ignore_errors: bool,
    error: &'static str,
}

impl Deletion {
    pub(crate) fn setup(
        &mut self,
        parent: DirectoryId,
        entry: EntryId,
        next: Option<EntryId>,
        confirm: bool,
    ) {
        self.parent = Some(parent);
        self.entry = Some(entry);
        self.next = next;
        self.state = if confirm { State::Confirm } else { State::Busy };
        self.confirm = Confirm::No;
    }

    /// Whether the deletion runs, from its confirmation until it ends or is aborted.
    pub fn is_active(&self) -> bool {
        self.state != State::Closed
    }

    pub(crate) fn is_confirming(&self) -> bool {
        self.state == State::Confirm
    }

    /// Abort the deletion, as Ctrl-C does.
    pub fn abort(&mut self) {
        self.state = State::Closed;
    }

    pub(crate) fn confirm_key_input(&mut self, key: Key, confirm_delete: &mut bool) -> Action {
        match key {
            Key::Char('h') | Key::Left => {
                self.confirm = match self.confirm {
                    Confirm::Ignore => Confirm::No,
                    Confirm::Yes | Confirm::No => Confirm::Yes,
                };
            }
            Key::Char('l') | Key::Right => {
                self.confirm = match self.confirm {
                    Confirm::Yes => Confirm::No,
                    Confirm::No | Confirm::Ignore => Confirm::Ignore,
                };
            }
            Key::Char('q') => self.state = State::Closed,
            Key::Enter => match self.confirm {
                Confirm::Yes => self.state = State::Busy,
                Confirm::No => self.state = State::Closed,
                Confirm::Ignore => {
                    *confirm_delete = false;
                    self.state = State::Busy;
                }
            },
            Key::Char(_)
            | Key::Backspace
            | Key::Up
            | Key::Down
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown
            | Key::Other => {}
        }
        match self.state {
            State::Busy => Action::Delete,
            State::Closed | State::Confirm | State::Error => Action::Browse,
        }
    }

    /// Handle `key` while the deletion deletes or shows an error, `keyInput` in `delete.zig`.
    pub fn key_input(&mut self, key: Key) {
        match self.state {
            State::Closed | State::Confirm => {}
            State::Busy => {
                if key == Key::Char('q') {
                    self.state = State::Closed;
                }
            }
            State::Error => match key {
                Key::Char('h') | Key::Left => {
                    self.error_option = match self.error_option {
                        ErrorOption::All => ErrorOption::Ignore,
                        ErrorOption::Abort | ErrorOption::Ignore => ErrorOption::Abort,
                    };
                }
                Key::Char('l') | Key::Right => {
                    self.error_option = match self.error_option {
                        ErrorOption::Abort => ErrorOption::Ignore,
                        ErrorOption::Ignore | ErrorOption::All => ErrorOption::All,
                    };
                }
                Key::Char('q') => self.state = State::Closed,
                Key::Enter => match self.error_option {
                    ErrorOption::Abort => self.state = State::Closed,
                    ErrorOption::Ignore => self.state = State::Busy,
                    ErrorOption::All => {
                        self.ignore_errors = true;
                        self.state = State::Busy;
                    }
                },
                Key::Char(_)
                | Key::Backspace
                | Key::Up
                | Key::Down
                | Key::Home
                | Key::End
                | Key::PageUp
                | Key::PageDown
                | Key::Other => {}
            },
        }
    }

    pub(crate) fn run(
        &mut self,
        tree: &mut Tree,
        events: &mut dyn FnMut(&mut Self, &Tree, bool),
    ) -> Option<EntryId> {
        let (Some(parent), Some(entry)) = (self.parent, self.entry) else {
            return None;
        };
        if self.state != State::Busy {
            self.state = State::Closed;
            return Some(entry);
        }
        let previous = tree.entries(parent).take_while(|&id| id != entry).last();
        let path = entry_path(tree, parent, entry);
        let next = self.next;
        let mut run = Run {
            deletion: self,
            tree,
            events,
        };
        let outcome = run.delete_item(CWD, &path, parent, previous, entry);
        tree.add_all_stats(&mut |_, _| {});
        self.state = State::Closed;
        match outcome {
            Outcome::Deleted => next,
            Outcome::Kept | Outcome::Aborted => Some(entry),
        }
    }

    pub(crate) fn run_command(
        &mut self,
        tree: &mut Tree,
        delete_command: &[u8],
        extended: bool,
        run: &mut dyn FnMut(&mut Command, &CStr) -> Option<Stat>,
    ) -> Result<(Option<EntryId>, Option<DirectoryId>), TreeError> {
        let (Some(parent), Some(entry)) = (self.parent, self.entry) else {
            return Ok((None, None));
        };
        let state = self.state;
        self.state = State::Closed;
        if state != State::Busy {
            return Ok((Some(entry), None));
        }
        let previous = tree.entries(parent).take_while(|&id| id != entry).last();
        let path = until_nul(&entry_path(tree, parent, entry));
        let mut script = delete_command.to_vec();
        script.extend_from_slice(b" \"$");
        script.extend_from_slice(DELETE_PATH_VARIABLE.as_bytes());
        script.push(b'"');
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(OsStr::from_bytes(&script))
            .env(DELETE_PATH_VARIABLE, OsStr::from_bytes(path.to_bytes()));
        let Some(stat) = run(&mut command, &path) else {
            tree.remove(parent, previous, entry);
            tree.add_all_stats(&mut |_, _| {});
            return Ok((self.next, None));
        };
        let entry = match (tree.entry(entry).kind, stat.entry_type) {
            (EntryKind::Directory(_), EntryType::Directory) => entry,
            (
                EntryKind::Directory(_)
                | EntryKind::HardLink(_)
                | EntryKind::Regular
                | EntryKind::NonRegular
                | EntryKind::Special(_),
                EntryType::Directory
                | EntryType::Regular
                | EntryType::NonRegular
                | EntryType::HardLink,
            ) => tree.replace(
                parent,
                previous,
                entry,
                &stat,
                EntryExtended::of(&stat, extended),
            )?,
        };
        tree.add_all_stats(&mut |_, _| {});
        let directory = match tree.entry(entry).kind {
            EntryKind::Directory(directory) => Some(directory),
            EntryKind::HardLink(_)
            | EntryKind::Regular
            | EntryKind::NonRegular
            | EntryKind::Special(_) => None,
        };
        Ok((Some(entry), directory))
    }

    pub(crate) fn draw_confirm(
        &self,
        buffer: &mut Buffer,
        tree: &Tree,
        scheme: ColorScheme,
        delete_command: &[u8],
    ) {
        let Some(entry) = self.entry else {
            return;
        };
        let window = Window::new(buffer, scheme, 6, 60, "Confirm delete");
        let name = to_utf8(tree.name(entry));
        if delete_command.is_empty() {
            let x = window.text(
                buffer,
                1,
                2,
                "Are you sure you want to delete \"",
                Style::Default,
            );
            let x = window.text(buffer, 1, x, &shorten(&name, 21), Style::Default);
            let x = window.text(buffer, 1, x, "\"", Style::Default);
            match tree.entry(entry).kind {
                EntryKind::Directory(_) => {
                    window.text(buffer, 2, 18, "and all of its contents?", Style::Default);
                }
                EntryKind::HardLink(_)
                | EntryKind::Regular
                | EntryKind::NonRegular
                | EntryKind::Special(_) => {
                    window.text(buffer, 1, x, "?", Style::Default);
                }
            }
        } else {
            let x = window.text(
                buffer,
                1,
                2,
                "Are you sure you want to run \"",
                Style::Default,
            );
            let command = to_utf8(delete_command);
            let x = window.text(buffer, 1, x, &shorten(&command, 25), Style::Default);
            window.text(buffer, 1, x, "\"", Style::Default);
            let x = window.text(buffer, 2, 4, "on \"", Style::Default);
            let x = window.text(buffer, 2, x, &shorten(&name, 50), Style::Default);
            window.text(buffer, 2, x, "\"", Style::Default);
        }
        for (column, label, button) in [
            (15, "yes", Confirm::Yes),
            (25, "no", Confirm::No),
            (31, "don't ask me again", Confirm::Ignore),
        ] {
            let style = if self.confirm == button {
                Style::Sel
            } else {
                Style::Default
            };
            window.text(buffer, 4, column, label, style);
        }
    }

    /// Draw the progress or the error window of the deletion on all of `frame`, which is erased
    /// first, `draw` in `delete.zig`. The window names the entry by its path from below the root.
    pub fn draw(&self, frame: &mut Frame<'_>, tree: &Tree, options: &BrowserOptions) {
        let buffer = frame.buffer_mut();
        let scheme = options.color;
        buffer.set_style(buffer.area, scheme.style(Style::Default));
        let (Some(parent), Some(entry)) = (self.parent, self.entry) else {
            return;
        };
        let mut path = Vec::new();
        tree.format_path(parent, false, &mut path);
        path.push(b'/');
        path.extend_from_slice(tree.name(entry));
        let path = to_utf8(&path);
        match self.state {
            State::Closed | State::Confirm => {}
            State::Busy => {
                let window = Window::new(buffer, scheme, 6, 60, "Deleting...");
                window.text(buffer, 2, 2, &shorten(&path, 56), Style::Default);
                let x = window.text(buffer, 4, 41, "Press ", Style::Default);
                let x = window.text(buffer, 4, x, "q", Style::Key);
                window.text(buffer, 4, x, " to abort", Style::Default);
            }
            State::Error => {
                let window = Window::new(buffer, scheme, 6, 60, "Error");
                let x = window.text(buffer, 1, 2, "Error deleting ", Style::Default);
                window.text(buffer, 1, x, &shorten(&path, 41), Style::Default);
                window.text(buffer, 2, 4, self.error, Style::Default);
                for (column, label, button) in [
                    (14, "abort", ErrorOption::Abort),
                    (23, "ignore", ErrorOption::Ignore),
                    (33, "ignore all", ErrorOption::All),
                ] {
                    let style = if self.error_option == button {
                        Style::Sel
                    } else {
                        Style::Default
                    };
                    window.text(buffer, 4, column, label, style);
                }
            }
        }
    }
}

fn entry_path(tree: &Tree, parent: DirectoryId, entry: EntryId) -> Vec<u8> {
    let mut path = Vec::new();
    tree.format_path(parent, true, &mut path);
    if path.last() != Some(&b'/') {
        path.push(b'/');
    }
    path.extend_from_slice(tree.name(entry));
    path
}

struct Run<'a> {
    deletion: &'a mut Deletion,
    tree: &'a mut Tree,
    events: &'a mut dyn FnMut(&mut Deletion, &Tree, bool),
}

impl Run<'_> {
    fn handle_event(&mut self, block: bool) {
        (self.events)(self.deletion, self.tree, block);
    }

    fn delete_item(
        &mut self,
        directory: BorrowedFd<'_>,
        path: &[u8],
        parent: DirectoryId,
        previous: Option<EntryId>,
        id: EntryId,
    ) -> Outcome {
        self.deletion.entry = Some(id);
        self.handle_event(false);
        if self.deletion.state == State::Closed {
            return Outcome::Aborted;
        }
        if let EntryKind::Directory(data) = self.tree.entry(id).kind {
            let outcome = self.delete_directory(directory, path, data);
            self.deletion.parent = Some(parent);
            if let Some(outcome) = outcome {
                return outcome;
            }
        } else if let Err(error) = unlinkat(directory, path, AtFlags::empty()) {
            return self.error(unlink_file_error(directory, path, error));
        }
        self.tree.remove(parent, previous, id);
        Outcome::Deleted
    }

    fn delete_directory(
        &mut self,
        directory: BorrowedFd<'_>,
        path: &[u8],
        data: DirectoryId,
    ) -> Option<Outcome> {
        let entries = match openat(directory, path, open_flags(), Mode::empty()) {
            Ok(entries) => entries,
            Err(error) => return Some(self.error(open_error(error))),
        };
        self.deletion.parent = Some(data);
        let mut kept = None;
        for child in self.tree.entries(data).collect::<Vec<_>>() {
            let name = self.tree.name(child).to_vec();
            match self.delete_item(entries.as_fd(), &name, data, kept, child) {
                Outcome::Deleted => {}
                Outcome::Kept => kept = Some(child),
                Outcome::Aborted => return Some(Outcome::Aborted),
            }
        }
        drop(entries);
        match unlinkat(directory, path, AtFlags::REMOVEDIR) {
            Ok(()) => None,
            Err(Errno::NOTEMPTY | Errno::EXIST) if kept.is_some() => Some(Outcome::Kept),
            Err(error) => Some(self.error(unlink_error(error))),
        }
    }

    fn error(&mut self, error: &'static str) -> Outcome {
        if self.deletion.ignore_errors {
            return Outcome::Kept;
        }
        self.deletion.error = error;
        self.deletion.state = State::Error;
        while self.deletion.state == State::Error {
            self.handle_event(true);
        }
        if self.deletion.state == State::Closed {
            Outcome::Aborted
        } else {
            Outcome::Kept
        }
    }
}

/// `O_PATH`, which Zig's `Dir.openDir` adds without `iterate` where the system has it.
#[cfg(any(target_os = "linux", target_os = "android"))]
const PATH: OFlags = OFlags::PATH;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
const PATH: OFlags = OFlags::empty();

fn open_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | PATH
}

/// The description of an error of opening a directory (`std.posix.openatZ`), as Zig 0.15 maps the
/// errors and `ui.errorString` prints them.
fn open_error(error: Errno) -> &'static str {
    match error.raw_os_error() {
        libc::EINVAL => "BadPathName",
        libc::EACCES => "Access denied",
        libc::ELOOP => "SymLinkLoop",
        libc::EMFILE => "Process file descriptor limit exceeded",
        libc::ENAMETOOLONG => "Filename too long",
        libc::ENFILE => "System file descriptor limit exceeded",
        libc::ENODEV | libc::ENXIO => "NoDevice",
        libc::ENOENT => "No such file or directory",
        libc::ESRCH => "ProcessNotFound",
        libc::ENOMEM => "Out of memory",
        libc::ENOTDIR => "Not a directory",
        libc::EPERM => "PermissionDenied",
        libc::EBUSY => "DeviceBusy",
        _ => "Unexpected",
    }
}

/// The description of an error of `unlinkat` (`std.posix.unlinkatZ`).
fn unlink_error(error: Errno) -> &'static str {
    match error.raw_os_error() {
        libc::EACCES => "Access denied",
        libc::EPERM => "PermissionDenied",
        libc::EBUSY => "File is busy",
        libc::EIO => "I/O error",
        libc::EISDIR => "Is a directory",
        libc::ELOOP => "SymLinkLoop",
        libc::ENAMETOOLONG => "Filename too long",
        libc::ENOENT => "No such file or directory",
        libc::ENOTDIR => "Not a directory",
        libc::ENOMEM => "Out of memory",
        libc::EROFS => "ReadOnlyFileSystem",
        libc::EEXIST | libc::ENOTEMPTY => "Directory not empty",
        _ => "Unexpected",
    }
}

/// Whether unlinking a directory fails with a permission error, as on macOS and the BSDs, where
/// Zig's `Dir.deleteFile` then reports a directory as `Is a directory`.
const UNLINK_DIRECTORY_DENIED: bool = cfg!(any(
    target_vendor = "apple",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "dragonfly",
    target_os = "openbsd",
    target_os = "solaris",
    target_os = "illumos",
));

/// The description of an error of unlinking the file `path` from `directory` (`Dir.deleteFile`).
fn unlink_file_error(directory: BorrowedFd<'_>, path: &[u8], error: Errno) -> &'static str {
    if UNLINK_DIRECTORY_DENIED
        && (error == Errno::ACCESS || error == Errno::PERM)
        && statat(directory, path, AtFlags::SYMLINK_NOFOLLOW)
            .is_ok_and(|stat| FileType::from_raw_mode(stat.st_mode) == FileType::Directory)
    {
        return "Is a directory";
    }
    unlink_error(error)
}
