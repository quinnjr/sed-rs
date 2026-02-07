// Adapted from sd (https://github.com/chmln/sd) - MIT License
// Copyright (c) 2018 Gregory

use std::char;
use std::str::Chars;

/// Takes in a string with backslash escapes written out with literal
/// backslash characters and converts it to a string with the proper
/// escaped characters.
#[allow(dead_code)]
pub fn unescape(input: &str) -> String {
    let mut chars = input.chars();
    let mut s = String::new();

    while let Some(c) = chars.next() {
        if c != '\\' {
            s.push(c);
            continue;
        }
        let Some(ch) = chars.next() else {
            assert_eq!(c, '\\');
            s.push('\\');
            break;
        };

        let escaped: Option<char> = match ch {
            'n' => Some('\n'),
            'r' => Some('\r'),
            't' => Some('\t'),
            '\'' => Some('\''),
            '\"' => Some('\"'),
            '\\' => Some('\\'),
            'a' => Some('\x07'),
            'u' => escape_n_chars(&mut chars, 4),
            'x' => escape_n_chars(&mut chars, 2),
            _ => None,
        };
        if let Some(ch) = escaped {
            s.push(ch);
        } else {
            s.push('\\');
            s.push(ch);
        }
    }

    s
}

/// This is for sequences such as `\x08` or `\u1234`
fn escape_n_chars(chars: &mut Chars<'_>, length: usize) -> Option<char> {
    let s = chars.as_str().get(0..length)?;
    let u = u32::from_str_radix(s, 16).ok()?;
    let ch = char::from_u32(u)?;
    // Advance past the hex digits. nth(length-1) advances by `length`
    // positions (0-indexed), but we need to skip exactly `length` chars.
    for _ in 0..length {
        chars.next();
    }
    Some(ch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_escapes() {
        assert_eq!(unescape("hello\\nworld"), "hello\nworld");
        assert_eq!(unescape("tab\\there"), "tab\there");
        assert_eq!(unescape("cr\\rhere"), "cr\rhere");
        assert_eq!(unescape("bell\\ahere"), "bell\x07here");
    }

    #[test]
    fn test_backslash() {
        assert_eq!(unescape("\\\\"), "\\");
        assert_eq!(unescape("\\"), "\\");
    }

    #[test]
    fn test_hex_and_unicode() {
        assert_eq!(unescape("\\x41"), "A");
        assert_eq!(unescape("\\u0042"), "B");
    }

    #[test]
    fn test_passthrough() {
        assert_eq!(unescape("no escapes"), "no escapes");
        assert_eq!(unescape(""), "");
    }

    #[test]
    fn test_invalid_escapes() {
        assert_eq!(unescape("\\q"), "\\q");
        assert_eq!(unescape("\\xG"), "\\xG");
    }
}
