//! The deletion of `delete.zig`: the confirmation, the deletion of an entry and of everything below
//! it from the filesystem and from the tree, and the windows that it shows while it runs.

use std::ffi::{CStr, OsStr};
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

/// The window of a deletion, `state` in `delete.zig`, or none when no deletion runs, which is
/// `main.state` in `main.zig`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    /// No deletion runs, or it was aborted.
    #[default]
    Closed,
    Confirm,
    /// The deletion deletes, and shows its progress.
    Busy,
    Error,
}

/// A button of the confirmation, `confirm` in `delete.zig`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Confirm {
    Yes,
    #[default]
    No,
    /// "don't ask me again".
    Ignore,
}

/// A button of the error window, `error_option` in `delete.zig`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum ErrorOption {
    #[default]
    Abort,
    Ignore,
    /// "ignore all".
    All,
}

/// What became of an entry that the deletion went through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Deleted,
    /// The entry is still on the filesystem and in the tree.
    Kept,
    /// The deletion was aborted before the entry was deleted.
    Aborted,
}

/// The deletion of the browser, `delete.zig`. The selected button of the error window and "ignore
/// all" stay as they are for the next deletion, as in ncdu.
#[derive(Debug, Default)]
pub struct Deletion {
    state: State,
    /// The directory and the entry that the windows name, `parent` and `entry` in `delete.zig`:
    /// the entry to delete, and while it is deleted, the entry that the deletion goes through.
    parent: Option<DirectoryId>,
    entry: Option<EntryId>,
    /// The entry to put the cursor on once the entry is deleted, `next_sel` in `delete.zig`.
    next: Option<EntryId>,
    confirm: Confirm,
    error_option: ErrorOption,
    /// "ignore all" was selected, `ignore_delete_errors` in `main.zig`.
    ignore_errors: bool,
    /// The description of the error that the error window shows.
    error: &'static str,
}

impl Deletion {
    /// Start to delete `entry` of `parent`, with `next` to select once it is deleted, `setup` in
    /// `delete.zig`. With `confirm`, the confirmation opens first.
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

    /// Handle `key` in the confirmation, `keyInput` in `delete.zig`. "don't ask me again" turns
    /// `confirm_delete` off. Returns [`Action::Delete`] once the deletion is confirmed.
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

    /// Delete the entry once the deletion is confirmed, `delete` in `delete.zig`, from the
    /// filesystem and from `tree`, and return the entry to put the cursor on: the entry when it is
    /// still in the tree, and else the entry after it, or the one before it when it was the last,
    /// or none. `events` gets the deletion before each entry, and with `true` whenever it waits for
    /// a key, to draw it and give it keys, as `handleEvent` in `main.zig`.
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
        tree.add_all_stats();
        self.state = State::Closed;
        match outcome {
            Outcome::Deleted => next,
            Outcome::Kept | Outcome::Aborted => Some(entry),
        }
    }

    /// Run `delete_command` on the entry once the deletion is confirmed instead of deleting it,
    /// `delete` and `deleteCmd` in `delete.zig`, and update `tree` with what the command left at
    /// the path of the entry. `run` runs the command, and returns the [`Stat`] of the path
    /// afterwards, or `None` when it cannot be read. With `extended`, a new entry keeps its
    /// extended information, as with `-e`.
    ///
    /// Returns the entry to put the cursor on, as [`Deletion::run`] does, and the directory at the
    /// path, which `main.zig` scans again next. Fails, as ncdu does, when a new directory is on a
    /// device that the tree cannot number.
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
        let mut path = entry_path(tree, parent, entry);
        path.push(0);
        // A name with a NUL byte, which only an import has, ends the path there, as for ncdu.
        let path = CStr::from_bytes_until_nul(&path).unwrap_or_default();
        // The path goes through the environment, so that the shell needs no quoting of it.
        let mut script = delete_command.to_vec();
        script.extend_from_slice(b" \"$NCDU_DELETE_PATH\"");
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(OsStr::from_bytes(&script))
            .env("NCDU_DELETE_PATH", OsStr::from_bytes(path.to_bytes()));
        // Nothing at the path, or a path that cannot be read, counts as deleted.
        let Some(stat) = run(&mut command, path) else {
            tree.remove(parent, previous, entry);
            tree.add_all_stats();
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
        tree.add_all_stats();
        let directory = match tree.entry(entry).kind {
            EntryKind::Directory(directory) => Some(directory),
            EntryKind::HardLink(_)
            | EntryKind::Regular
            | EntryKind::NonRegular
            | EntryKind::Special(_) => None,
        };
        Ok((Some(entry), directory))
    }

    /// Draw the confirmation over the screen in `buffer`, `drawConfirm` in `delete.zig`. With
    /// `delete_command`, it asks to run the command on the entry.
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
        // ncurses erases the screen to the background of the `default` style before each frame.
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
            // The browser draws the confirmation over the listing.
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

/// The path of `entry` of `parent`, from the root, as `delete` in `delete.zig` builds it.
fn entry_path(tree: &Tree, parent: DirectoryId, entry: EntryId) -> Vec<u8> {
    let mut path = Vec::new();
    tree.format_path(parent, true, &mut path);
    if path.last() != Some(&b'/') {
        path.push(b'/');
    }
    path.extend_from_slice(tree.name(entry));
    path
}

/// A deletion that runs, with the tree that it deletes from and the events of the main loop.
struct Run<'a> {
    deletion: &'a mut Deletion,
    tree: &'a mut Tree,
    events: &'a mut dyn FnMut(&mut Deletion, &Tree, bool),
}

impl Run<'_> {
    /// Give the deletion to the main loop, `handleEvent` in `main.zig`.
    fn handle_event(&mut self, block: bool) {
        (self.events)(self.deletion, self.tree, block);
    }

    /// Delete `id`, the entry at `path` from `directory`, and everything below it, and take it out
    /// of `parent`, where it follows `previous`, `deleteItem` in `delete.zig`.
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

    /// Delete the entries of the directory `data` at `path` from `directory`, then the directory
    /// itself. Returns the outcome unless the directory was deleted. The windows name the
    /// directory as the parent of the entries, also when the directory itself fails, as in ncdu.
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
            // An entry that was kept already had its error.
            Err(Errno::NOTEMPTY | Errno::EXIST) if kept.is_some() => Some(Outcome::Kept),
            Err(error) => Some(self.error(unlink_error(error))),
        }
    }

    /// Show `error` in the error window until a button closes it, `err` in `delete.zig`, unless
    /// "ignore all" was selected before.
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

/// The flags that open a directory to delete its entries, as Zig's `Dir.openDir` does with
/// `no_follow` and without `iterate`.
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
