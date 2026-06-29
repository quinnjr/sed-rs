use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum Address {
    /// A specific line number (1-indexed)
    Line(usize),
    /// The last line of input
    Last,
    /// Lines matching a regex pattern
    Regex(String),
    /// GNU extension: first~step (every step-th line starting at first)
    Step { first: usize, step: usize },
}

#[derive(Debug, Clone)]
pub enum AddressRange {
    /// No address — matches every line
    None,
    /// Single address, optionally negated
    Single { addr: Address, negated: bool },
    /// Range of two addresses (inclusive), optionally negated
    Range {
        start: Address,
        end: Address,
        negated: bool,
    },
}

#[derive(Debug, Clone)]
pub struct SubstituteCmd {
    pub pattern: String,
    pub replacement: String,
    pub global: bool,
    pub print: bool,
    pub nth: Option<usize>,
    pub case_insensitive: bool,
    pub write_file: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Command {
    // -- Substitution & Transliteration --
    Substitute(SubstituteCmd),
    Transliterate { from: Vec<char>, to: Vec<char> },

    // -- Output --
    Print,
    PrintFirstLine,
    PrintLineNumber,
    List,

    // -- Deletion --
    Delete,
    DeleteFirstLine,

    // -- Input --
    Next,
    NextAppend,

    // -- Hold space --
    HoldReplace,
    HoldAppend,
    GetReplace,
    GetAppend,
    Exchange,

    // -- Text insertion --
    Append(String),
    Insert(String),
    Change(String),

    // -- Branching --
    Label(String),
    Branch(Option<String>),
    BranchIfSub(Option<String>),
    BranchIfNotSub(Option<String>),

    // -- I/O --
    ReadFile(String),
    /// GNU extension: read one line per cycle from a file (`R`)
    ReadLine(String),
    WriteFile(String),
    WriteFirstLine(String),
    /// GNU extension: print the current input file name (`F`)
    PrintFileName,
    /// GNU extension: execute a shell command (`e`).
    /// `None` runs the pattern space and replaces it with the output;
    /// `Some(cmd)` runs `cmd` and sends its output to the stream.
    Execute(Option<String>),

    // -- Control --
    Quit(Option<i32>),
    QuitNoprint(Option<i32>),
    ClearPattern,
    Noop,

    // -- Grouping --
    Block(Vec<SedCommand>),
}

#[derive(Debug, Clone)]
pub struct SedCommand {
    pub address: AddressRange,
    pub command: Command,
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

pub fn parse(script: &str) -> Result<Vec<SedCommand>> {
    let mut parser = Parser::new(script);
    parser.parse_script()
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn new(input: &str) -> Self {
        Self {
            chars: input.chars().collect(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.chars.get(self.pos).copied();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn is_at_end(&self) -> bool {
        self.pos >= self.chars.len()
    }

    /// Consume the next char if it matches `c`, returning true if consumed.
    fn consume_if(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Skip spaces and tabs (but NOT newlines).
    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.advance();
        }
    }

    /// Skip whitespace including newlines, semicolons, and comments.
    fn skip_blanks(&mut self) {
        loop {
            match self.peek() {
                Some(' ' | '\t' | '\n' | '\r' | ';') => {
                    self.advance();
                }
                Some('#') => self.skip_line(),
                _ => break,
            }
        }
    }

    /// Skip to end of current line.
    fn skip_line(&mut self) {
        while let Some(c) = self.advance() {
            if c == '\n' {
                break;
            }
        }
    }

    // -- Top-level parsing --

    fn parse_script(&mut self) -> Result<Vec<SedCommand>> {
        let mut commands = Vec::new();
        loop {
            self.skip_blanks();
            if self.is_at_end() {
                break;
            }
            if let Some(cmd) = self.parse_one_command()? {
                commands.push(cmd);
            }
        }
        Ok(commands)
    }

    fn parse_one_command(&mut self) -> Result<Option<SedCommand>> {
        self.skip_blanks();
        if self.is_at_end() {
            return Ok(None);
        }

        // Parse address range
        let address = self.parse_address_range()?;
        self.skip_spaces();

        let Some(ch) = self.advance() else {
            return Ok(None);
        };

        let command = match ch {
            '{' => {
                let block = self.parse_block()?;
                Command::Block(block)
            }
            '}' => return Ok(None), // handled by parse_block
            's' => self.parse_substitute()?,
            'y' => self.parse_transliterate()?,
            'd' => Command::Delete,
            'D' => Command::DeleteFirstLine,
            'p' => Command::Print,
            'P' => Command::PrintFirstLine,
            '=' => Command::PrintLineNumber,
            'l' => Command::List,
            'q' => Command::Quit(self.parse_optional_int()),
            'Q' => Command::QuitNoprint(self.parse_optional_int()),
            'h' => Command::HoldReplace,
            'H' => Command::HoldAppend,
            'g' => Command::GetReplace,
            'G' => Command::GetAppend,
            'x' => Command::Exchange,
            'n' => Command::Next,
            'N' => Command::NextAppend,
            'z' => Command::ClearPattern,
            'a' => Command::Append(self.parse_text_arg()),
            'i' => Command::Insert(self.parse_text_arg()),
            'c' => Command::Change(self.parse_text_arg()),
            'r' => Command::ReadFile(self.parse_filename_arg()),
            'w' => Command::WriteFile(self.parse_filename_arg()),
            'W' => Command::WriteFirstLine(self.parse_filename_arg()),
            'R' => Command::ReadLine(self.parse_filename_arg()),
            'F' => Command::PrintFileName,
            'e' => {
                let arg = self.parse_rest_of_line();
                Command::Execute(if arg.is_empty() {
                    None
                } else {
                    Some(arg)
                })
            }
            // GNU `v [version]` asserts a minimum sed version and enables
            // GNU extensions. They are always on here, so it's a no-op.
            // The version is a single token; `;` terminates it (unlike `e`).
            'v' => {
                let _ = self.parse_label_arg();
                Command::Noop
            }
            'b' => Command::Branch(self.parse_label_arg()),
            't' => Command::BranchIfSub(self.parse_label_arg()),
            'T' => Command::BranchIfNotSub(self.parse_label_arg()),
            ':' => {
                let label = self.parse_label_arg().unwrap_or_default();
                Command::Label(label)
            }
            '#' => {
                self.skip_line();
                Command::Noop
            }
            c if c.is_whitespace() => return self.parse_one_command(),
            c => {
                return Err(Error::Parse(format!("unknown command: '{c}'")));
            }
        };

        Ok(Some(SedCommand { address, command }))
    }

    // -- Address parsing --

    fn parse_address_range(&mut self) -> Result<AddressRange> {
        let Some(addr1) = self.parse_address()? else {
            // Check for negation without address
            if self.consume_if('!') {
                return Err(Error::Parse(
                    "'!' without preceding address".into(),
                ));
            }
            return Ok(AddressRange::None);
        };

        self.skip_spaces();

        if self.consume_if(',') {
            self.skip_spaces();
            let addr2 = self.parse_address()?.ok_or_else(|| {
                Error::Parse("expected address after ','".into())
            })?;
            self.skip_spaces();
            let negated = self.consume_if('!');
            Ok(AddressRange::Range {
                start: addr1,
                end: addr2,
                negated,
            })
        } else {
            let negated = self.consume_if('!');
            Ok(AddressRange::Single {
                addr: addr1,
                negated,
            })
        }
    }

    fn parse_address(&mut self) -> Result<Option<Address>> {
        match self.peek() {
            Some(c) if c.is_ascii_digit() => {
                let n = self.parse_number();
                if self.consume_if('~') {
                    let step = self.parse_number();
                    Ok(Some(Address::Step { first: n, step }))
                } else {
                    Ok(Some(Address::Line(n)))
                }
            }
            Some('$') => {
                self.advance();
                Ok(Some(Address::Last))
            }
            Some('/') => {
                self.advance();
                let pattern = self.parse_regex_delimited('/')?;
                Ok(Some(Address::Regex(pattern)))
            }
            Some('\\') => {
                self.advance();
                let delim = self.advance().ok_or_else(|| {
                    Error::Parse("expected delimiter after '\\'".into())
                })?;
                let pattern = self.parse_regex_delimited(delim)?;
                Ok(Some(Address::Regex(pattern)))
            }
            _ => Ok(None),
        }
    }

    fn parse_number(&mut self) -> usize {
        let mut n: usize = 0;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                n = n
                    .saturating_mul(10)
                    .saturating_add((c as u8 - b'0') as usize);
                self.advance();
            } else {
                break;
            }
        }
        n
    }

    fn parse_optional_int(&mut self) -> Option<i32> {
        self.skip_spaces();
        if self.peek().is_some_and(|c| c.is_ascii_digit()) {
            Some(self.parse_number() as i32)
        } else {
            None
        }
    }

    // -- Delimited content parsing --

    /// Parse content between matching delimiters, handling backslash escapes.
    fn parse_regex_delimited(&mut self, delim: char) -> Result<String> {
        let mut s = String::new();
        loop {
            match self.advance() {
                None => {
                    return Err(Error::Parse(format!(
                        "unterminated regex (expected closing '{delim}')"
                    )));
                }
                Some(c) if c == delim => return Ok(s),
                Some('\\') => {
                    if let Some(next) = self.advance() {
                        if next == delim {
                            // Escaped delimiter → literal delimiter in regex
                            s.push('\\');
                            s.push(next);
                        } else {
                            s.push('\\');
                            s.push(next);
                        }
                    } else {
                        s.push('\\');
                    }
                }
                Some(c) => s.push(c),
            }
        }
    }

    /// Parse content between delimiters for s/// replacement strings.
    /// Does NOT add extra backslash escaping (preserves sed replacement syntax).
    fn parse_replacement_delimited(&mut self, delim: char) -> Result<String> {
        let mut s = String::new();
        loop {
            match self.advance() {
                None => {
                    return Err(Error::Parse(format!(
                        "unterminated replacement (expected closing '{delim}')"
                    )));
                }
                Some(c) if c == delim => return Ok(s),
                Some('\\') => {
                    if let Some(next) = self.advance() {
                        if next == delim {
                            s.push(next);
                        } else {
                            s.push('\\');
                            s.push(next);
                        }
                    } else {
                        s.push('\\');
                    }
                }
                Some(c) => s.push(c),
            }
        }
    }

    // -- Command-specific parsing --

    fn parse_substitute(&mut self) -> Result<Command> {
        let delim = self.advance().ok_or_else(|| {
            Error::Parse("missing delimiter for s command".into())
        })?;
        let pattern = self.parse_regex_delimited(delim)?;
        let replacement = self.parse_replacement_delimited(delim)?;

        let mut global = false;
        let mut print = false;
        let mut nth: Option<usize> = None;
        let mut case_insensitive = false;
        let mut write_file = None;

        loop {
            match self.peek() {
                Some('g') => {
                    self.advance();
                    global = true;
                }
                Some('p') => {
                    self.advance();
                    print = true;
                }
                Some('i' | 'I') => {
                    self.advance();
                    case_insensitive = true;
                }
                Some('w') => {
                    self.advance();
                    self.skip_spaces();
                    write_file = Some(self.parse_filename_arg());
                    break;
                }
                Some(c) if c.is_ascii_digit() => {
                    nth = Some(self.parse_number());
                }
                _ => break,
            }
        }

        Ok(Command::Substitute(SubstituteCmd {
            pattern,
            replacement,
            global,
            print,
            nth,
            case_insensitive,
            write_file,
        }))
    }

    fn parse_transliterate(&mut self) -> Result<Command> {
        let delim = self.advance().ok_or_else(|| {
            Error::Parse("missing delimiter for y command".into())
        })?;
        let from_str = self.parse_regex_delimited(delim)?;
        let to_str = self.parse_regex_delimited(delim)?;

        let from: Vec<char> = from_str.chars().collect();
        let to: Vec<char> = to_str.chars().collect();

        if from.len() != to.len() {
            return Err(Error::Parse(format!(
                "y command: 'from' and 'to' must be same length ({} vs {})",
                from.len(),
                to.len()
            )));
        }

        Ok(Command::Transliterate { from, to })
    }

    /// Parse text argument for a/i/c commands.
    ///
    /// Handles:
    ///   a text       (GNU extension: text on same line)
    ///   a\ text      (text after backslash)
    ///   a\           (text on next line, with backslash continuation)
    fn parse_text_arg(&mut self) -> String {
        // Skip optional backslash
        if self.peek() == Some('\\') {
            self.advance();
        }

        // Skip one space or newline after command char / backslash
        match self.peek() {
            Some('\n') => {
                self.advance();
            }
            Some(' ' | '\t') => {
                self.advance();
            }
            _ => {}
        }

        let mut text = String::new();
        loop {
            match self.peek() {
                None => break,
                Some('\n') => {
                    // Check for backslash continuation
                    if text.ends_with('\\') {
                        text.pop();
                        text.push('\n');
                        self.advance();
                    } else {
                        break;
                    }
                }
                Some(c) => {
                    text.push(c);
                    self.advance();
                }
            }
        }

        text
    }

    /// Parse a label name (for b, t, T, : commands).
    fn parse_label_arg(&mut self) -> Option<String> {
        self.skip_spaces();
        let mut label = String::new();
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' || c == '.' || c == '-' {
                label.push(c);
                self.advance();
            } else {
                break;
            }
        }
        if label.is_empty() {
            None
        } else {
            Some(label)
        }
    }

    /// Parse a filename argument (for r, w, W commands).
    fn parse_filename_arg(&mut self) -> String {
        self.skip_spaces();
        let mut filename = String::new();
        while let Some(c) = self.peek() {
            if c == '\n' || c == ';' {
                break;
            }
            filename.push(c);
            self.advance();
        }
        filename.trim_end().to_string()
    }

    /// Parse the rest of the line as a single argument (for `e`, `v`).
    /// Unlike `parse_filename_arg`, `;` is not a terminator — the
    /// argument runs to the end of the line, matching GNU sed.
    fn parse_rest_of_line(&mut self) -> String {
        self.skip_spaces();
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c == '\n' {
                break;
            }
            s.push(c);
            self.advance();
        }
        s.trim_end().to_string()
    }

    /// Parse a { ... } block of commands.
    fn parse_block(&mut self) -> Result<Vec<SedCommand>> {
        let mut commands = Vec::new();
        loop {
            self.skip_blanks();
            if self.is_at_end() {
                return Err(Error::Parse(
                    "unterminated block (missing '}')".into(),
                ));
            }
            if self.peek() == Some('}') {
                self.advance();
                break;
            }
            if let Some(cmd) = self.parse_one_command()? {
                commands.push(cmd);
            }
        }
        Ok(commands)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_substitute() {
        let cmds = parse("s/foo/bar/g").unwrap();
        assert_eq!(cmds.len(), 1);
        match &cmds[0].command {
            Command::Substitute(s) => {
                assert_eq!(s.pattern, "foo");
                assert_eq!(s.replacement, "bar");
                assert!(s.global);
            }
            other => panic!("expected Substitute, got {other:?}"),
        }
    }

    #[test]
    fn parse_substitute_custom_delim() {
        let cmds = parse("s|foo|bar|").unwrap();
        assert_eq!(cmds.len(), 1);
        match &cmds[0].command {
            Command::Substitute(s) => {
                assert_eq!(s.pattern, "foo");
                assert_eq!(s.replacement, "bar");
                assert!(!s.global);
            }
            other => panic!("expected Substitute, got {other:?}"),
        }
    }

    #[test]
    fn parse_address_line() {
        let cmds = parse("3d").unwrap();
        assert_eq!(cmds.len(), 1);
        match &cmds[0].address {
            AddressRange::Single {
                addr: Address::Line(3),
                negated: false,
            } => {}
            other => panic!("unexpected address: {other:?}"),
        }
    }

    #[test]
    fn parse_address_range_lines() {
        let cmds = parse("1,10d").unwrap();
        assert_eq!(cmds.len(), 1);
        match &cmds[0].address {
            AddressRange::Range {
                start: Address::Line(1),
                end: Address::Line(10),
                negated: false,
            } => {}
            other => panic!("unexpected address: {other:?}"),
        }
    }

    #[test]
    fn parse_address_regex() {
        let cmds = parse("/^foo/d").unwrap();
        assert_eq!(cmds.len(), 1);
        match &cmds[0].address {
            AddressRange::Single {
                addr: Address::Regex(re),
                negated: false,
            } => assert_eq!(re, "^foo"),
            other => panic!("unexpected address: {other:?}"),
        }
    }

    #[test]
    fn parse_negated() {
        let cmds = parse("/foo/!d").unwrap();
        assert_eq!(cmds.len(), 1);
        match &cmds[0].address {
            AddressRange::Single {
                addr: Address::Regex(_),
                negated: true,
            } => {}
            other => panic!("unexpected address: {other:?}"),
        }
    }

    #[test]
    fn parse_multiple_commands() {
        let cmds = parse("s/a/b/; s/c/d/").unwrap();
        assert_eq!(cmds.len(), 2);
    }

    #[test]
    fn parse_block() {
        let cmds = parse("/foo/ { s/a/b/; s/c/d/ }").unwrap();
        assert_eq!(cmds.len(), 1);
        match &cmds[0].command {
            Command::Block(block) => assert_eq!(block.len(), 2),
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn parse_transliterate() {
        let cmds = parse("y/abc/xyz/").unwrap();
        assert_eq!(cmds.len(), 1);
        match &cmds[0].command {
            Command::Transliterate { from, to } => {
                assert_eq!(from, &['a', 'b', 'c']);
                assert_eq!(to, &['x', 'y', 'z']);
            }
            other => panic!("expected Transliterate, got {other:?}"),
        }
    }

    #[test]
    fn parse_labels_and_branches() {
        let cmds = parse(":loop\ns/foo/bar/\nt loop").unwrap();
        assert_eq!(cmds.len(), 3);
        match &cmds[0].command {
            Command::Label(l) => assert_eq!(l, "loop"),
            other => panic!("expected Label, got {other:?}"),
        }
        match &cmds[2].command {
            Command::BranchIfSub(Some(l)) => assert_eq!(l, "loop"),
            other => panic!("expected BranchIfSub, got {other:?}"),
        }
    }

    #[test]
    fn parse_append_text() {
        let cmds = parse("a hello world").unwrap();
        assert_eq!(cmds.len(), 1);
        match &cmds[0].command {
            Command::Append(t) => assert_eq!(t, "hello world"),
            other => panic!("expected Append, got {other:?}"),
        }
    }

    #[test]
    fn parse_last_line_address() {
        let cmds = parse("$d").unwrap();
        assert_eq!(cmds.len(), 1);
        match &cmds[0].address {
            AddressRange::Single {
                addr: Address::Last,
                ..
            } => {}
            other => panic!("expected Last address, got {other:?}"),
        }
    }

    #[test]
    fn parse_step_address() {
        let cmds = parse("0~2d").unwrap();
        assert_eq!(cmds.len(), 1);
        match &cmds[0].address {
            AddressRange::Single {
                addr: Address::Step { first: 0, step: 2 },
                ..
            } => {}
            other => panic!("expected Step address, got {other:?}"),
        }
    }
}
