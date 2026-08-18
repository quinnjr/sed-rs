// Translation of sed regex dialects into Rust `regex` syntax.
//
// GNU sed defaults to POSIX *basic* regular expressions (BRE), where
// `\(...\)` groups, `\{m,n\}` is an interval, and bare `( ) { } + ? |`
// are literals; `-E` switches to *extended* REs (ERE), which is what the
// Rust `regex` crate speaks natively. `translate_pattern` rewrites a sed
// pattern into equivalent `regex` syntax, swapping the escaped/unescaped
// meaning of the metacharacters in BRE mode.
//
// Both modes also normalize GNU sed's character escapes — `\xHH` (hex),
// `\oNNN` (octal), and `\dNNN` (decimal) — into `\x{..}` regex syntax.

/// Parse up to `max` digits of the given radix starting at `chars[*i]`,
/// advancing `*i` past them. Returns `None` if no digit is present.
pub(crate) fn parse_digits(chars: &[char], i: &mut usize, radix: u32, max: usize) -> Option<u32> {
    let mut val: u32 = 0;
    let mut n = 0;
    while n < max {
        match chars.get(*i).and_then(|c| c.to_digit(radix)) {
            Some(d) => {
                val = val * radix + d;
                *i += 1;
                n += 1;
            }
            None => break,
        }
    }
    if n == 0 { None } else { Some(val) }
}

/// Parse the digits of a `\xHH` / `\oNNN` / `\dNNN` character escape
/// (after the letter), returning the decoded value. `None` if no digit
/// of the right radix follows.
pub(crate) fn parse_escape_value(kind: char, chars: &[char], i: &mut usize) -> Option<u32> {
    let (radix, max) = match kind {
        'x' => (16, 2),
        'o' => (8, 3),
        _ => (10, 3),
    };
    parse_digits(chars, i, radix, max)
}

pub(crate) fn translate_pattern(pattern: &str, extended: bool) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::with_capacity(pattern.len() + 8);
    let mut i = 0;
    // BRE bookkeeping: `^` anchors and a leading `*` is literal only at the
    // start of the pattern or of a subexpression (after `\(` or `\|`).
    let mut at_subexpr_start = true;
    // Whether a repeatable atom precedes the current position.
    let mut prev_atom = false;

    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' if i + 1 < chars.len() => {
                let next = chars[i + 1];
                i += 2;
                match next {
                    'x' | 'o' | 'd' => {
                        match parse_escape_value(next, &chars, &mut i) {
                            Some(v) => out.push_str(&format!("\\x{{{v:X}}}")),
                            // Bare `\d` keeps the Rust regex digit-class
                            // meaning; `\x`/`\o` degrade to the letter.
                            None if next == 'd' => out.push_str("\\d"),
                            None => out.push(next),
                        }
                        prev_atom = true;
                        at_subexpr_start = false;
                    }
                    '(' | '|' if !extended => {
                        out.push(next);
                        at_subexpr_start = true;
                        prev_atom = false;
                    }
                    ')' if !extended => {
                        out.push(')');
                        prev_atom = true;
                        at_subexpr_start = false;
                    }
                    '{' | '}' | '+' | '?' if !extended => {
                        out.push(next);
                        at_subexpr_start = false;
                    }
                    _ => {
                        out.push('\\');
                        out.push(next);
                        prev_atom = true;
                        at_subexpr_start = false;
                    }
                }
            }
            '\\' => {
                // Trailing backslash: pass through, regex will reject it.
                out.push('\\');
                i += 1;
            }
            '[' => {
                copy_bracket_expression(&chars, &mut i, &mut out);
                prev_atom = true;
                at_subexpr_start = false;
            }
            '(' | ')' | '{' | '}' | '+' | '?' | '|' if !extended => {
                // Literal in BRE
                out.push('\\');
                out.push(c);
                i += 1;
                prev_atom = true;
                at_subexpr_start = false;
            }
            '^' if !extended => {
                if at_subexpr_start {
                    out.push('^');
                    prev_atom = false;
                } else {
                    out.push_str("\\^");
                    prev_atom = true;
                }
                at_subexpr_start = false;
                i += 1;
            }
            '$' if !extended => {
                // Anchor only at end of pattern or before `\)` / `\|`
                let is_anchor = matches!(
                    (chars.get(i + 1), chars.get(i + 2)),
                    (None, _) | (Some('\\'), Some(')' | '|'))
                );
                if is_anchor {
                    out.push('$');
                    prev_atom = false;
                } else {
                    out.push_str("\\$");
                    prev_atom = true;
                }
                at_subexpr_start = false;
                i += 1;
            }
            '*' if !extended && !prev_atom => {
                // A `*` with nothing to repeat is literal in BRE
                out.push_str("\\*");
                i += 1;
                prev_atom = true;
                at_subexpr_start = false;
            }
            _ => {
                out.push(c);
                i += 1;
                if !(c == '*' && !extended) {
                    prev_atom = true;
                }
                at_subexpr_start = false;
            }
        }
    }
    out
}

/// Copy a bracket expression (`[...]`) verbatim, respecting the POSIX
/// rules for a leading `^`/`]` and embedded `[:class:]`, `[=equiv=]`,
/// and `[.collate.]` forms.
fn copy_bracket_expression(chars: &[char], i: &mut usize, out: &mut String) {
    out.push('[');
    *i += 1;
    if chars.get(*i) == Some(&'^') {
        out.push('^');
        *i += 1;
    }
    if chars.get(*i) == Some(&']') {
        out.push(']');
        *i += 1;
    }
    while *i < chars.len() {
        let c = chars[*i];
        if c == '[' && matches!(chars.get(*i + 1), Some(':' | '.' | '=')) {
            let kind = chars[*i + 1];
            out.push('[');
            out.push(kind);
            *i += 2;
            while *i < chars.len() && !(chars[*i] == kind && chars.get(*i + 1) == Some(&']')) {
                out.push(chars[*i]);
                *i += 1;
            }
            if *i < chars.len() {
                out.push(kind);
                out.push(']');
                *i += 2;
            }
        } else if c == ']' {
            out.push(']');
            *i += 1;
            return;
        } else if c == '\\' && chars.get(*i + 1) == Some(&']') {
            // GNU treats a backslash before `]` as a literal backslash
            // (the `]` still closes the class); double it so the regex
            // crate doesn't read `\]` as an escaped bracket.
            out.push_str("\\\\");
            *i += 1;
        } else {
            out.push(c);
            *i += 1;
        }
    }
    // Unterminated bracket: everything was copied; the regex compiler
    // will report the error.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bre_swaps_group_escapes() {
        assert_eq!(translate_pattern(r"\(a\)", false), "(a)");
        assert_eq!(translate_pattern("(a)", false), r"\(a\)");
    }

    #[test]
    fn bre_swaps_repetition_escapes() {
        assert_eq!(translate_pattern(r"a\+b\?", false), "a+b?");
        assert_eq!(translate_pattern("a+b?", false), r"a\+b\?");
        assert_eq!(translate_pattern(r"a\{2,3\}", false), "a{2,3}");
        assert_eq!(translate_pattern("a{2}", false), r"a\{2\}");
    }

    #[test]
    fn bre_alternation() {
        assert_eq!(translate_pattern(r"a\|b", false), "a|b");
        assert_eq!(translate_pattern("a|b", false), r"a\|b");
    }

    #[test]
    fn bre_anchors() {
        assert_eq!(translate_pattern("^a$", false), "^a$");
        assert_eq!(translate_pattern("a^b", false), r"a\^b");
        assert_eq!(translate_pattern("a$b", false), r"a\$b");
        assert_eq!(translate_pattern(r"\(^a$\)", false), "(^a$)");
    }

    #[test]
    fn bre_leading_star_is_literal() {
        assert_eq!(translate_pattern("*a", false), r"\*a");
        assert_eq!(translate_pattern("a*", false), "a*");
        assert_eq!(translate_pattern(r"\(*\)", false), r"(\*)");
    }

    #[test]
    fn ere_passthrough() {
        assert_eq!(translate_pattern("(a|b)+c?", true), "(a|b)+c?");
        assert_eq!(translate_pattern("a{2,3}", true), "a{2,3}");
    }

    #[test]
    fn char_escapes_both_modes() {
        assert_eq!(translate_pattern(r"\x41", false), r"\x{41}");
        assert_eq!(translate_pattern(r"\o101", false), r"\x{41}");
        assert_eq!(translate_pattern(r"\d065", false), r"\x{41}");
        assert_eq!(translate_pattern(r"\x41", true), r"\x{41}");
    }

    #[test]
    fn digit_class_preserved_without_digits() {
        assert_eq!(translate_pattern(r"\d", false), r"\d");
        assert_eq!(translate_pattern(r"\w\s", true), r"\w\s");
    }

    #[test]
    fn bracket_expression_verbatim() {
        assert_eq!(translate_pattern("[a+?(]", false), "[a+?(]");
        assert_eq!(translate_pattern("[^]a]", false), "[^]a]");
        assert_eq!(translate_pattern("[[:alpha:]+]", false), "[[:alpha:]+]");
    }

    #[test]
    fn bracket_backslash_before_closing_is_literal() {
        // GNU: `[\]` is a class holding a literal backslash; the `]`
        // still closes it. The regex crate would read `\]` as an
        // escaped bracket, so the backslash must be doubled.
        assert_eq!(translate_pattern(r"[\]", false), r"[\\]");
        assert_eq!(translate_pattern(r"[a\]b", false), r"[a\\]b");
        // Other in-class escapes pass through (regex handles \n, \t…)
        assert_eq!(translate_pattern(r"[\n\t]", false), r"[\n\t]");
    }
}
