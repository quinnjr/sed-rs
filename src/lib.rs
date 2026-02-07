//! # sed-rs
//!
//! A GNU-compatible `sed` (stream editor) implementation in Rust.
//!
//! This crate can be used both as a standalone command-line tool and as a
//! library for programmatic stream editing.
//!
//! ## Quick start
//!
//! ```rust
//! // Simple substitution
//! let output = sed_rs::eval("s/hello/world/", "hello there\n").unwrap();
//! assert_eq!(output, "world there\n");
//!
//! // Global substitution
//! let output = sed_rs::eval("s/o/0/g", "foo boo\n").unwrap();
//! assert_eq!(output, "f00 b00\n");
//!
//! // Delete lines matching a pattern
//! let output = sed_rs::eval("/^#/d", "# comment\ncode\n").unwrap();
//! assert_eq!(output, "code\n");
//!
//! // Multiple commands
//! let output = sed_rs::eval("s/a/X/; s/b/Y/", "ab\n").unwrap();
//! assert_eq!(output, "XY\n");
//! ```
//!
//! ## Advanced usage
//!
//! For more control, use [`Sed`] directly:
//!
//! ```rust
//! use sed_rs::Sed;
//!
//! let mut sed = Sed::new("s/foo/bar/g").unwrap();
//! sed.quiet(true);           // suppress auto-print (-n)
//!
//! let output = sed.eval("no match here\n").unwrap();
//! assert_eq!(output, "");    // quiet mode: nothing printed unless explicit `p`
//! ```
//!
//! ## Using the lower-level API
//!
//! The [`command`] and [`engine`] modules expose the parser and execution
//! engine for full control:
//!
//! ```rust
//! use sed_rs::{command, engine, Options};
//!
//! let commands = command::parse("2d").unwrap();
//! let options = Options::default();
//! let engine = engine::Engine::new(commands, &options).unwrap();
//! // engine.run(&[]) reads from stdin, engine.run(&[path]) reads files
//! ```

pub mod cli;
pub mod command;
pub mod engine;
pub mod error;
pub mod unescape;

pub use cli::Options;
pub use error::{Error, Result};

use std::io;

// ---------------------------------------------------------------------------
// Convenience API
// ---------------------------------------------------------------------------

/// A configured sed instance that can process text.
///
/// This is the recommended entry point for library usage. It wraps the
/// lower-level [`command::parse`] and [`engine::Engine`] with a builder-style
/// API.
///
/// # Examples
///
/// ```rust
/// use sed_rs::Sed;
///
/// let output = Sed::new("s/hello/world/")
///     .unwrap()
///     .eval("hello\n")
///     .unwrap();
/// assert_eq!(output, "world\n");
/// ```
pub struct Sed {
    options: Options,
    script: String,
}

impl Sed {
    /// Create a new `Sed` instance from a sed script string.
    ///
    /// The script is validated (parsed and regex-compiled) eagerly; an
    /// error is returned immediately if the script is malformed.
    pub fn new(script: &str) -> Result<Self> {
        // Validate the script eagerly
        let cmds = command::parse(script)?;
        let opts = Options::default();
        let _ = engine::Engine::new(cmds, &opts)?;

        Ok(Self {
            options: opts,
            script: script.to_string(),
        })
    }

    /// Suppress automatic printing of the pattern space (equivalent to
    /// the `-n` / `--quiet` flag).
    pub fn quiet(&mut self, yes: bool) -> &mut Self {
        self.options.quiet = yes;
        self
    }

    /// Use NUL (`\0`) as the line delimiter instead of newline
    /// (equivalent to `-z` / `--null-data`).
    pub fn null_data(&mut self, yes: bool) -> &mut Self {
        self.options.null_data = yes;
        self
    }

    /// Evaluate the script against the given input string and return
    /// the output as a `String`.
    pub fn eval(&self, input: &str) -> Result<String> {
        self.eval_bytes(input.as_bytes())
    }

    /// Evaluate the script against raw bytes and return the output as
    /// a `String`.
    pub fn eval_bytes(&self, input: &[u8]) -> Result<String> {
        let commands = command::parse(&self.script)?;
        let engine = engine::Engine::new(commands, &self.options)?;
        let reader = io::BufReader::new(io::Cursor::new(input));
        let mut output = Vec::new();
        engine.process_stream(reader, &mut output)?;
        Ok(String::from_utf8_lossy(&output).into_owned())
    }

    /// Evaluate the script by reading from a [`std::io::Read`] source
    /// and writing to a [`std::io::Write`] sink.
    pub fn eval_stream<R: io::Read, W: io::Write>(
        &self,
        reader: R,
        writer: &mut W,
    ) -> Result<()> {
        let commands = command::parse(&self.script)?;
        let engine = engine::Engine::new(commands, &self.options)?;
        let buf_reader = io::BufReader::new(reader);
        engine.process_stream(buf_reader, writer)
    }
}

/// Evaluate a sed script against an input string and return the result.
///
/// This is the simplest way to use the library. For repeated use with
/// the same script, prefer [`Sed::new`] to avoid re-parsing.
///
/// # Examples
///
/// ```rust
/// let output = sed_rs::eval("s/world/rust/", "hello world\n").unwrap();
/// assert_eq!(output, "hello rust\n");
/// ```
pub fn eval(script: &str, input: &str) -> Result<String> {
    Sed::new(script)?.eval(input)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_basic() {
        assert_eq!(eval("s/foo/bar/", "foo\n").unwrap(), "bar\n");
    }

    #[test]
    fn eval_global() {
        assert_eq!(eval("s/o/0/g", "foo\n").unwrap(), "f00\n");
    }

    #[test]
    fn eval_delete() {
        assert_eq!(eval("2d", "a\nb\nc\n").unwrap(), "a\nc\n");
    }

    #[test]
    fn eval_multiple_commands() {
        assert_eq!(eval("s/a/X/; s/b/Y/", "ab\n").unwrap(), "XY\n");
    }

    #[test]
    fn eval_empty_input() {
        assert_eq!(eval("s/a/b/", "").unwrap(), "");
    }

    #[test]
    fn eval_bad_script() {
        assert!(eval("s/[invalid/x/", "test").is_err());
    }

    #[test]
    fn sed_builder_quiet() {
        let output = Sed::new("2p")
            .unwrap()
            .quiet(true)
            .eval("a\nb\nc\n")
            .unwrap();
        assert_eq!(output, "b\n");
    }

    #[test]
    fn sed_builder_stream() {
        let sed = Sed::new("s/hello/world/").unwrap();
        let input = b"hello\n";
        let mut output = Vec::new();
        sed.eval_stream(&input[..], &mut output).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "world\n");
    }

    #[test]
    fn sed_reuse() {
        let sed = Sed::new("s/x/y/g").unwrap();
        assert_eq!(sed.eval("xxx\n").unwrap(), "yyy\n");
        assert_eq!(sed.eval("axa\n").unwrap(), "aya\n");
    }
}
