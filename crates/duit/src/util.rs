use std::borrow::Cow;
use std::env;
use std::ffi::{CStr, CString, OsString};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStringExt;
use std::ptr;

pub(crate) fn until_nul(bytes: &[u8]) -> &[u8] {
    bytes.split(|&byte| byte == 0).next().unwrap_or_default()
}

#[derive(Clone, Copy)]
enum User<'a> {
    Current,
    Name(&'a [u8]),
}

pub(crate) fn expand_user(path: &[u8]) -> Cow<'_, [u8]> {
    let Some(rest) = path.strip_prefix(b"~") else {
        return Cow::Borrowed(path);
    };
    let (name, tail) = rest.split_at(
        rest.iter()
            .position(|&byte| byte == b'/')
            .unwrap_or(rest.len()),
    );
    let home = if name.is_empty() {
        env::var_os("HOME")
            .map(OsString::into_vec)
            .or_else(|| home_directory(User::Current))
    } else {
        home_directory(User::Name(name))
    };
    let Some(home) = home else {
        return Cow::Borrowed(path);
    };
    let home = &home[..home
        .iter()
        .rposition(|&byte| byte != b'/')
        .map_or(0, |last| last + 1)];
    if home.is_empty() && tail.is_empty() {
        return Cow::Borrowed(b"/");
    }
    Cow::Owned([home, tail].concat())
}

const PASSWORD_BUFFER_LIMIT: usize = 1 << 20;

#[expect(
    unsafe_code,
    reason = "getpwuid_r and getpwnam_r are foreign functions"
)]
fn home_directory(user: User<'_>) -> Option<Vec<u8>> {
    let name = match user {
        User::Current => None,
        User::Name(name) => Some(CString::new(until_nul(name)).ok()?),
    };
    let mut buffer: Vec<libc::c_char> = vec![0; 1024];
    loop {
        let mut entry = MaybeUninit::<libc::passwd>::uninit();
        let mut result = ptr::null_mut();
        // SAFETY: `entry` and `result` are valid for writes, `buffer` is valid for writes of
        // `buffer.len()` bytes, and `name` is a NUL-terminated string. All of them outlive the
        // call.
        let status = unsafe {
            match &name {
                None => libc::getpwuid_r(
                    libc::getuid(),
                    entry.as_mut_ptr(),
                    buffer.as_mut_ptr(),
                    buffer.len(),
                    &raw mut result,
                ),
                Some(name) => libc::getpwnam_r(
                    name.as_ptr(),
                    entry.as_mut_ptr(),
                    buffer.as_mut_ptr(),
                    buffer.len(),
                    &raw mut result,
                ),
            }
        };
        if status == libc::ERANGE && buffer.len() < PASSWORD_BUFFER_LIMIT {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if status != 0 || result.is_null() {
            return None;
        }
        // SAFETY: On success, `result` points to `entry`, which the call initialized, and whose
        // `pw_dir` is null or points to a NUL-terminated string in `buffer`, which is alive.
        let directory = unsafe { (*result).pw_dir };
        if directory.is_null() {
            return None;
        }
        // SAFETY: As above, `directory` points to a NUL-terminated string in `buffer`.
        return Some(unsafe { CStr::from_ptr(directory) }.to_bytes().to_vec());
    }
}

const LINE_BUFFER: usize = 4096;

pub(crate) struct LineReader {
    reader: BufReader<File>,
    line: Vec<u8>,
}

impl LineReader {
    pub(crate) fn new(file: File) -> Self {
        Self {
            reader: BufReader::new(file),
            line: Vec::new(),
        }
    }

    pub(crate) fn next_line(&mut self) -> Result<Option<&[u8]>, &'static str> {
        self.line.clear();
        // Zig's reader reports any read error as `ReadFailed`.
        (&mut self.reader)
            .take(LINE_BUFFER as u64)
            .read_until(b'\n', &mut self.line)
            .map_err(|_| "ReadFailed")?;
        if self.line.is_empty() {
            return Ok(None);
        }
        if self.line.pop_if(|byte| *byte == b'\n').is_none() && self.line.len() == LINE_BUFFER {
            return Err("StreamTooLong");
        }
        Ok(Some(&self.line))
    }
}
