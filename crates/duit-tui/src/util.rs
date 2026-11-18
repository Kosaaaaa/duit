//! Helpers that ncdu keeps in `util.zig` and that only the UI calls: number formatting and natural
//! sort order.

use std::cmp::Ordering;

/// Format `number` tenths as a right-aligned `###.#`, five columns wide. `number` is at most 9999.
pub fn fmt5dec(number: u64) -> String {
    format!("{:>3}.{}", number / 10, number % 10)
}

/// Whitespace as in Zig's `std.ascii.isWhitespace`, which unlike [`u8::is_ascii_whitespace`]
/// includes the vertical tab.
fn is_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t'..=b'\r')
}

/// Compare `a` and `b` in natural order, where runs of digits compare as numbers. A port of
/// `strnatcmp()` from <https://github.com/sourcefrog/natsort/>.
pub fn strnatcmp(a: &[u8], b: &[u8]) -> Ordering {
    // The algorithm reads one byte past the end, where the C string has its NUL terminator.
    let at = |string: &[u8], index: usize| string.get(index).copied().unwrap_or(0);
    let mut a_index = 0;
    let mut b_index = 0;
    loop {
        while is_whitespace(at(a, a_index)) {
            a_index += 1;
        }
        while is_whitespace(at(b, b_index)) {
            b_index += 1;
        }

        if at(a, a_index).is_ascii_digit() && at(b, b_index).is_ascii_digit() {
            if at(a, a_index) == b'0' || at(b, b_index) == b'0' {
                // Compare left-aligned, as fractional parts.
                loop {
                    let (a_byte, b_byte) = (at(a, a_index), at(b, b_index));
                    match (a_byte.is_ascii_digit(), b_byte.is_ascii_digit()) {
                        (false, false) => break,
                        (false, true) => return Ordering::Less,
                        (true, false) => return Ordering::Greater,
                        (true, true) => {}
                    }
                    match a_byte.cmp(&b_byte) {
                        Ordering::Equal => {}
                        order @ (Ordering::Less | Ordering::Greater) => return order,
                    }
                    a_index += 1;
                    b_index += 1;
                }
            } else {
                // Compare right-aligned: the longer run is greater, and otherwise the first
                // differing digit decides.
                let mut bias = Ordering::Equal;
                loop {
                    let (a_byte, b_byte) = (at(a, a_index), at(b, b_index));
                    match (a_byte.is_ascii_digit(), b_byte.is_ascii_digit()) {
                        (false, false) => {
                            if bias != Ordering::Equal || (a_byte == 0 && b_byte == 0) {
                                return bias;
                            }
                            break;
                        }
                        (false, true) => return Ordering::Less,
                        (true, false) => return Ordering::Greater,
                        (true, true) => {}
                    }
                    if bias == Ordering::Equal {
                        bias = a_byte.cmp(&b_byte);
                    }
                    a_index += 1;
                    b_index += 1;
                }
            }
        }
        let (a_byte, b_byte) = (at(a, a_index), at(b, b_index));
        if a_byte == 0 && b_byte == 0 {
            return Ordering::Equal;
        }
        match a_byte.cmp(&b_byte) {
            Ordering::Equal => {}
            order @ (Ordering::Less | Ordering::Greater) => return order,
        }
        a_index += 1;
        b_index += 1;
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::{fmt5dec, strnatcmp};

    /// Port of the upstream test "fmt5dec".
    #[test]
    fn fmt5dec_numbers() {
        assert_eq!(fmt5dec(0), "  0.0");
        assert_eq!(fmt5dec(5), "  0.5");
        assert_eq!(fmt5dec(95), "  9.5");
        assert_eq!(fmt5dec(125), " 12.5");
        assert_eq!(fmt5dec(1239), "123.9");
        assert_eq!(fmt5dec(9999), "999.9");
    }

    /// Port of the upstream test "strnatcmp".
    #[test]
    fn strnatcmp_sorted() {
        // Test strings from https://github.com/sourcefrog/natsort/
        // Includes sorted-words, sorted-dates and sorted-fractions.
        let words: [&[u8]; 42] = [
            b"1-02",
            b"1-2",
            b"1-20",
            b"1.002.01",
            b"1.002.03",
            b"1.002.08",
            b"1.009.02",
            b"1.009.10",
            b"1.009.20",
            b"1.010.12",
            b"1.011.02",
            b"10-20",
            b"1999-3-3",
            b"1999-12-25",
            b"2000-1-2",
            b"2000-1-10",
            b"2000-3-23",
            b"fred",
            b"jane",
            b"pic01",
            b"pic02",
            b"pic02a",
            b"pic02000",
            b"pic05",
            b"pic2",
            b"pic3",
            b"pic4",
            b"pic 4 else",
            b"pic 5",
            b"pic 5 ",
            b"pic 5 something",
            b"pic 6",
            b"pic   7",
            b"pic100",
            b"pic100a",
            b"pic120",
            b"pic121",
            b"tom",
            b"x2-g8",
            b"x2-y08",
            b"x2-y7",
            b"x8-y8",
        ];
        // Test each string against each other string, simple and thorough.
        for (index, word) in words.iter().enumerate() {
            assert_eq!(strnatcmp(word, word), Ordering::Equal);
            for other in &words[..index] {
                assert_eq!(strnatcmp(word, other), Ordering::Greater);
            }
            for other in &words[index + 1..] {
                assert_eq!(strnatcmp(word, other), Ordering::Less);
            }
        }
    }

    /// Whitespace includes vertical tab and form feed, as in Zig's `std.ascii.isWhitespace`.
    #[test]
    fn strnatcmp_whitespace() {
        assert_eq!(strnatcmp(b"\x0b\x0ca", b"a"), Ordering::Equal);
    }
}
