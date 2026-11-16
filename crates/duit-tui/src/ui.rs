//! Helpers that ncdu keeps in `ui.zig`: printable file names, shortening to a column width,
//! human-readable sizes, numbers, modes and timestamps, and drawing text on the screen.

use std::borrow::Cow;
use std::fmt::Write;
use std::iter;
use std::mem::MaybeUninit;

use ratatui::buffer::Buffer;
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthChar;

use crate::util::fmt5dec;

/// Whether `byte` is a control character that [`to_utf8`] escapes.
fn is_bad_char(byte: u8) -> bool {
    matches!(byte, 0..=0x1F | 0x7F)
}

/// Convert `input` to valid, mostly printable UTF-8. Control characters and bytes that are not
/// valid UTF-8 become `\x##`. Non-printable Unicode characters are not escaped.
pub fn to_utf8(input: &[u8]) -> Cow<'_, str> {
    if !input.iter().copied().any(is_bad_char)
        && let Ok(valid) = str::from_utf8(input)
    {
        return Cow::Borrowed(valid);
    }
    // Writing to a `String` cannot fail.
    let mut output = String::with_capacity(input.len());
    for chunk in input.utf8_chunks() {
        for character in chunk.valid().chars() {
            if let Ok(byte) = u8::try_from(character)
                && is_bad_char(byte)
            {
                let _ = write!(output, "\\x{byte:02X}");
            } else {
                output.push(character);
            }
        }
        for byte in chunk.invalid() {
            let _ = write!(output, "\\x{byte:02X}");
        }
    }
    Cow::Owned(output)
}

/// The terminal width of `character`, as ncurses counts it with `wcwidth()`. Control characters
/// count as zero.
fn char_width(character: char) -> usize {
    character.width().unwrap_or(0)
}

/// Shorten `input` to fit in `max_width` columns. A string that is too long keeps a prefix and a
/// suffix with `...` in between.
pub fn shorten(input: &str, max_width: usize) -> Cow<'_, str> {
    if max_width < 4 {
        return Cow::Borrowed("...");
    }
    let mut total_width = 0;
    let mut prefix_width = 0;
    let mut prefix_end = 0;
    let mut prefix_done = false;
    for character in input.chars() {
        let width = char_width(character);
        total_width += width;
        if !prefix_done && prefix_width + width < (max_width - 1) / 2 {
            prefix_width += width;
            prefix_end += character.len_utf8();
        } else {
            prefix_done = true;
        }
    }
    if total_width <= max_width {
        return Cow::Borrowed(input);
    }

    let mut output = String::with_capacity(max_width);
    output.push_str(&input[..prefix_end]);
    output.push_str("...");
    let mut start_width = prefix_width;
    let mut start_end = prefix_end;
    for character in input[prefix_end..].chars() {
        start_width += char_width(character);
        start_end += character.len_utf8();
        if total_width - start_width <= max_width - prefix_width - 3 {
            output.push_str(&input[start_end..]);
            break;
        }
    }
    Cow::Owned(output)
}

/// A size in a human-readable form: a [`fmt5dec`] number and a unit. The two take 8 columns
/// together with SI units, or 9 with binary units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FmtSize {
    /// The number, as `###.#`.
    pub number: String,
    /// The unit with its leading spaces, as `" XB"` or `" XiB"`.
    pub unit: &'static str,
}

impl FmtSize {
    /// Format `value` bytes, in powers of 1000 if `si` is set and powers of 1024 otherwise.
    pub fn new(value: u64, si: bool) -> Self {
        let (unit, multiplier, divisor) = if si {
            if value < 1000 {
                ("  B", 10, 1)
            } else if value < 999_950 {
                (" kB", 1, 100)
            } else if value < 999_950_000 {
                (" MB", 1, 100_000)
            } else if value < 999_950_000_000 {
                (" GB", 1, 100_000_000)
            } else if value < 999_950_000_000_000 {
                (" TB", 1, 100_000_000_000)
            } else if value < 999_950_000_000_000_000 {
                (" PB", 1, 100_000_000_000_000)
            } else {
                (" EB", 1, 100_000_000_000_000_000)
            }
        } else {
            // Each cutoff is the smallest size that is at least 999.95 of the unit, which would
            // round to 1000.0.
            if value < 1000 {
                ("   B", 10, 1)
            } else if value < 1_023_949 {
                (" KiB", 10, 1 << 10)
            } else if value < 1_048_523_572 {
                (" MiB", 10, 1 << 20)
            } else if value < 1_073_688_136_909 {
                (" GiB", 10, 1 << 30)
            } else if value < 1_099_456_652_194_612 {
                (" TiB", 10, 1 << 40)
            } else if value < 1_125_843_611_847_281_869 {
                (" PiB", 10, 1 << 50)
            } else {
                (" EiB", 1, (1 << 60) / 10)
            }
        };
        Self {
            number: fmt5dec((value * multiplier).saturating_add(divisor / 2) / divisor),
            unit,
        }
    }
}

/// Text in the `default` style of ncdu's `off` color scheme.
pub(crate) const PLAIN: Style = Style::new();
/// The header, the footer, and the cursor row, which ncdu's `off` color scheme draws in reverse
/// video.
pub(crate) const REVERSE: Style = Style::new().add_modifier(Modifier::REVERSED);
/// The bold text of the header and the footer.
pub(crate) const BOLD_REVERSE: Style =
    Style::new().add_modifier(Modifier::REVERSED.union(Modifier::BOLD));

/// Draw `text` at column `x` of row `y` in `style`, cut off at the right edge, and return the
/// column after it. Rows outside of `buffer` are skipped.
pub(crate) fn draw_text(buffer: &mut Buffer, x: u16, y: u16, text: &str, style: Style) -> u16 {
    if y >= buffer.area.bottom() {
        return x;
    }
    buffer.set_stringn(x, y, text, usize::MAX, style).0
}

/// Draw `length` copies of `character` from column `x` of row `y`, as ncurses `hline()`.
pub(crate) fn draw_line(
    buffer: &mut Buffer,
    x: u16,
    y: u16,
    character: char,
    length: u16,
    style: Style,
) {
    let line: String = iter::repeat_n(character, length.into()).collect();
    draw_text(buffer, x, y, &line, style);
}

/// Draw `value` bytes as a human-readable size, as [`FmtSize`] formats it, and return the column
/// after it.
pub(crate) fn draw_size(
    buffer: &mut Buffer,
    x: u16,
    y: u16,
    value: u64,
    si: bool,
    style: Style,
) -> u16 {
    let size = FmtSize::new(value, si);
    let x = draw_text(buffer, x, y, &size.number, style);
    draw_text(buffer, x, y, size.unit, style)
}

/// Format `value` in full, with `separator` between groups of three digits.
pub fn format_number(value: u64, separator: &str) -> String {
    let digits = value.to_string();
    let mut output = String::with_capacity(digits.len() * (1 + separator.len()));
    for (index, digit) in digits.chars().enumerate() {
        if index != 0 && (digits.len() - index).is_multiple_of(3) {
            output.push_str(separator);
        }
        output.push(digit);
    }
    output
}

/// Format the file type and the permission bits of `mode` in 10 columns, as `ls -l` does. The
/// file types have the values of POSIX, which Linux and macOS share.
pub fn format_mode(mode: u16) -> String {
    let file_type = match mode & 0o170_000 {
        0o040_000 => 'd',
        0o100_000 => '-',
        0o120_000 => 'l',
        0o010_000 => 'p',
        0o140_000 => 's',
        0o020_000 => 'c',
        0o060_000 => 'b',
        _ => '?',
    };
    let bit = |mask: u16, set: char| if mode & mask > 0 { set } else { '-' };
    let execute = |special: u16, special_set: char, execute: u16| {
        if mode & special > 0 {
            special_set
        } else {
            bit(execute, 'x')
        }
    };
    let sticky = if file_type == 'd' { 't' } else { 'T' };
    [
        file_type,
        bit(0o400, 'r'),
        bit(0o200, 'w'),
        execute(0o4000, 's', 0o100),
        bit(0o040, 'r'),
        bit(0o020, 'w'),
        execute(0o2000, 's', 0o010),
        bit(0o004, 'r'),
        bit(0o002, 'w'),
        execute(0o1000, sticky, 0o001),
    ]
    .into_iter()
    .collect()
}

/// Format `timestamp`, in seconds since the Unix epoch, in the local time zone in 25 columns, as
/// `2024-01-01 12:00:00 +0100`. Returns `None` for a time that the C library cannot convert.
#[expect(unsafe_code, reason = "localtime_r and strftime are foreign functions")]
pub(crate) fn format_timestamp(timestamp: u64) -> Option<String> {
    // ncdu clamps the timestamp to the range of `time_t`.
    let time = libc::time_t::try_from(timestamp).unwrap_or(libc::time_t::MAX);
    let mut local = MaybeUninit::<libc::tm>::uninit();
    // `localtime_r` reads the time zone from `TZ` on its first call, as `localtime` in ncdu.
    // SAFETY: `localtime_r` reads `time` and writes to `local`, which is valid for writes. It
    // returns null when it fails, and has filled `local` otherwise.
    let local = unsafe {
        if libc::localtime_r(&raw const time, local.as_mut_ptr()).is_null() {
            return None;
        }
        local.assume_init()
    };
    let mut output = [0u8; 32];
    // SAFETY: `strftime` writes at most `output.len()` bytes to `output`, which is valid for
    // writes. The format is a NUL-terminated string, and `local` is initialized.
    let length = unsafe {
        libc::strftime(
            output.as_mut_ptr().cast(),
            output.len(),
            c"%Y-%m-%d %H:%M:%S %z".as_ptr(),
            &raw const local,
        )
    };
    (length > 0).then(|| String::from_utf8_lossy(&output[..length]).into_owned())
}

#[cfg(test)]
mod tests {
    use super::{FmtSize, format_mode, format_number, shorten, to_utf8};

    #[track_caller]
    fn shorten_test(input: &str, max_width: usize, output: &str) {
        assert_eq!(shorten(input, max_width), output);
    }

    /// Port of the upstream test "shorten".
    #[test]
    fn shorten_strings() {
        shorten_test("abcde", 3, "...");
        shorten_test("abcde", 5, "abcde");
        shorten_test("abcde", 4, "...e");
        shorten_test("abcdefgh", 6, "a...gh");
        shorten_test("abcdefgh", 7, "ab...gh");
        shorten_test("ＡＢＣＤＥＦＧＨ", 16, "ＡＢＣＤＥＦＧＨ");
        shorten_test("ＡＢＣＤＥＦＧＨ", 7, "Ａ...Ｈ");
        shorten_test("ＡＢＣＤＥＦＧＨ", 8, "Ａ...Ｈ");
        shorten_test("ＡＢＣＤＥＦＧＨ", 9, "Ａ...ＧＨ");
        shorten_test("ＡaＢＣＤＥＦＧＨ", 8, "Ａ...Ｈ");
        shorten_test("ＡＢＣＤＥＦＧaＨ", 8, "Ａ...aＨ");
        shorten_test("ＡＢＣＤＥＦＧＨ", 15, "ＡＢＣ...ＦＧＨ");
        // Variation selectors: the U+FE0E of the last dropped heart stays after the '...'.
        shorten_test(
            "\u{2764}\u{fe0e}a\u{2764}\u{fe0e}a\u{2764}\u{fe0e}a",
            5,
            "\u{2764}\u{fe0e}...\u{fe0e}a",
        );
        // Combining marks: the marks of the last dropped character stay after the '...'.
        shorten_test(
            "a\u{328}\u{301}a\u{328}\u{301}a\u{328}\u{301}a\u{328}\u{301}a\u{328}\u{301}a\u{328}\u{301}",
            5,
            "a\u{328}\u{301}...\u{328}\u{301}a\u{328}\u{301}",
        );
    }

    /// Control characters and invalid UTF-8, including an encoded surrogate, are escaped byte by
    /// byte. Upstream has no test for `toUtf8`.
    #[test]
    fn to_utf8_escapes() {
        assert_eq!(to_utf8(b"plain \xc4\x85"), "plain \u{105}");
        assert_eq!(to_utf8(b"tab\there\x7f"), "tab\\x09here\\x7F");
        assert_eq!(to_utf8(b"\xe2\x82A\xff"), "\\xE2\\x82A\\xFF");
        assert_eq!(to_utf8(b"\xed\xa0\x80"), "\\xED\\xA0\\x80");
    }

    #[track_caller]
    fn fmt_size_test(value: u64, si: bool, expected: &str) {
        let size = FmtSize::new(value, si);
        assert_eq!(format!("{}{}", size.number, size.unit), expected);
    }

    /// Port of the upstream test "fmtsize".
    #[test]
    fn fmt_size() {
        fmt_size_test(0, true, "  0.0  B");
        fmt_size_test(999, true, "999.0  B");
        fmt_size_test(1000, true, "  1.0 kB");
        fmt_size_test(1049, true, "  1.0 kB");
        fmt_size_test(1050, true, "  1.1 kB");
        fmt_size_test(999_899, true, "999.9 kB");
        fmt_size_test(999_949, true, "999.9 kB");
        fmt_size_test(999_950, true, "  1.0 MB");
        fmt_size_test(1_000_000, true, "  1.0 MB");
        fmt_size_test(999_850_009, true, "999.9 MB");
        fmt_size_test(999_899_999, true, "999.9 MB");
        fmt_size_test(999_900_000, true, "999.9 MB");
        fmt_size_test(999_949_999, true, "999.9 MB");
        fmt_size_test(999_950_000, true, "  1.0 GB");
        fmt_size_test(999_999_999, true, "  1.0 GB");
        fmt_size_test(u64::MAX, true, " 18.4 EB");

        fmt_size_test(0, false, "  0.0   B");
        fmt_size_test(999, false, "999.0   B");
        fmt_size_test(1000, false, "  1.0 KiB");
        fmt_size_test(1024, false, "  1.0 KiB");
        fmt_size_test(102_400, false, "100.0 KiB");
        fmt_size_test(1_023_898, false, "999.9 KiB");
        fmt_size_test(1_023_949, false, "  1.0 MiB");
        fmt_size_test(1_048_523_571, false, "999.9 MiB");
        fmt_size_test(1_048_523_572, false, "  1.0 GiB");
        fmt_size_test(1_073_688_136_908, false, "999.9 GiB");
        fmt_size_test(1_073_688_136_909, false, "  1.0 TiB");
        fmt_size_test(1_099_456_652_194_611, false, "999.9 TiB");
        fmt_size_test(1_099_456_652_194_612, false, "  1.0 PiB");
        fmt_size_test(1_125_843_611_847_281_868, false, "999.9 PiB");
        fmt_size_test(1_125_843_611_847_281_869, false, "  1.0 EiB");
        fmt_size_test(u64::MAX, false, " 16.0 EiB");
    }

    /// Upstream has no test for `addnum`.
    #[test]
    fn format_numbers() {
        assert_eq!(format_number(0, ","), "0");
        assert_eq!(format_number(999, ","), "999");
        assert_eq!(format_number(1000, ","), "1,000");
        assert_eq!(format_number(123_456, "."), "123.456");
        assert_eq!(format_number(1_234_567, "\u{a0}"), "1\u{a0}234\u{a0}567");
        assert_eq!(format_number(u64::MAX, ","), "18,446,744,073,709,551,615");
    }

    /// The set-user-ID, set-group-ID, and sticky bits replace the execute bits, and the sticky bit
    /// is `T` on anything but a directory. Upstream has no test for `addmode`.
    #[test]
    fn format_modes() {
        assert_eq!(format_mode(0o100_644), "-rw-r--r--");
        assert_eq!(format_mode(0o040_755), "drwxr-xr-x");
        assert_eq!(format_mode(0o041_777), "drwxrwxrwt");
        assert_eq!(format_mode(0o101_644), "-rw-r--r-T");
        assert_eq!(format_mode(0o106_755), "-rwsr-sr-x");
        assert_eq!(format_mode(0o106_644), "-rwsr-sr--");
        assert_eq!(format_mode(0o120_777), "lrwxrwxrwx");
        assert_eq!(format_mode(0o140_755), "srwxr-xr-x");
        assert_eq!(format_mode(0o010_600), "prw-------");
        assert_eq!(format_mode(0o020_600), "crw-------");
        assert_eq!(format_mode(0o060_600), "brw-------");
        assert_eq!(format_mode(0o000_600), "?rw-------");
    }
}
