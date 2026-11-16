//! Text helpers that ncdu keeps in `ui.zig`: printable file names, shortening to a column width,
//! and human-readable sizes.

use std::borrow::Cow;
use std::fmt::Write;

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

#[cfg(test)]
mod tests {
    use super::{FmtSize, shorten, to_utf8};

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
}
