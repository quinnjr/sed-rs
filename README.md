# sed-rs

A GNU-compatible [`sed`](https://www.gnu.org/software/sed/) implementation in Rust, built on the foundations of [`sd`](https://github.com/chmln/sd).

`sd` is an excellent modern find-and-replace tool, but it intentionally breaks from traditional `sed` syntax. `sed-rs` takes the opposite approach: it adapts `sd`'s fast Rust regex engine and atomic file-write strategy into a tool that speaks fluent GNU `sed`, so it can serve as a drop-in replacement in scripts, pipelines, and anywhere else GNU `sed` is expected.

## Why

- You want `sed` but compiled from Rust with no C dependencies.
- You want consistent, modern regex syntax (Rust/PCRE-style ERE) without the BRE/ERE confusion of traditional `sed`.
- You want a `sed` you can also call as a Rust library.

## Install

```bash
cargo install sed-rs
```

Or build from source:

```bash
git clone https://github.com/pegasusheavy/sed-rs.git
cd sed-rs
cargo build --release
# binary is at target/release/sed
```

## Usage

`sed-rs` aims to accept the same flags and scripting language as GNU `sed`:

```bash
# simple substitution
echo 'hello world' | sed 's/world/rust/'

# in-place editing with backup
sed -i.bak 's/foo/bar/g' file.txt

# multiple expressions
sed -e '2d' -e 's/old/new/' input.txt

# script file
sed -f commands.sed input.txt

# quiet mode — only print explicit `p` output
sed -n '/pattern/p' file.txt
```

### Supported flags

| Flag | Description |
|------|-------------|
| `-n`, `--quiet`, `--silent` | Suppress automatic printing of pattern space |
| `-e SCRIPT`, `--expression=SCRIPT` | Add script commands |
| `-f FILE`, `--file=FILE` | Read script from file |
| `-i[SUFFIX]`, `--in-place[=SUFFIX]` | Edit files in place (backup if SUFFIX given) |
| `-E`, `-r`, `--regexp-extended` | Accepted for compatibility (ERE is always on) |
| `-s`, `--separate` | Treat files as separate streams |
| `-z`, `--null-data` | NUL-delimited input/output |

### Supported commands

`s`, `d`, `p`, `P`, `q`, `Q`, `a`, `i`, `c`, `y`, `=`, `l`, `z`, `n`, `N`, `D`, `h`, `H`, `g`, `G`, `x`, `b`, `t`, `T`, `:`, `r`, `w`, `{...}`

Addressing: line numbers, `$` (last line), `/regex/`, `first~step`, ranges (`addr1,addr2`), and negation (`!`).

## Library

`sed-rs` also exports a Rust library so you can use `sed` scripting from your own code:

```rust
// One-shot convenience function
let output = sed_rs::eval("s/hello/world/", "hello there\n").unwrap();
assert_eq!(output, "world there\n");
```

```rust
// Builder API with options
use sed_rs::Sed;

let mut sed = Sed::new("s/foo/bar/g").unwrap();
sed.quiet(true);

let output = sed.eval("foo foo\n").unwrap();
assert_eq!(output, "");  // quiet: nothing unless explicit `p`
```

```rust
// Streaming I/O
use sed_rs::Sed;
use std::io;

let sed = Sed::new("s/old/new/g").unwrap();
let input = b"old old old\n";
let mut output = Vec::new();
sed.eval_stream(&input[..], &mut output).unwrap();
assert_eq!(String::from_utf8(output).unwrap(), "new new new\n");
```

## Regex dialect

`sed-rs` always uses the Rust [`regex`](https://docs.rs/regex) crate, which provides syntax similar to PCRE / ERE. The `-E` and `-r` flags are accepted for compatibility but are no-ops since extended syntax is the default. Traditional BRE-only constructs (e.g. `\(` for grouping) are **not** supported — use `(` directly.

## Acknowledgements

This project is built on ideas and code adapted from [`sd`](https://github.com/chmln/sd) by [Gregory](https://github.com/chmln), licensed under the MIT License. Specifically, `sed-rs` draws from `sd`'s approach to regex replacement mapping, escape-sequence handling, and atomic file writes. See [LICENSE-MIT](LICENSE-MIT) for full attribution.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.

Copyright (c) 2026 Pegasus Heavy Industries LLC
