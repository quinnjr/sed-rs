// The sed execution engine.
//
// Compiles parsed commands into an internal representation with pre-compiled
// regexes, then processes input line-by-line applying the sed cycle:
//   1. Read a line into the pattern space
//   2. Execute all commands
//   3. Unless -n, print the pattern space
//   4. Flush any queued output (from a, r, etc.)

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::Path;

use regex::Regex;

use crate::cli::Options;
use crate::command::{Address, AddressRange, Command, SedCommand};
use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// Compiled command representation
// ---------------------------------------------------------------------------

enum CompiledAddress {
    Line(usize),
    Last,
    Regex(Regex),
    Step { first: usize, step: usize },
}

struct CompiledAddressRange {
    kind: CompiledAddressKind,
    negated: bool,
}

enum CompiledAddressKind {
    None,
    Single(CompiledAddress),
    Range(CompiledAddress, CompiledAddress),
}

impl CompiledAddressRange {
    fn none() -> Self {
        Self {
            kind: CompiledAddressKind::None,
            negated: false,
        }
    }
}

struct CompiledSubstitute {
    pattern: Regex,
    replacement: String,
    global: bool,
    print: bool,
    nth: Option<usize>,
    write_file: Option<String>,
}

enum CompiledCommand {
    ScopeStart,
    ScopeEnd,
    Substitute(CompiledSubstitute),
    Transliterate { from: Vec<char>, to: Vec<char> },
    Print,
    PrintFirstLine,
    PrintLineNumber,
    List,
    Delete,
    DeleteFirstLine,
    Next,
    NextAppend,
    HoldReplace,
    HoldAppend,
    GetReplace,
    GetAppend,
    Exchange,
    Append(String),
    Insert(String),
    Change(String),
    #[allow(dead_code)]
    Label(String),
    Branch(Option<String>),
    BranchIfSub(Option<String>),
    BranchIfNotSub(Option<String>),
    ReadFile(String),
    WriteFile(String),
    Quit(Option<i32>),
    QuitNoprint(Option<i32>),
    ClearPattern,
    Noop,
}

struct CompiledSedCommand {
    address: CompiledAddressRange,
    command: CompiledCommand,
}

// ---------------------------------------------------------------------------
// Flow control
// ---------------------------------------------------------------------------

enum Flow {
    /// Continue to next command
    Continue,
    /// Restart the cycle — read next line (from `d`)
    Restart,
    /// Restart script on current pattern space without reading (from `D`)
    RestartScript,
    /// Branch to a label or end of script
    Branch(Option<String>),
    /// Quit processing
    Quit(i32),
    /// Quit without printing
    QuitNoPrint(i32),
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

pub struct Engine {
    commands: Vec<CompiledSedCommand>,
    labels: HashMap<String, usize>,
    quiet: bool,
    null_data: bool,
}

/// Mutable state during execution
struct State {
    line_number: usize,
    is_last_line: bool,
    pattern_space: String,
    hold_space: String,
    append_queue: Vec<String>,
    last_sub_success: bool,
    /// Tracks whether each range-addressed command is currently "in range"
    range_active: Vec<bool>,
}

impl State {
    fn new(num_commands: usize) -> Self {
        Self {
            line_number: 0,
            is_last_line: false,
            pattern_space: String::new(),
            hold_space: String::new(),
            append_queue: Vec::new(),
            last_sub_success: false,
            range_active: vec![false; num_commands],
        }
    }
}

impl Engine {
    pub fn new(
        parsed: Vec<SedCommand>,
        options: &Options,
    ) -> Result<Self> {
        let (commands, labels) = compile_commands(parsed)?;
        Ok(Self {
            commands,
            labels,
            quiet: options.quiet,
            null_data: options.null_data,
        })
    }

    /// Write a string followed by the appropriate line terminator
    /// (NUL in -z mode, newline otherwise).
    fn write_line<W: Write>(
        &self,
        writer: &mut W,
        s: &str,
    ) -> Result<()> {
        write!(writer, "{}", s)?;
        if self.null_data {
            writer.write_all(b"\0")?;
        } else {
            writer.write_all(b"\n")?;
        }
        Ok(())
    }

    /// Process files (or stdin if empty) and write results to stdout.
    pub fn run(&self, files: &[std::path::PathBuf]) -> Result<()> {
        let stdout = io::stdout();
        let mut out = io::BufWriter::new(stdout.lock());

        if files.is_empty() {
            let stdin = io::stdin();
            let reader = stdin.lock();
            self.process_stream(reader, &mut out)?;
        } else {
            for path in files {
                if !path.exists() {
                    eprintln!(
                        "sed: can't read {}: No such file or directory",
                        path.display()
                    );
                    continue;
                }
                let file = fs::File::open(path)?;
                let reader = io::BufReader::new(file);
                self.process_stream(reader, &mut out)?;
            }
        }

        out.flush()?;
        Ok(())
    }

    /// Process files in-place, optionally creating backups.
    pub fn run_in_place(
        &self,
        files: &[std::path::PathBuf],
        backup_suffix: &str,
    ) -> Result<()> {
        for path in files {
            if !path.exists() {
                eprintln!(
                    "sed: can't read {}: No such file or directory",
                    path.display()
                );
                continue;
            }

            // Read the file
            let file = fs::File::open(path)?;
            let reader = io::BufReader::new(file);
            let mut output = Vec::new();
            {
                let mut cursor = io::Cursor::new(&mut output);
                self.process_stream(reader, &mut cursor)?;
            }

            // Create backup if suffix is non-empty
            if !backup_suffix.is_empty() {
                let backup_path = format!(
                    "{}{}",
                    path.display(),
                    backup_suffix
                );
                fs::copy(path, &backup_path)?;
            }

            // Write atomically using tempfile (adapted from sd)
            write_atomic(path, &output)?;
        }
        Ok(())
    }

    /// Process input from a buffered reader and write output to a writer.
    ///
    /// This is the core sed execution loop. It reads lines (or NUL-delimited
    /// records in `-z` mode), applies the compiled commands, and writes the
    /// results.
    pub fn process_stream<R: BufRead, W: Write>(
        &self,
        reader: R,
        writer: &mut W,
    ) -> Result<()> {
        let mut line_reader = LineReader::new(reader, self.null_data);
        let mut state = State::new(self.commands.len());

        while let Some((line, is_last)) = line_reader.read_line()? {
            state.line_number += 1;
            state.is_last_line = is_last;
            state.pattern_space = line;
            state.append_queue.clear();

            // Inner loop: allows `D` to re-run the script on the
            // remaining pattern space without reading a new input line.
            loop {
                match self.execute_all(
                    &mut state,
                    &mut line_reader,
                    writer,
                )? {
                    Flow::Restart => {
                        // `d` command: skip printing, flush appends,
                        // break to read next line
                        self.flush_appends(&state, writer)?;
                        break;
                    }
                    Flow::RestartScript => {
                        // `D` command: re-run the script on the
                        // remaining pattern space (no new read)
                        state.append_queue.clear();
                        continue;
                    }
                    Flow::Quit(code) => {
                        // Print pattern space then quit
                        if !self.quiet {
                            self.write_line(
                                writer,
                                &state.pattern_space,
                            )?;
                        }
                        self.flush_appends(&state, writer)?;
                        if code != 0 {
                            std::process::exit(code);
                        }
                        return Ok(());
                    }
                    Flow::QuitNoPrint(code) => {
                        if code != 0 {
                            std::process::exit(code);
                        }
                        return Ok(());
                    }
                    Flow::Continue | Flow::Branch(_) => {
                        if !self.quiet {
                            self.write_line(
                                writer,
                                &state.pattern_space,
                            )?;
                        }
                        self.flush_appends(&state, writer)?;
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    fn execute_all<R: BufRead, W: Write>(
        &self,
        state: &mut State,
        line_reader: &mut LineReader<R>,
        writer: &mut W,
    ) -> Result<Flow> {
        let mut pc: usize = 0;
        let mut scope_depth: usize = 0;
        let mut skip_depth: Option<usize> = None;

        // Reset sub success flag at start of each cycle
        state.last_sub_success = false;

        while pc < self.commands.len() {
            let cmd = &self.commands[pc];

            // -- Scope tracking --
            if let Some(sd) = skip_depth {
                match &cmd.command {
                    CompiledCommand::ScopeStart => {
                        scope_depth += 1;
                    }
                    CompiledCommand::ScopeEnd => {
                        if scope_depth == sd {
                            skip_depth = None;
                        }
                        scope_depth = scope_depth.saturating_sub(1);
                    }
                    _ => {}
                }
                pc += 1;
                continue;
            }

            match &cmd.command {
                CompiledCommand::ScopeStart => {
                    scope_depth += 1;
                    if !self.address_matches(
                        &cmd.address,
                        state,
                        pc,
                    ) {
                        skip_depth = Some(scope_depth);
                    }
                    pc += 1;
                    continue;
                }
                CompiledCommand::ScopeEnd => {
                    scope_depth = scope_depth.saturating_sub(1);
                    pc += 1;
                    continue;
                }
                _ => {}
            }

            // Check if this command's address matches
            if !self.address_matches(&cmd.address, state, pc) {
                pc += 1;
                continue;
            }

            // Execute the command
            let flow = self.execute_one(
                &cmd.command,
                state,
                line_reader,
                writer,
            )?;

            match flow {
                Flow::Continue => {
                    pc += 1;
                }
                Flow::Restart => return Ok(Flow::Restart),
                Flow::RestartScript => {
                    return Ok(Flow::RestartScript)
                }
                Flow::Quit(c) => return Ok(Flow::Quit(c)),
                Flow::QuitNoPrint(c) => return Ok(Flow::QuitNoPrint(c)),
                Flow::Branch(ref label) => {
                    if let Some(name) = label {
                        if let Some(&target) = self.labels.get(name) {
                            pc = target;
                            continue;
                        }
                    }
                    // Branch to end of script
                    return Ok(Flow::Continue);
                }
            }
        }

        Ok(Flow::Continue)
    }

    fn execute_one<R: BufRead, W: Write>(
        &self,
        command: &CompiledCommand,
        state: &mut State,
        line_reader: &mut LineReader<R>,
        writer: &mut W,
    ) -> Result<Flow> {
        match command {
            CompiledCommand::Substitute(sub) => {
                let (result, matched) = apply_substitution(
                    &sub.pattern,
                    &state.pattern_space,
                    &sub.replacement,
                    sub.global,
                    sub.nth,
                );
                if matched {
                    state.pattern_space = result;
                    state.last_sub_success = true;
                    if sub.print {
                        self.write_line(
                            writer,
                            &state.pattern_space,
                        )?;
                    }
                    if let Some(ref path) = sub.write_file {
                        let mut f = fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(path)?;
                        writeln!(f, "{}", state.pattern_space)?;
                    }
                }
            }

            CompiledCommand::Transliterate { from, to } => {
                let mut new = String::with_capacity(
                    state.pattern_space.len(),
                );
                for c in state.pattern_space.chars() {
                    if let Some(pos) = from.iter().position(|&f| f == c) {
                        new.push(to[pos]);
                    } else {
                        new.push(c);
                    }
                }
                state.pattern_space = new;
            }

            CompiledCommand::Delete => return Ok(Flow::Restart),

            CompiledCommand::DeleteFirstLine => {
                if let Some(pos) = state.pattern_space.find('\n') {
                    state.pattern_space =
                        state.pattern_space[pos + 1..].to_string();
                    // Re-run the script on the remaining pattern space
                    // without reading a new input line
                    return Ok(Flow::RestartScript);
                } else {
                    // No newline: same as `d` — discard and read next
                    return Ok(Flow::Restart);
                }
            }

            CompiledCommand::Print => {
                self.write_line(writer, &state.pattern_space)?;
            }

            CompiledCommand::PrintFirstLine => {
                if let Some(pos) = state.pattern_space.find('\n') {
                    self.write_line(
                        writer,
                        &state.pattern_space[..pos],
                    )?;
                } else {
                    self.write_line(writer, &state.pattern_space)?;
                }
            }

            CompiledCommand::PrintLineNumber => {
                self.write_line(
                    writer,
                    &state.line_number.to_string(),
                )?;
            }

            CompiledCommand::List => {
                // Print pattern space with non-printing chars shown
                let listed = list_escape(&state.pattern_space);
                self.write_line(writer, &format!("{}$", listed))?;
            }

            CompiledCommand::Next => {
                // Print current pattern space (if not -n)
                if !self.quiet {
                    self.write_line(writer, &state.pattern_space)?;
                }
                self.flush_appends(state, writer)?;

                // Read next line
                if let Some((line, is_last)) = line_reader.read_line()? {
                    state.line_number += 1;
                    state.is_last_line = is_last;
                    state.pattern_space = line;
                } else {
                    // No more input — we already printed, so exit
                    // without the auto-print at end of cycle
                    return Ok(Flow::QuitNoPrint(0));
                }
            }

            CompiledCommand::NextAppend => {
                if let Some((line, is_last)) = line_reader.read_line()? {
                    state.line_number += 1;
                    state.is_last_line = is_last;
                    state.pattern_space.push('\n');
                    state.pattern_space.push_str(&line);
                } else {
                    // Default: print and quit if no more input
                    if !self.quiet {
                        self.write_line(
                            writer,
                            &state.pattern_space,
                        )?;
                    }
                    return Ok(Flow::QuitNoPrint(0));
                }
            }

            CompiledCommand::HoldReplace => {
                state.hold_space = state.pattern_space.clone();
            }
            CompiledCommand::HoldAppend => {
                state.hold_space.push('\n');
                state.hold_space.push_str(&state.pattern_space);
            }
            CompiledCommand::GetReplace => {
                state.pattern_space = state.hold_space.clone();
            }
            CompiledCommand::GetAppend => {
                state.pattern_space.push('\n');
                let hold = state.hold_space.clone();
                state.pattern_space.push_str(&hold);
            }
            CompiledCommand::Exchange => {
                std::mem::swap(
                    &mut state.pattern_space,
                    &mut state.hold_space,
                );
            }

            CompiledCommand::Append(text) => {
                state.append_queue.push(text.clone());
            }
            CompiledCommand::Insert(text) => {
                self.write_line(writer, text)?;
            }
            CompiledCommand::Change(text) => {
                // Output the replacement text, then restart the cycle
                // (skipping the auto-print of the pattern space)
                self.write_line(writer, text)?;
                return Ok(Flow::Restart);
            }

            CompiledCommand::Branch(label) => {
                return Ok(Flow::Branch(label.clone()));
            }
            CompiledCommand::BranchIfSub(label) => {
                if state.last_sub_success {
                    state.last_sub_success = false;
                    return Ok(Flow::Branch(label.clone()));
                }
            }
            CompiledCommand::BranchIfNotSub(label) => {
                if !state.last_sub_success {
                    state.last_sub_success = false;
                    return Ok(Flow::Branch(label.clone()));
                }
            }

            CompiledCommand::ReadFile(path) => {
                if let Ok(content) = fs::read_to_string(path) {
                    state.append_queue.push(content.trim_end().to_string());
                }
            }
            CompiledCommand::WriteFile(path) => {
                let mut f = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)?;
                writeln!(f, "{}", state.pattern_space)?;
            }

            CompiledCommand::Quit(code) => {
                return Ok(Flow::Quit(code.unwrap_or(0)));
            }
            CompiledCommand::QuitNoprint(code) => {
                return Ok(Flow::QuitNoPrint(code.unwrap_or(0)));
            }
            CompiledCommand::ClearPattern => {
                state.pattern_space.clear();
            }

            CompiledCommand::Label(_)
            | CompiledCommand::Noop
            | CompiledCommand::ScopeStart
            | CompiledCommand::ScopeEnd => {}
        }

        Ok(Flow::Continue)
    }

    fn address_matches(
        &self,
        addr: &CompiledAddressRange,
        state: &mut State,
        cmd_index: usize,
    ) -> bool {
        let raw = match &addr.kind {
            CompiledAddressKind::None => true,
            CompiledAddressKind::Single(a) => {
                addr_matches_line(a, state)
            }
            CompiledAddressKind::Range(start, end) => {
                let active = state
                    .range_active
                    .get(cmd_index)
                    .copied()
                    .unwrap_or(false);

                if active {
                    // We're in the range; check if end matches
                    if addr_matches_line(end, state) {
                        // End of range — still in range for this line
                        if let Some(v) =
                            state.range_active.get_mut(cmd_index)
                        {
                            *v = false;
                        }
                    }
                    true
                } else if addr_matches_line(start, state) {
                    // Start of range — activate it.
                    if let Some(v) =
                        state.range_active.get_mut(cmd_index)
                    {
                        *v = true;
                    }
                    // Per POSIX/GNU: regex end addresses are NOT
                    // checked on the start line. But line-number end
                    // addresses that are already at or past the
                    // current line close the range immediately (GNU
                    // extension: addr2 <= addr1 means one-line range).
                    if is_line_addr_at_or_before(end, state) {
                        if let Some(v) =
                            state.range_active.get_mut(cmd_index)
                        {
                            *v = false;
                        }
                    }
                    true
                } else {
                    false
                }
            }
        };

        if addr.negated { !raw } else { raw }
    }

    fn flush_appends<W: Write>(
        &self,
        state: &State,
        writer: &mut W,
    ) -> Result<()> {
        for text in &state.append_queue {
            self.write_line(writer, text)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Address matching helper
// ---------------------------------------------------------------------------

/// Check if the address is a line number at or before the current line.
/// Used for immediate range closure when the end address is a line number.
fn is_line_addr_at_or_before(
    addr: &CompiledAddress,
    state: &State,
) -> bool {
    match addr {
        CompiledAddress::Line(n) => *n <= state.line_number,
        _ => false,
    }
}

fn addr_matches_line(addr: &CompiledAddress, state: &State) -> bool {
    match addr {
        CompiledAddress::Line(n) => state.line_number == *n,
        CompiledAddress::Last => state.is_last_line,
        CompiledAddress::Regex(re) => re.is_match(&state.pattern_space),
        CompiledAddress::Step { first, step } => {
            if *step == 0 {
                state.line_number == *first
            } else if *first == 0 {
                state.line_number % step == 0
            } else {
                state.line_number >= *first
                    && (state.line_number - first) % step == 0
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Substitution (inspired by sd's replacer)
// ---------------------------------------------------------------------------

/// Apply a sed-style substitution. Returns the new string and whether any
/// replacement was made.
fn apply_substitution(
    regex: &Regex,
    text: &str,
    replacement: &str,
    global: bool,
    nth: Option<usize>,
) -> (String, bool) {
    let mut result = String::with_capacity(text.len());
    let mut last_end = 0;
    let mut match_count: usize = 0;
    let mut made_substitution = false;

    for captures in regex.captures_iter(text) {
        let whole = captures.get(0).unwrap();
        match_count += 1;

        let should_replace = match nth {
            Some(n) => match_count == n,
            None => global || match_count == 1,
        };

        if should_replace {
            result.push_str(&text[last_end..whole.start()]);
            apply_sed_replacement(&captures, replacement, &mut result);
            last_end = whole.end();
            made_substitution = true;
            if !global {
                break;
            }
        } else if !global && nth.is_none() {
            break;
        }
    }

    result.push_str(&text[last_end..]);
    (result, made_substitution)
}

/// Interpret a sed-style replacement string against captures.
///
/// Handles:
///   &       → whole match ($0)
///   \1..\9  → numbered capture group
///   \n      → newline
///   \t      → tab
///   \\      → literal backslash
///   \&      → literal &
fn apply_sed_replacement(
    captures: &regex::Captures,
    replacement: &str,
    output: &mut String,
) {
    let mut chars = replacement.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(&next) = chars.peek() {
                    match next {
                        '0'..='9' => {
                            chars.next();
                            let group = (next as u8 - b'0') as usize;
                            if let Some(m) = captures.get(group) {
                                output.push_str(m.as_str());
                            }
                        }
                        'n' => {
                            chars.next();
                            output.push('\n');
                        }
                        't' => {
                            chars.next();
                            output.push('\t');
                        }
                        '\\' => {
                            chars.next();
                            output.push('\\');
                        }
                        '&' => {
                            chars.next();
                            output.push('&');
                        }
                        _ => {
                            // Not a recognized escape; pass through
                            output.push('\\');
                        }
                    }
                } else {
                    output.push('\\');
                }
            }
            '&' => {
                if let Some(m) = captures.get(0) {
                    output.push_str(m.as_str());
                }
            }
            _ => output.push(c),
        }
    }
}

// ---------------------------------------------------------------------------
// List escaping (for `l` command)
// ---------------------------------------------------------------------------

fn list_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\x07' => out.push_str("\\a"),
            '\x08' => out.push_str("\\b"),
            '\x0C' => out.push_str("\\f"),
            '\x1B' => out.push_str("\\e"),
            c if c.is_control() => {
                out.push_str(&format!("\\x{:02X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Line reader with lookahead (to detect last line for $ address)
// ---------------------------------------------------------------------------

struct LineReader<R: BufRead> {
    reader: R,
    buf: String,
    pending: Option<String>,
    exhausted: bool,
    null_data: bool,
}

impl<R: BufRead> LineReader<R> {
    fn new(mut reader: R, null_data: bool) -> Self {
        let mut buf = String::new();
        let pending = if null_data {
            read_until_null(&mut reader, &mut buf)
        } else {
            match reader.read_line(&mut buf) {
                Ok(0) => None,
                Ok(_) => Some(chomp(&buf)),
                Err(_) => None,
            }
        };

        let exhausted = pending.is_none();
        Self {
            reader,
            buf: String::new(),
            pending,
            exhausted,
            null_data,
        }
    }

    /// Returns (line_content, is_last_line)
    fn read_line(&mut self) -> Result<Option<(String, bool)>> {
        let current = match self.pending.take() {
            Some(line) => line,
            None => return Ok(None),
        };

        // Read-ahead to determine if `current` is the last line
        self.buf.clear();
        let next = if self.null_data {
            read_until_null(&mut self.reader, &mut self.buf)
        } else {
            match self.reader.read_line(&mut self.buf) {
                Ok(0) => None,
                Ok(_) => Some(chomp(&self.buf)),
                Err(e) => return Err(Error::Io(e)),
            }
        };

        let is_last = next.is_none();
        self.pending = next;
        self.exhausted = is_last;

        Ok(Some((current, is_last)))
    }
}

/// Remove trailing newline (LF or CRLF) from a line.
fn chomp(s: &str) -> String {
    let mut s = s.to_string();
    if s.ends_with('\n') {
        s.pop();
        if s.ends_with('\r') {
            s.pop();
        }
    }
    s
}

/// Read until NUL byte for -z/--null-data mode.
fn read_until_null<R: BufRead>(
    reader: &mut R,
    buf: &mut String,
) -> Option<String> {
    buf.clear();
    let mut byte_buf = Vec::new();
    match reader.read_until(b'\0', &mut byte_buf) {
        Ok(0) => None,
        Ok(_) => {
            // Remove trailing NUL
            if byte_buf.last() == Some(&b'\0') {
                byte_buf.pop();
            }
            Some(String::from_utf8_lossy(&byte_buf).into_owned())
        }
        Err(_) => None,
    }
}

// ---------------------------------------------------------------------------
// Atomic file write (adapted from sd)
// ---------------------------------------------------------------------------

fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let path = fs::canonicalize(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| Error::InvalidPath(path.to_path_buf()))?;

    let temp = tempfile::NamedTempFile::new_in(parent)?;
    let file = temp.as_file();

    // Copy permissions from original
    if let Ok(metadata) = fs::metadata(&path) {
        file.set_permissions(metadata.permissions()).ok();

        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, fchown};
            let _ =
                fchown(file, Some(metadata.uid()), Some(metadata.gid()));
        }
    }

    let mut writer = io::BufWriter::new(file);
    writer.write_all(data)?;
    writer.flush()?;
    drop(writer);

    temp.persist(&path)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Compilation: parsed commands → compiled commands
// ---------------------------------------------------------------------------

fn compile_commands(
    parsed: Vec<SedCommand>,
) -> Result<(Vec<CompiledSedCommand>, HashMap<String, usize>)> {
    let mut compiled = Vec::new();
    let mut labels = HashMap::new();
    flatten_and_compile(parsed, &mut compiled, &mut labels)?;
    Ok((compiled, labels))
}

fn flatten_and_compile(
    commands: Vec<SedCommand>,
    compiled: &mut Vec<CompiledSedCommand>,
    labels: &mut HashMap<String, usize>,
) -> Result<()> {
    for cmd in commands {
        match cmd.command {
            Command::Block(block) => {
                let addr = compile_address_range(cmd.address)?;
                compiled.push(CompiledSedCommand {
                    address: addr,
                    command: CompiledCommand::ScopeStart,
                });
                flatten_and_compile(block, compiled, labels)?;
                compiled.push(CompiledSedCommand {
                    address: CompiledAddressRange::none(),
                    command: CompiledCommand::ScopeEnd,
                });
            }
            Command::Label(ref name) => {
                labels.insert(name.clone(), compiled.len());
                compiled.push(CompiledSedCommand {
                    address: CompiledAddressRange::none(),
                    command: CompiledCommand::Label(name.clone()),
                });
            }
            other => {
                let addr = compile_address_range(cmd.address)?;
                let cc = compile_single_command(other)?;
                compiled.push(CompiledSedCommand {
                    address: addr,
                    command: cc,
                });
            }
        }
    }
    Ok(())
}

fn compile_address_range(
    range: AddressRange,
) -> Result<CompiledAddressRange> {
    match range {
        AddressRange::None => Ok(CompiledAddressRange::none()),
        AddressRange::Single { addr, negated } => {
            Ok(CompiledAddressRange {
                kind: CompiledAddressKind::Single(
                    compile_address(addr)?,
                ),
                negated,
            })
        }
        AddressRange::Range {
            start,
            end,
            negated,
        } => Ok(CompiledAddressRange {
            kind: CompiledAddressKind::Range(
                compile_address(start)?,
                compile_address(end)?,
            ),
            negated,
        }),
    }
}

fn compile_address(addr: Address) -> Result<CompiledAddress> {
    match addr {
        Address::Line(n) => Ok(CompiledAddress::Line(n)),
        Address::Last => Ok(CompiledAddress::Last),
        Address::Regex(pattern) => {
            let re = regex::RegexBuilder::new(&pattern)
                .multi_line(true)
                .build()?;
            Ok(CompiledAddress::Regex(re))
        }
        Address::Step { first, step } => {
            Ok(CompiledAddress::Step { first, step })
        }
    }
}

fn compile_single_command(cmd: Command) -> Result<CompiledCommand> {
    match cmd {
        Command::Substitute(sub) => {
            let re = regex::RegexBuilder::new(&sub.pattern)
                .case_insensitive(sub.case_insensitive)
                .multi_line(true)
                .build()?;
            Ok(CompiledCommand::Substitute(CompiledSubstitute {
                pattern: re,
                replacement: sub.replacement,
                global: sub.global,
                print: sub.print,
                nth: sub.nth,
                write_file: sub.write_file,
            }))
        }
        Command::Transliterate { from, to } => {
            Ok(CompiledCommand::Transliterate { from, to })
        }
        Command::Print => Ok(CompiledCommand::Print),
        Command::PrintFirstLine => Ok(CompiledCommand::PrintFirstLine),
        Command::PrintLineNumber => Ok(CompiledCommand::PrintLineNumber),
        Command::List => Ok(CompiledCommand::List),
        Command::Delete => Ok(CompiledCommand::Delete),
        Command::DeleteFirstLine => {
            Ok(CompiledCommand::DeleteFirstLine)
        }
        Command::Next => Ok(CompiledCommand::Next),
        Command::NextAppend => Ok(CompiledCommand::NextAppend),
        Command::HoldReplace => Ok(CompiledCommand::HoldReplace),
        Command::HoldAppend => Ok(CompiledCommand::HoldAppend),
        Command::GetReplace => Ok(CompiledCommand::GetReplace),
        Command::GetAppend => Ok(CompiledCommand::GetAppend),
        Command::Exchange => Ok(CompiledCommand::Exchange),
        Command::Append(t) => Ok(CompiledCommand::Append(t)),
        Command::Insert(t) => Ok(CompiledCommand::Insert(t)),
        Command::Change(t) => Ok(CompiledCommand::Change(t)),
        Command::Branch(l) => Ok(CompiledCommand::Branch(l)),
        Command::BranchIfSub(l) => Ok(CompiledCommand::BranchIfSub(l)),
        Command::BranchIfNotSub(l) => {
            Ok(CompiledCommand::BranchIfNotSub(l))
        }
        Command::ReadFile(f) => Ok(CompiledCommand::ReadFile(f)),
        Command::WriteFile(f) => Ok(CompiledCommand::WriteFile(f)),
        Command::WriteFirstLine(f) => {
            Ok(CompiledCommand::WriteFile(f))
        }
        Command::Quit(c) => Ok(CompiledCommand::Quit(c)),
        Command::QuitNoprint(c) => Ok(CompiledCommand::QuitNoprint(c)),
        Command::ClearPattern => Ok(CompiledCommand::ClearPattern),
        Command::Noop => Ok(CompiledCommand::Noop),
        Command::Label(l) => Ok(CompiledCommand::Label(l)),
        Command::Block(_) => {
            unreachable!("blocks should be flattened before this point")
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command;

    fn run_sed(script: &str, input: &str) -> String {
        run_sed_opts(script, input, false)
    }

    fn run_sed_opts(script: &str, input: &str, quiet: bool) -> String {
        let parsed = command::parse(script).unwrap();
        let options = Options {
            quiet,
            expressions: vec![],
            script_files: vec![],
            in_place: None,
            extended_regexp: false,
            separate: false,
            null_data: false,
            args: vec![],
        };
        let engine = Engine::new(parsed, &options).unwrap();
        let reader = io::Cursor::new(input.as_bytes());
        let mut output = Vec::new();
        engine
            .process_stream(io::BufReader::new(reader), &mut output)
            .unwrap();
        String::from_utf8(output).unwrap()
    }

    #[test]
    fn substitute_basic() {
        assert_eq!(run_sed("s/foo/bar/", "foo\n"), "bar\n");
    }

    #[test]
    fn substitute_global() {
        assert_eq!(
            run_sed("s/o/0/g", "foo boo\n"),
            "f00 b00\n"
        );
    }

    #[test]
    fn substitute_first_only() {
        assert_eq!(run_sed("s/o/0/", "foo\n"), "f0o\n");
    }

    #[test]
    fn substitute_nth() {
        assert_eq!(run_sed("s/o/0/2", "foo boo\n"), "fo0 boo\n");
    }

    #[test]
    fn substitute_ampersand() {
        assert_eq!(
            run_sed("s/foo/[&]/", "foo\n"),
            "[foo]\n"
        );
    }

    #[test]
    fn substitute_backreference() {
        assert_eq!(
            run_sed("s/(f)(o+)/\\2\\1/", "foo\n"),
            "oof\n"
        );
    }

    #[test]
    fn delete_command() {
        assert_eq!(run_sed("2d", "a\nb\nc\n"), "a\nc\n");
    }

    #[test]
    fn print_command_quiet() {
        assert_eq!(
            run_sed_opts("2p", "a\nb\nc\n", true),
            "b\n"
        );
    }

    #[test]
    fn address_regex() {
        assert_eq!(
            run_sed("/^b/d", "apple\nbanana\ncherry\n"),
            "apple\ncherry\n"
        );
    }

    #[test]
    fn address_range() {
        assert_eq!(
            run_sed("2,3d", "a\nb\nc\nd\n"),
            "a\nd\n"
        );
    }

    #[test]
    fn address_negation() {
        assert_eq!(
            run_sed("2!d", "a\nb\nc\n"),
            "b\n"
        );
    }

    #[test]
    fn transliterate() {
        assert_eq!(
            run_sed("y/abc/xyz/", "abc\n"),
            "xyz\n"
        );
    }

    #[test]
    fn quit_command() {
        assert_eq!(run_sed("2q", "a\nb\nc\n"), "a\nb\n");
    }

    #[test]
    fn hold_and_get() {
        // Store line 1 in hold, append it after line 2
        assert_eq!(
            run_sed("1h;2G", "first\nsecond\n"),
            "first\nsecond\nfirst\n"
        );
    }

    #[test]
    fn exchange() {
        assert_eq!(
            run_sed("1{h;d};2{x}", "first\nsecond\n"),
            "first\n"
        );
    }

    #[test]
    fn labels_and_branch() {
        // Simple loop: replace one 'a' at a time with 'b', branch back if substitution made
        assert_eq!(
            run_sed(":l\ns/a/b/\nt l", "aaa\n"),
            "bbb\n"
        );
    }

    #[test]
    fn append_text() {
        assert_eq!(
            run_sed("1a after first", "first\nsecond\n"),
            "first\nafter first\nsecond\n"
        );
    }

    #[test]
    fn insert_text() {
        assert_eq!(
            run_sed("2i before second", "first\nsecond\n"),
            "first\nbefore second\nsecond\n"
        );
    }

    #[test]
    fn line_number() {
        assert_eq!(
            run_sed("=", "a\nb\n"),
            "1\na\n2\nb\n"
        );
    }

    #[test]
    fn multiple_commands() {
        assert_eq!(
            run_sed("s/a/x/; s/b/y/", "ab\n"),
            "xy\n"
        );
    }

    #[test]
    fn last_line_address() {
        assert_eq!(
            run_sed("$d", "a\nb\nc\n"),
            "a\nb\n"
        );
    }

    #[test]
    fn clear_pattern() {
        assert_eq!(run_sed("z", "hello\n"), "\n");
    }

    #[test]
    fn custom_delimiter() {
        assert_eq!(
            run_sed("s|foo|bar|", "foo\n"),
            "bar\n"
        );
    }

    #[test]
    fn case_insensitive_sub() {
        assert_eq!(
            run_sed("s/foo/bar/i", "FOO\n"),
            "bar\n"
        );
    }

    #[test]
    fn block_with_address() {
        assert_eq!(
            run_sed("/a/ { s/a/x/; s/$/!/ }", "a\nb\n"),
            "x!\nb\n"
        );
    }

    #[test]
    fn regex_range_address() {
        assert_eq!(
            run_sed("/start/,/end/d", "a\nstart\nb\nend\nc\n"),
            "a\nc\n"
        );
    }

    #[test]
    fn empty_input() {
        assert_eq!(run_sed("s/a/b/", ""), "");
    }

    #[test]
    fn passthrough_no_match() {
        assert_eq!(
            run_sed("s/xyz/abc/", "hello\n"),
            "hello\n"
        );
    }

    // ---------------------------------------------------------------
    // Comprehensive coverage tests
    // ---------------------------------------------------------------

    // -- n command --

    #[test]
    fn n_single_line() {
        // n on the only line: print it, exit (no double-print)
        assert_eq!(run_sed("n", "a\n"), "a\n");
    }

    #[test]
    fn n_two_lines() {
        // n on line 1: print "a", read "b" into PS, auto-print "b"
        assert_eq!(run_sed("n", "a\nb\n"), "a\nb\n");
    }

    #[test]
    fn n_with_command_after() {
        // n reads next line, then subsequent commands operate on it
        assert_eq!(
            run_sed("n;s/b/X/", "a\nb\nc\n"),
            "a\nX\nc\n"
        );
    }

    #[test]
    fn n_quiet_mode() {
        // With -n, n does NOT print before reading next
        assert_eq!(
            run_sed_opts("n;p", "a\nb\nc\n", true),
            "b\n"
        );
    }

    // -- N command --

    #[test]
    fn big_n_appends() {
        // N appends next line to pattern space with \n
        assert_eq!(
            run_sed("N;s/\\n/ /", "a\nb\n"),
            "a b\n"
        );
    }

    #[test]
    fn big_n_at_end() {
        // N at last line: print and exit
        assert_eq!(
            run_sed("N", "a\nb\nc\n"),
            "a\nb\nc\n"
        );
    }

    // -- D command --

    #[test]
    fn big_d_deletes_first_line_of_pattern() {
        // N;P;D is the classic "sliding window" idiom
        assert_eq!(
            run_sed("N;P;D", "a\nb\nc\n"),
            "a\nb\nc\n"
        );
    }

    #[test]
    fn big_d_single_line() {
        // D on single-line pattern space acts like d (deletes every line)
        assert_eq!(run_sed("D", "a\nb\n"), "");
    }

    // -- c command --

    #[test]
    fn change_single_address() {
        assert_eq!(
            run_sed("2c REPLACED", "a\nb\nc\n"),
            "a\nREPLACED\nc\n"
        );
    }

    #[test]
    fn change_regex_address() {
        assert_eq!(
            run_sed("/b/c GONE", "a\nb\nc\n"),
            "a\nGONE\nc\n"
        );
    }

    // -- P command --

    #[test]
    fn big_p_first_line() {
        // P prints up to first \n in pattern space
        assert_eq!(
            run_sed("N;P", "first\nsecond\n"),
            "first\nfirst\nsecond\n"
        );
    }

    // -- i (insert) with address --

    #[test]
    fn insert_before_last() {
        assert_eq!(
            run_sed("$i END", "a\nb\n"),
            "a\nEND\nb\n"
        );
    }

    // -- a (append) with regex address --

    #[test]
    fn append_after_match() {
        assert_eq!(
            run_sed("/b/a AFTER", "a\nb\nc\n"),
            "a\nb\nAFTER\nc\n"
        );
    }

    // -- Hold space operations --

    #[test]
    fn hold_append_and_get() {
        // H appends to hold; G appends hold to pattern
        assert_eq!(
            run_sed("1H;2G", "first\nsecond\n"),
            "first\nsecond\n\nfirst\n"
        );
    }

    #[test]
    fn hold_get_replace() {
        // h copies to hold; g copies from hold to pattern
        assert_eq!(
            run_sed("1h;2g", "first\nsecond\n"),
            "first\nfirst\n"
        );
    }

    #[test]
    fn reverse_lines() {
        // Classic sed reverse: 1!G;h;$!d
        assert_eq!(
            run_sed("1!G;h;$!d", "a\nb\nc\n"),
            "c\nb\na\n"
        );
    }

    // -- x (exchange) --

    #[test]
    fn exchange_basic() {
        // Hold starts empty, so x on line 1 gives empty, hold gets "a"
        assert_eq!(
            run_sed("x", "a\nb\n"),
            "\na\n"
        );
    }

    // -- Address ranges --

    #[test]
    fn range_regex_to_regex() {
        assert_eq!(
            run_sed("/start/,/end/d", "a\nstart\nb\nend\nc\n"),
            "a\nc\n"
        );
    }

    #[test]
    fn range_line_to_last() {
        assert_eq!(
            run_sed("2,$d", "a\nb\nc\nd\n"),
            "a\n"
        );
    }

    #[test]
    fn range_negated() {
        // Delete everything NOT in range 2,3
        assert_eq!(
            run_sed("2,3!d", "a\nb\nc\nd\n"),
            "b\nc\n"
        );
    }

    #[test]
    fn range_start_equals_end_on_same_line() {
        // 2,2d only deletes line 2
        assert_eq!(
            run_sed("2,2d", "a\nb\nc\n"),
            "a\nc\n"
        );
    }

    #[test]
    fn range_regex_no_check_on_start_line() {
        // When start and end regexes could match same line,
        // end is NOT checked on start line (POSIX/GNU behavior)
        assert_eq!(
            run_sed_opts(
                "/\\[start\\]/,/\\[/p",
                "[start]\nfoo\nbar\n[end]\n",
                true,
            ),
            "[start]\nfoo\nbar\n[end]\n"
        );
    }

    // -- Step address --

    #[test]
    fn step_even_lines() {
        assert_eq!(
            run_sed("0~2d", "a\nb\nc\nd\ne\n"),
            "a\nc\ne\n"
        );
    }

    #[test]
    fn step_odd_lines() {
        assert_eq!(
            run_sed("1~2d", "a\nb\nc\nd\ne\n"),
            "b\nd\n"
        );
    }

    // -- Substitute flags --

    #[test]
    fn sub_nth_3() {
        assert_eq!(run_sed("s/a/X/3", "aaaaa\n"), "aaXaa\n");
    }

    #[test]
    fn sub_global_and_print() {
        assert_eq!(
            run_sed_opts("s/a/X/gp", "aaa\nbbb\n", true),
            "XXX\n"
        );
    }

    #[test]
    fn sub_escaped_delimiter() {
        // Use | as delimiter, pattern contains |
        assert_eq!(
            run_sed("s/a\\/b/X/", "a/b\n"),
            "X\n"
        );
    }

    #[test]
    fn sub_newline_in_replacement() {
        assert_eq!(
            run_sed("s/a/X\\nY/", "a\n"),
            "X\nY\n"
        );
    }

    #[test]
    fn sub_tab_in_replacement() {
        assert_eq!(
            run_sed("s/a/X\\tY/", "a\n"),
            "X\tY\n"
        );
    }

    #[test]
    fn sub_literal_ampersand() {
        // \& should be literal &
        assert_eq!(
            run_sed("s/foo/\\&/", "foo\n"),
            "&\n"
        );
    }

    #[test]
    fn sub_literal_backslash() {
        // \\\\ in replacement → literal backslash
        assert_eq!(
            run_sed("s/a/\\\\/", "a\n"),
            "\\\n"
        );
    }

    // -- Branching --

    #[test]
    fn branch_unconditional() {
        // b skip jumps forward past the d command, preserving output
        assert_eq!(
            run_sed("b skip;d;:skip", "hello\n"),
            "hello\n"
        );
    }

    #[test]
    fn branch_unconditional_no_label() {
        // b (no label) branches to end of script
        assert_eq!(
            run_sed("b\nd", "hello\n"),
            "hello\n"
        );
    }

    #[test]
    fn branch_if_sub_no_match() {
        // t should NOT branch if no sub was made
        assert_eq!(
            run_sed("s/x/y/;t end;s/a/X/;:end", "abc\n"),
            "Xbc\n"
        );
    }

    #[test]
    fn branch_if_not_sub() {
        // T branches if last sub was NOT successful
        assert_eq!(
            run_sed("s/x/y/;T skip;s/a/SHOULD_NOT/;:skip", "abc\n"),
            "abc\n"
        );
    }

    // -- l command (list) --

    #[test]
    fn list_command() {
        assert_eq!(
            run_sed("l", "a\tb\n"),
            "a\\tb$\na\tb\n"
        );
    }

    // -- = command (line number) with address --

    #[test]
    fn line_number_with_address() {
        assert_eq!(
            run_sed("2=", "a\nb\nc\n"),
            "a\n2\nb\nc\n"
        );
    }

    // -- z (clear pattern) --

    #[test]
    fn clear_pattern_in_block() {
        assert_eq!(
            run_sed("/hello/{z;s/$/EMPTY/}", "hello\nworld\n"),
            "EMPTY\nworld\n"
        );
    }

    // -- Multiple commands, complex scripts --

    #[test]
    fn strip_html_tags() {
        assert_eq!(
            run_sed("s/<[^>]*>//g", "<b>bold</b>\n"),
            "bold\n"
        );
    }

    #[test]
    fn sed_multiline_join() {
        // Join all lines with space using N and s
        assert_eq!(
            run_sed(":a;N;s/\\n/ /;$!b a", "one\ntwo\nthree\n"),
            "one two three\n"
        );
    }

    #[test]
    fn double_space() {
        // Classic double-spacing: G appends empty hold to pattern
        assert_eq!(
            run_sed("G", "a\nb\n"),
            "a\n\nb\n\n"
        );
    }

    #[test]
    fn delete_empty_lines() {
        assert_eq!(
            run_sed("/^$/d", "a\n\nb\n\nc\n"),
            "a\nb\nc\n"
        );
    }

    #[test]
    fn multiple_expressions() {
        // Simulates -e 'cmd1' -e 'cmd2' by joining with newline
        assert_eq!(
            run_sed("s/a/x/\ns/b/y/", "ab\n"),
            "xy\n"
        );
    }

    #[test]
    fn nested_blocks() {
        assert_eq!(
            run_sed("1{/a/{s/a/X/}}", "abc\ndef\n"),
            "Xbc\ndef\n"
        );
    }

    // -- Edge cases --

    #[test]
    fn line_with_no_trailing_newline() {
        // Input without trailing newline
        assert_eq!(run_sed("s/a/X/", "abc"), "Xbc\n");
    }

    #[test]
    fn single_empty_line() {
        assert_eq!(run_sed("s/^$/EMPTY/", "\n"), "EMPTY\n");
    }

    #[test]
    fn substitute_with_empty_replacement() {
        assert_eq!(run_sed("s/foo//", "foobar\n"), "bar\n");
    }

    #[test]
    fn substitute_with_empty_pattern_match() {
        // Regex that matches empty string at beginning
        assert_eq!(run_sed("s/^/PREFIX: /", "hello\n"), "PREFIX: hello\n");
    }

    #[test]
    fn multiple_ranges_interleaved() {
        // Two separate range commands
        let input = "a\nb\nc\nd\ne\nf\n";
        assert_eq!(
            run_sed("2,3s/./X/;5,6s/./Y/", input),
            "a\nX\nX\nd\nY\nY\n"
        );
    }

    #[test]
    fn regex_special_chars() {
        assert_eq!(
            run_sed("s/\\./X/g", "a.b.c\n"),
            "aXbXc\n"
        );
    }

    #[test]
    fn regex_anchors() {
        assert_eq!(
            run_sed("s/^/> /", "hello\n"),
            "> hello\n"
        );
        assert_eq!(
            run_sed("s/$/ </", "hello\n"),
            "hello <\n"
        );
    }

    // -- q and Q --

    #[test]
    fn quit_prints_current_line() {
        // q should print the current line before quitting
        assert_eq!(run_sed("2q", "a\nb\nc\n"), "a\nb\n");
    }

    #[test]
    fn quit_no_print() {
        // Q quits without printing pattern space
        assert_eq!(run_sed("2Q", "a\nb\nc\n"), "a\n");
    }

    // -- Complex real-world scripts --

    #[test]
    fn remove_trailing_whitespace() {
        assert_eq!(
            run_sed("s/[ \t]*$//", "hello   \nworld\t\t\n"),
            "hello\nworld\n"
        );
    }

    #[test]
    fn number_lines() {
        // Print line number then line (like nl)
        assert_eq!(
            run_sed("=;s/^/  /", "a\nb\n"),
            "1\n  a\n2\n  b\n"
        );
    }

    #[test]
    fn print_only_matches() {
        // -n with /pattern/p
        assert_eq!(
            run_sed_opts("/foo/p", "foo\nbar\nfoo2\n", true),
            "foo\nfoo2\n"
        );
    }

    #[test]
    fn delete_first_and_last() {
        assert_eq!(
            run_sed("1d;$d", "a\nb\nc\n"),
            "b\n"
        );
    }

    #[test]
    fn change_every_matching_line() {
        assert_eq!(
            run_sed("/old/c new", "old\nkeep\nold\n"),
            "new\nkeep\nnew\n"
        );
    }
}
