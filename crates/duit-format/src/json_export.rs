use std::io::{self, BufWriter, IntoInnerError, Write};

use duit_model::{EntryType, Sink, Special, Stat};
use zstd::stream::write::Encoder;

/// The name written as `progname` in the metadata block.
const PROGRAM_NAME: &str = "duit";

/// A [`Sink`] that writes the ncdu JSON export format.
///
/// Entries must arrive depth-first, which every single-threaded producer does. Names are written
/// as raw bytes, except for the bytes that JSON requires to be escaped, so an export of a name that
/// is not UTF-8 is not valid JSON. ncdu writes the same bytes and reads them back.
pub struct JsonExport<W: Write> {
    out: BufWriter<Output<W>>,
    extended: bool,
    /// Whether the object of the last directory still lacks its closing `}`, so that a read error
    /// can be added to it. A read error after the first entry of a directory has no place in the
    /// format and is dropped.
    directory_open: bool,
}

/// The writer under the buffer of a [`JsonExport`].
enum Output<W: Write> {
    Plain(W),
    /// One zstd frame, which [`Output::finish`] ends.
    Zstd(Encoder<'static, W>),
}

impl<W: Write> Output<W> {
    fn finish(self) -> io::Result<W> {
        match self {
            Self::Plain(out) => Ok(out),
            Self::Zstd(encoder) => encoder.finish(),
        }
    }
}

impl<W: Write> Write for Output<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(out) => out.write(buf),
            Self::Zstd(encoder) => encoder.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(out) => out.flush(),
            Self::Zstd(encoder) => encoder.flush(),
        }
    }
}

/// The handle of a directory in a [`JsonExport`].
pub struct JsonDirectory {
    device: u64,
}

impl<W: Write> JsonExport<W> {
    /// Start an export to `out` and write the metadata block. `timestamp` is the time of the export
    /// in seconds since the Unix epoch. With `extended`, the fields of
    /// [`duit_model::Extended`] are written too. With `compress_level`, the export is compressed
    /// with zstd at that level.
    pub fn new(
        out: W,
        timestamp: u64,
        extended: bool,
        compress_level: Option<u8>,
    ) -> io::Result<Self> {
        let out = match compress_level {
            Some(level) => Output::Zstd(Encoder::new(out, i32::from(level))?),
            None => Output::Plain(out),
        };
        let mut out = BufWriter::with_capacity(64 * 1024, out);
        write!(
            out,
            "[1,2,{{\"progname\":\"{PROGRAM_NAME}\",\"progver\":\"{}\",\"timestamp\":{timestamp}}}",
            env!("CARGO_PKG_VERSION")
        )?;
        Ok(Self {
            out,
            extended,
            directory_open: false,
        })
    }

    /// End the export after the root is finished, flush it, and return the writer.
    pub fn finish(mut self) -> io::Result<W> {
        self.out.write_all(b"]\n")?;
        let mut out = self
            .out
            .into_inner()
            .map_err(IntoInnerError::into_error)?
            .finish()?;
        // A writer such as standard output buffers too, and the end of a compressed export need not
        // hold the newline that flushes it. Flushed later, at exit, a write error would be lost.
        out.flush()?;
        Ok(out)
    }

    /// Drop the export without writing what is still buffered, as ncdu does when it stops on an
    /// error.
    pub fn discard(self) {
        drop(self.out.into_parts());
    }

    fn close_directory_entry(&mut self, read_error: bool) -> io::Result<()> {
        if self.directory_open {
            self.directory_open = false;
            if read_error {
                self.out.write_all(b",\"read_error\":true")?;
            }
            self.out.write_all(b"}")?;
        }
        Ok(())
    }

    /// Start the object of an entry and write its name. A directory is wrapped in an array that
    /// holds the object followed by its entries.
    fn write_entry_name(&mut self, is_directory: bool, name: &[u8]) -> io::Result<()> {
        self.out.write_all(if is_directory {
            b",\n[{\"name\":\""
        } else {
            b",\n{\"name\":\""
        })?;
        self.write_name(name)?;
        self.out.write_all(b"\"")
    }

    /// Write the bytes of `name`, escaped as a JSON string without the quotes.
    fn write_name(&mut self, name: &[u8]) -> io::Result<()> {
        const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut start = 0;
        for (index, &byte) in name.iter().enumerate() {
            let mut unicode = *b"\\u0000";
            let escaped: &[u8] = match byte {
                b'\n' => b"\\n",
                b'\r' => b"\\r",
                0x08 => b"\\b",
                b'\t' => b"\\t",
                0x0c => b"\\f",
                b'\\' => b"\\\\",
                b'"' => b"\\\"",
                0x00..0x20 | 0x7f => {
                    unicode[4] = HEX_DIGITS[usize::from(byte >> 4)];
                    unicode[5] = HEX_DIGITS[usize::from(byte & 0xf)];
                    &unicode
                }
                0x20..=0xff => continue,
            };
            self.out.write_all(&name[start..index])?;
            self.out.write_all(escaped)?;
            start = index + 1;
        }
        self.out.write_all(&name[start..])
    }

    /// Write `name` and the fields of `stat`, without the closing `}`. `parent_device` is `Some`
    /// for a directory, whose device is written when it differs from its parent's.
    fn write_stat(
        &mut self,
        name: &[u8],
        stat: &Stat,
        parent_device: Option<u64>,
    ) -> io::Result<()> {
        self.write_entry_name(parent_device.is_some(), name)?;
        if stat.size > 0 {
            write!(self.out, ",\"asize\":{}", stat.size)?;
        }
        if stat.blocks > 0 {
            write!(self.out, ",\"dsize\":{}", stat.blocks.saturating_mul(512))?;
        }
        if let Some(parent_device) = parent_device
            && stat.device != parent_device
        {
            write!(self.out, ",\"dev\":{}", stat.device)?;
        }
        match stat.entry_type {
            EntryType::HardLink => write!(
                self.out,
                ",\"ino\":{},\"hlnkc\":true,\"nlink\":{}",
                stat.inode, stat.link_count
            )?,
            EntryType::NonRegular => self.out.write_all(b",\"notreg\":true")?,
            EntryType::Directory | EntryType::Regular => {}
        }
        if self.extended {
            let extended = &stat.extended;
            if let Some(uid) = extended.uid {
                write!(self.out, ",\"uid\":{uid}")?;
            }
            if let Some(gid) = extended.gid {
                write!(self.out, ",\"gid\":{gid}")?;
            }
            if let Some(mode) = extended.mode {
                write!(self.out, ",\"mode\":{mode}")?;
            }
            if let Some(mtime) = extended.mtime {
                write!(self.out, ",\"mtime\":{mtime}")?;
            }
        }
        Ok(())
    }
}

impl<W: Write> Sink for JsonExport<W> {
    type Directory = JsonDirectory;

    fn create_root(&mut self, path: &[u8], stat: &Stat) -> io::Result<JsonDirectory> {
        self.add_directory(&mut JsonDirectory { device: 0 }, path, stat)
    }

    fn add_special(
        &mut self,
        _directory: &mut JsonDirectory,
        name: &[u8],
        special: Special,
    ) -> io::Result<()> {
        self.close_directory_entry(false)?;
        let (is_directory, fields): (bool, &[u8]) = match special {
            Special::ReadError => (false, b",\"read_error\":true}"),
            Special::Pattern => (false, b",\"excluded\":\"pattern\"}"),
            Special::OtherFilesystem => (true, b",\"excluded\":\"otherfs\"}"),
            Special::KernelFilesystem => (true, b",\"excluded\":\"kernfs\"}"),
        };
        self.write_entry_name(is_directory, name)?;
        self.out.write_all(fields)?;
        if is_directory {
            self.out.write_all(b"]")?;
        }
        Ok(())
    }

    fn add_stat(
        &mut self,
        _directory: &mut JsonDirectory,
        name: &[u8],
        stat: &Stat,
    ) -> io::Result<()> {
        self.close_directory_entry(false)?;
        self.write_stat(name, stat, None)?;
        self.out.write_all(b"}")
    }

    fn add_directory(
        &mut self,
        parent: &mut JsonDirectory,
        name: &[u8],
        stat: &Stat,
    ) -> io::Result<JsonDirectory> {
        self.close_directory_entry(false)?;
        self.write_stat(name, stat, Some(parent.device))?;
        self.directory_open = true;
        Ok(JsonDirectory {
            device: stat.device,
        })
    }

    fn set_read_error(&mut self, _directory: &mut JsonDirectory) -> io::Result<()> {
        self.close_directory_entry(true)
    }

    fn finish_directory(
        &mut self,
        _directory: JsonDirectory,
        _parent: Option<&mut JsonDirectory>,
    ) -> io::Result<()> {
        self.close_directory_entry(false)?;
        self.out.write_all(b"]")
    }
}
