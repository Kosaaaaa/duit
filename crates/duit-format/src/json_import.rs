use std::io::{self, Read};
use std::mem;

use duit_model::{EntryType, Sink, Special, Stat};
use zstd::stream::read::Decoder;

use crate::item::{ItemType, add_entry};

const ZSTD_MAGIC: &[u8] = b"\x28\xb5\x2f\xfd";

const BUFFER_SIZE: usize = 129 * 1024;

const KEY_CAPACITY: usize = 32;

const END_OF_INPUT: u8 = 0;

/// The bytes of a name that are kept. ncdu stops on a name that does not fit with 5 bytes to
/// spare.
const NAME_CAPACITY: usize = 32 * 1024;

/// Why an [`import`] failed.
#[derive(Debug)]
pub enum ImportError {
    /// The input is not a valid export, or it could not be read. ncdu reports the line and the
    /// byte within that line where reading stopped, both counted from 1.
    Input {
        line: u64,
        byte: u64,
        message: &'static str,
    },
    /// The sink failed.
    Sink(io::Error),
}

/// Import the ncdu JSON export that starts with `head` and continues with `input` into `sink`.
/// An export that starts with the zstd magic number is decompressed.
///
/// The input is read as bytes, so a name that is not UTF-8 is imported as written. Every
/// malformed input ends in an [`ImportError::Input`] with ncdu's message.
pub fn import<S: Sink>(sink: &mut S, input: impl Read, head: &[u8]) -> Result<(), ImportError> {
    let input = head.chain(input);
    if head.starts_with(ZSTD_MAGIC) {
        let decoder = Decoder::new(input).map_err(|_| ImportError::Input {
            line: 1,
            byte: 1,
            message: "decompression error",
        })?;
        export(&mut Parser::new(decoder, true), sink)
    } else {
        export(&mut Parser::new(input, false), sink)
    }
}

struct Item {
    kind: ItemType,
    stat: Stat,
    read_error: bool,
    name: Vec<u8>,
}

struct Container {
    is_object: bool,
    first: bool,
}

struct Parser<R> {
    input: R,
    compressed: bool,
    buffer: Vec<u8>,
    offset: usize,
    length: usize,
    pending: Option<u8>,
    line: u64,
    byte: u64,
}

impl<R: Read> Parser<R> {
    fn new(input: R, compressed: bool) -> Self {
        Self {
            input,
            compressed,
            buffer: vec![0; BUFFER_SIZE],
            offset: 0,
            length: 0,
            pending: None,
            line: 1,
            byte: 1,
        }
    }

    fn error(&self, message: &'static str) -> ImportError {
        ImportError::Input {
            line: self.line,
            byte: self.byte,
            message,
        }
    }

    fn fail<T>(&self, message: &'static str) -> Result<T, ImportError> {
        Err(self.error(message))
    }

    fn fill(&mut self) -> Result<(), ImportError> {
        self.offset = 0;
        self.length = loop {
            match self.input.read(&mut self.buffer) {
                Ok(length) => break length,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return self.fail(self.describe_read_error(&error)),
            }
        };
        Ok(())
    }

    fn describe_read_error(&self, error: &io::Error) -> &'static str {
        if self.compressed && error.raw_os_error().is_none() {
            "decompression error"
        } else if error.kind() == io::ErrorKind::IsADirectory {
            "not a file"
        } else if error.kind() == io::ErrorKind::OutOfMemory {
            "out of memory"
        } else {
            "I/O error"
        }
    }

    fn next_byte(&mut self) -> Result<u8, ImportError> {
        if let Some(byte) = self.pending.take() {
            self.advance_byte();
            return Ok(byte);
        }
        if self.offset == self.length {
            self.fill()?;
            if self.length == 0 {
                return Ok(END_OF_INPUT);
            }
        }
        let byte = self
            .buffer
            .get(self.offset)
            .copied()
            .unwrap_or(END_OF_INPUT);
        self.offset += 1;
        self.advance_byte();
        Ok(byte)
    }

    fn advance_byte(&mut self) {
        self.byte += 1;
    }

    fn retreat_byte(&mut self) {
        // As in ncdu, undoing the end of the input moves back, although reading it did not.
        self.byte = self.byte.saturating_sub(1);
    }

    fn undo(&mut self, byte: u8) {
        self.retreat_byte();
        self.pending = Some(byte);
    }

    fn next_char(&mut self) -> Result<u8, ImportError> {
        loop {
            match self.next_byte()? {
                b'\n' => {
                    self.line += 1;
                    self.byte = 1;
                }
                b' ' | b'\t' | b'\r' => {}
                byte => return Ok(byte),
            }
        }
    }

    fn literal(&mut self, literal: &[u8]) -> Result<(), ImportError> {
        for &expected in literal {
            if self.next_byte()? != expected {
                return self.fail("invalid JSON");
            }
        }
        Ok(())
    }

    fn hex_digit(&mut self) -> Result<u16, ImportError> {
        let byte = self.next_byte()?;
        match byte {
            b'0'..=b'9' => Ok(u16::from(byte - b'0')),
            b'a'..=b'f' => Ok(u16::from(byte - b'a' + 10)),
            b'A'..=b'F' => Ok(u16::from(byte - b'A' + 10)),
            _ => self.fail("invalid hex digit"),
        }
    }

    fn utf16_unit(&mut self) -> Result<u16, ImportError> {
        let mut unit = 0;
        for shift in [12, 8, 4, 0] {
            unit += self.hex_digit()? << shift;
        }
        Ok(unit)
    }

    fn unicode_escape(&mut self, buffer: &mut Vec<u8>, capacity: usize) -> Result<(), ImportError> {
        let first = self.utf16_unit()?;
        if (0xdc00..0xe000).contains(&first) {
            return self.fail("Unexpected low surrogate");
        }
        let code_point = if (0xd800..0xdc00).contains(&first) {
            self.literal(b"\\u")?;
            let second = self.utf16_unit()?;
            if !(0xdc00..0xe000).contains(&second) {
                return self.fail("Invalid low surrogate");
            }
            0x10000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00)
        } else {
            u32::from(first)
        };
        if buffer.len() + 6 < capacity
            && let Some(character) = char::from_u32(code_point)
        {
            buffer.extend_from_slice(character.encode_utf8(&mut [0; 4]).as_bytes());
        }
        Ok(())
    }

    fn string_content(&mut self, buffer: &mut Vec<u8>, capacity: usize) -> Result<(), ImportError> {
        buffer.clear();
        loop {
            let byte = match self.next_byte()? {
                b'"' => return Ok(()),
                b'\\' => match self.next_byte()? {
                    byte @ (b'"' | b'\\' | b'/') => byte,
                    b'b' => 0x08,
                    b'f' => 0x0c,
                    b'n' => b'\n',
                    b'r' => b'\r',
                    b't' => b'\t',
                    b'u' => {
                        self.unicode_escape(buffer, capacity)?;
                        continue;
                    }
                    _ => return self.fail("invalid escape sequence"),
                },
                byte @ 0x20..=0xff => byte,
                0x00..0x20 => return self.fail("invalid character in string"),
            };
            if buffer.len() < capacity {
                buffer.push(byte);
            }
        }
    }

    fn string(&mut self, buffer: &mut Vec<u8>, capacity: usize) -> Result<(), ImportError> {
        if self.next_char()? != b'"' {
            return self.fail("expected string");
        }
        self.string_content(buffer, capacity)
    }

    /// Read an unsigned integer of `bits` bits that starts with the digit `head`. As in ncdu, the
    /// value wraps, and an overflow is only detected when the value gets smaller. A leading `0`
    /// is the whole number.
    fn uint_tail<T: TryFrom<u64>>(&mut self, head: u8, bits: u32) -> Result<T, ImportError> {
        let mask = u64::MAX >> (u64::BITS - bits);
        let mut value = 0;
        if head != b'0' {
            value = u64::from(head - b'0');
            loop {
                let byte = self.next_byte()?;
                if !byte.is_ascii_digit() {
                    self.undo(byte);
                    break;
                }
                let next = value.wrapping_mul(10).wrapping_add(u64::from(byte - b'0')) & mask;
                if next < value {
                    return self.fail("integer out of range");
                }
                value = next;
            }
            if value == 0 {
                return self.fail("expected number");
            }
        }
        T::try_from(value).map_err(|_| self.error("integer out of range"))
    }

    fn uint<T: TryFrom<u64>>(&mut self, bits: u32) -> Result<T, ImportError> {
        match self.next_char()? {
            head @ b'0'..=b'9' => self.uint_tail(head, bits),
            _ => self.fail("expected number"),
        }
    }

    fn boolean(&mut self) -> Result<bool, ImportError> {
        match self.next_char()? {
            b't' => self.literal(&b"true"[1..]).map(|()| true),
            b'f' => self.literal(&b"false"[1..]).map(|()| false),
            _ => self.fail("expected boolean"),
        }
    }

    fn object(&mut self) -> Result<(), ImportError> {
        if self.next_char()? != b'{' {
            return self.fail("expected object");
        }
        Ok(())
    }

    fn key(
        &mut self,
        first: bool,
        buffer: &mut Vec<u8>,
        capacity: usize,
    ) -> Result<bool, ImportError> {
        match self.next_char()? {
            b',' if !first => self.string(buffer, capacity)?,
            b'"' if first => self.string_content(buffer, capacity)?,
            b'}' => return Ok(false),
            _ => return self.fail("invalid JSON"),
        }
        if self.next_char()? != b':' {
            return self.fail("invalid JSON");
        }
        Ok(true)
    }

    fn array(&mut self) -> Result<(), ImportError> {
        if self.next_char()? != b'[' {
            return self.fail("expected array");
        }
        Ok(())
    }

    fn element(&mut self, first: bool) -> Result<bool, ImportError> {
        match self.next_char()? {
            b',' if !first => Ok(true),
            b']' => Ok(false),
            byte if first => {
                self.undo(byte);
                Ok(true)
            }
            _ => self.fail("invalid JSON"),
        }
    }

    fn skip_content(&mut self, head: u8) -> Result<(), ImportError> {
        let mut containers: Vec<Container> = Vec::new();
        let mut head = head;
        loop {
            match head {
                b't' => self.literal(&b"true"[1..])?,
                b'f' => self.literal(&b"false"[1..])?,
                b'n' => self.literal(&b"null"[1..])?,
                b'-' | b'0'..=b'9' => loop {
                    let byte = self.next_byte()?;
                    if !matches!(byte, b'-' | b'+' | b'e' | b'E' | b'.' | b'0'..=b'9') {
                        self.undo(byte);
                        break;
                    }
                },
                b'"' => self.string_content(&mut Vec::new(), 0)?,
                b'[' => containers.push(Container {
                    is_object: false,
                    first: true,
                }),
                b'{' => containers.push(Container {
                    is_object: true,
                    first: true,
                }),
                _ => return self.fail("invalid JSON"),
            }
            loop {
                let Some(container) = containers.last_mut() else {
                    return Ok(());
                };
                let first = mem::replace(&mut container.first, false);
                let next = if container.is_object {
                    self.key(first, &mut Vec::new(), 0)?
                } else {
                    self.element(first)?
                };
                if next {
                    head = self.next_char()?;
                    break;
                }
                containers.pop();
            }
        }
    }

    fn skip(&mut self) -> Result<(), ImportError> {
        let head = self.next_char()?;
        self.skip_content(head)
    }
}

fn item_key<R: Read>(
    parser: &mut Parser<R>,
    item: &mut Item,
    key: &[u8],
) -> Result<(), ImportError> {
    let stat = &mut item.stat;
    match key {
        b"asize" => stat.size = parser.uint(u64::BITS)?,
        b"dsize" => stat.blocks = parser.uint::<u64>(u64::BITS)? >> 9,
        b"dev" => stat.device = parser.uint(u64::BITS)?,
        b"excluded" => {
            let mut value = Vec::new();
            parser.string(&mut value, KEY_CAPACITY)?;
            item.kind = ItemType::Special(match value.as_slice() {
                b"otherfs" | b"othfs" => Special::OtherFilesystem,
                b"kernfs" => Special::KernelFilesystem,
                _ => Special::Pattern,
            });
        }
        b"gid" => stat.extended.gid = Some(parser.uint(u32::BITS)?),
        b"hlnkc" => {
            if parser.boolean()? {
                item.kind = ItemType::Stat(EntryType::HardLink);
            }
        }
        b"ino" => stat.inode = parser.uint(u64::BITS)?,
        b"mode" => stat.extended.mode = Some(parser.uint(u16::BITS)?),
        b"mtime" => {
            stat.extended.mtime = Some(parser.uint(u64::BITS)?);
            let mut byte = parser.next_byte()?;
            if byte == b'.' {
                byte = parser.next_byte()?;
                while byte.is_ascii_digit() {
                    byte = parser.next_byte()?;
                }
            }
            parser.undo(byte);
        }
        b"name" => {
            if !item.name.is_empty() {
                return parser.fail("duplicate key");
            }
            parser.string(&mut item.name, NAME_CAPACITY)?;
            if item.name.len() > NAME_CAPACITY - 5 {
                return parser.fail("too long file name");
            }
        }
        b"nlink" => {
            stat.link_count = parser.uint(31)?;
            if item.kind != ItemType::Stat(EntryType::Directory) && stat.link_count > 1 {
                item.kind = ItemType::Stat(EntryType::HardLink);
            }
        }
        b"notreg" => {
            if parser.boolean()? {
                item.kind = ItemType::Stat(EntryType::NonRegular);
            }
        }
        b"read_error" => {
            if parser.boolean()? {
                if item.kind == ItemType::Stat(EntryType::Directory) {
                    item.read_error = true;
                } else {
                    item.kind = ItemType::Special(Special::ReadError);
                }
            }
        }
        b"uid" => stat.extended.uid = Some(parser.uint(u32::BITS)?),
        _ => parser.skip()?,
    }
    Ok(())
}

fn read_item<R: Read>(
    parser: &mut Parser<R>,
    item: &mut Item,
    device: u64,
    is_root: bool,
) -> Result<bool, ImportError> {
    let is_array = match parser.next_char()? {
        b'[' => {
            parser.object()?;
            true
        }
        b'{' => false,
        _ => return parser.fail("expected object or array"),
    };
    if is_root && !is_array {
        return parser.fail("parent item must be a directory");
    }
    item.kind = ItemType::Stat(if is_array {
        EntryType::Directory
    } else {
        EntryType::Regular
    });
    item.stat = Stat {
        device,
        ..Stat::default()
    };
    item.read_error = false;
    item.name.clear();
    let mut key = Vec::new();
    let mut first = true;
    while parser.key(first, &mut key, KEY_CAPACITY)? {
        first = false;
        item_key(parser, item, &key)?;
    }
    if item.name.is_empty() {
        return parser.fail("missing \"name\" field");
    }
    Ok(is_array)
}

fn export<R: Read, S: Sink>(parser: &mut Parser<R>, sink: &mut S) -> Result<(), ImportError> {
    parser.array()?;
    if parser.uint::<u16>(u16::BITS)? != 1 {
        return parser.fail("incompatible major format version");
    }
    if !parser.element(false)? {
        return parser.fail("expected array element");
    }
    let _minor_version = parser.uint::<u16>(u16::BITS)?;
    if !parser.element(false)? {
        return parser.fail("expected array element");
    }
    parser.object()?;
    parser.skip_content(b'{')?;
    if !parser.element(false)? {
        return parser.fail("expected array element");
    }
    items(parser, sink)?;
    while parser.element(false)? {
        parser.skip()?;
    }
    if parser.next_char()? != END_OF_INPUT {
        return parser.fail("trailing garbage");
    }
    Ok(())
}

fn items<R: Read, S: Sink>(parser: &mut Parser<R>, sink: &mut S) -> Result<(), ImportError> {
    let mut item = Item {
        kind: ItemType::Stat(EntryType::Directory),
        stat: Stat::default(),
        read_error: false,
        name: Vec::new(),
    };
    read_item(parser, &mut item, 0, true)?;
    if item.kind != ItemType::Stat(EntryType::Directory) {
        return parser.fail("parent item must be a directory");
    }
    item.stat.entry_type = EntryType::Directory;
    let mut root = sink
        .create_root(&item.name, &item.stat)
        .map_err(ImportError::Sink)?;
    if item.read_error {
        sink.set_read_error(&mut root).map_err(ImportError::Sink)?;
    }
    let mut directories = vec![(root, item.stat.device)];
    while let Some((directory, device)) = directories.last_mut() {
        if !parser.element(false)? {
            if let Some((directory, _)) = directories.pop() {
                let parent = directories.last_mut().map(|(parent, _)| parent);
                sink.finish_directory(directory, parent)
                    .map_err(ImportError::Sink)?;
            }
            continue;
        }
        let is_array = read_item(parser, &mut item, *device, false)?;
        let child = add_entry(
            sink,
            directory,
            item.kind,
            &item.name,
            &item.stat,
            item.read_error,
        )
        .map_err(ImportError::Sink)?;
        if let Some(child) = child {
            directories.push((child, item.stat.device));
            continue;
        }
        if is_array && parser.element(false)? {
            return parser.fail("unexpected contents in an excluded directory");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Parser;

    #[test]
    fn json_parser() {
        let json: &[u8] = br#"{
  "null": null,
  "true": true,
  "false": false,
  "zero":0 ,"uint": 123,
  "emptyObj": {},
  "emptyArray": [],
  "emptyString": "",
  "encString": "\"\\\/\b\f\n\uBe3F",
  "numbers": [0,1,20,-300, 3.4 ,0e-10  , -100.023e+13 ]
}"#;
        let mut parser = Parser::new(json, false);
        assert!(parser.skip().is_ok());

        let mut parser = Parser::new(json, false);
        let mut buffer = Vec::new();
        let mut key = |parser: &mut Parser<&[u8]>, first: bool| -> Option<Vec<u8>> {
            match parser.key(first, &mut buffer, 128) {
                Ok(true) => Some(buffer.clone()),
                Ok(false) | Err(_) => None,
            }
        };
        assert!(parser.object().is_ok());

        assert_eq!(key(&mut parser, true).as_deref(), Some(&b"null"[..]));
        assert!(parser.skip().is_ok());

        assert_eq!(key(&mut parser, false).as_deref(), Some(&b"true"[..]));
        assert_eq!(parser.boolean().ok(), Some(true));

        assert_eq!(key(&mut parser, false).as_deref(), Some(&b"false"[..]));
        assert_eq!(parser.boolean().ok(), Some(false));

        assert_eq!(key(&mut parser, false).as_deref(), Some(&b"zero"[..]));
        assert_eq!(parser.uint::<u8>(u8::BITS).ok(), Some(0));

        assert_eq!(key(&mut parser, false).as_deref(), Some(&b"uint"[..]));
        assert_eq!(parser.uint::<u8>(u8::BITS).ok(), Some(123));

        assert_eq!(key(&mut parser, false).as_deref(), Some(&b"emptyObj"[..]));
        assert!(parser.object().is_ok());
        assert_eq!(key(&mut parser, true), None);

        assert_eq!(key(&mut parser, false).as_deref(), Some(&b"emptyArray"[..]));
        assert!(parser.array().is_ok());
        assert_eq!(parser.element(true).ok(), Some(false));

        let mut string = Vec::new();
        assert_eq!(
            key(&mut parser, false).as_deref(),
            Some(&b"emptyString"[..])
        );
        assert!(parser.string(&mut string, 128).is_ok());
        assert_eq!(string, b"");

        assert_eq!(key(&mut parser, false).as_deref(), Some(&b"encString"[..]));
        assert!(parser.string(&mut string, 128).is_ok());
        assert_eq!(string, "\"\\/\x08\x0c\n\u{be3f}".as_bytes());

        assert_eq!(key(&mut parser, false).as_deref(), Some(&b"numbers"[..]));
        assert!(parser.skip().is_ok());

        assert_eq!(key(&mut parser, true), None);
    }
}
