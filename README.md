<div align="center">

# sed-rs

**A GNU-compatible `sed` written in Rust — drop-in CLI and embeddable library.**

[![crates.io](https://img.shields.io/crates/v/sed-rs.svg)](https://crates.io/crates/sed-rs)
[![docs.rs](https://img.shields.io/docsrs/sed-rs)](https://docs.rs/sed-rs)
[![license](https://img.shields.io/crates/l/sed-rs.svg)](#license)

</div>

`sed-rs` speaks fluent GNU [`sed`](https://www.gnu.org/software/sed/) — the same flags, the same scripting language — but ships as a single dependency-free Rust binary and a library you can call from your own code. It's built on the regex engine and atomic-write strategy of [`sd`](https://github.com/chmln/sd), wrapped in a traditional `sed` front end so it slots into existing scripts and pipelines unchanged.

```bash
echo 'hello world' | sed 's/world/rust/'      # → hello rust
sed -i.bak 's/foo/bar/g' config.txt           # in-place edit, keeps config.txt.bak
sed -n '/^ERROR/p' app.log                     # print only matching lines
```

## Why another sed?

`sd` is a great modern find-and-replace tool, but it deliberately abandons `sed` syntax. `sed-rs` goes the other way — it keeps the `sed` language you already know and the scripts you already have, while giving you:

- **No C, no system dependencies** — one self-contained Rust binary, `cargo install` and done.
- **One consistent regex dialect.** Rust [`regex`](https://docs.rs/regex) (ERE/PCRE-style) everywhere — no BRE-vs-ERE mode confusion, no `\(`-to-group surprises.
- **A real library API.** The same engine that powers the CLI is exported as `sed_rs`, so you can run `sed` scripts from Rust without shelling out.

## Install

```bash
cargo install sed-rs        # installs the `sed` binary
```

From source:

```bash
git clone https://github.com/pegasusheavy/sed-rs.git
cd sed-rs
cargo build --release       # → target/release/sed
```

## CLI usage

```bash
# substitution (first match per line, or /g for all)
echo 'a-a-a' | sed 's/a/X/g'                   # → X-X-X

# case-insensitive, print only the changed line
sed -n 's/error/ERROR/Ip' app.log

# delete, ranges, and negation
sed '2d' file.txt                              # drop line 2
sed '2,4d' file.txt                            # drop lines 2–4
sed '/^#/d' file.txt                           # drop comment lines
sed -n '/BEGIN/,/END/p' file.txt               # print a block between markers

# multiple expressions and script files
sed -e '1d' -e 's/old/new/g' input.txt
sed -f script.sed input.txt

# in-place edit (add a suffix to keep a backup)
sed -i 's/v1/v2/g' *.yaml
sed -i.orig 's/v1/v2/g' deploy.yaml            # writes deploy.yaml.orig

# NUL-delimited records, e.g. piping from `find -print0`
find . -name '*.txt' -print0 | sed -z 's/\n/ /g'
```

### Flags

| Flag | Description |
|------|-------------|
| `-n`, `--quiet`, `--silent` | Suppress automatic printing; only `p`/`P`/`=`/`l` produce output |
| `-e SCRIPT`, `--expression=SCRIPT` | Add a script expression (repeatable) |
| `-f FILE`, `--file=FILE` | Read script from a file (repeatable) |
| `-i[SUFFIX]`, `--in-place[=SUFFIX]` | Edit files in place; with `SUFFIX`, keep a backup |
| `-E`, `-r`, `--regexp-extended` | Accepted for compatibility — ERE is always on (no-op) |
| `-s`, `--separate` | Treat input files as separate streams rather than one |
| `-z`, `--null-data` | Use NUL (`\0`) as the line separator |

> **`-i` syntax:** like GNU `sed`, the optional backup suffix attaches directly to the flag — `-i.bak` or `--in-place=.bak`. A bare `-i` edits with no backup.

### Commands

| Group | Commands |
|-------|----------|
| Substitution / transliteration | `s`, `y` |
| Printing | `p`, `P`, `=`, `l` |
| Deletion | `d`, `D` |
| Input | `n`, `N` |
| Hold space | `h`, `H`, `g`, `G`, `x` |
| Text | `a` (append), `i` (insert), `c` (change) |
| Branching | `:label`, `b`, `t`, `T` |
| Files | `r` (read), `w` (write) |
| Control / grouping | `q`, `Q`, `z` (zap), `{ … }` |

The `s///` command supports the `g`, `p`, `i`/`I` (case-insensitive), `w FILE`, and numeric *N*th-occurrence flags — and they combine, e.g. `s/x/y/2g` replaces from the 2nd match onward.

### Addressing

| Form | Selects |
|------|---------|
| `N` | line number `N` |
| `$` | the last line |
| `/regex/` | lines matching `regex` |
| `first~step` | every `step`-th line starting at `first` (GNU extension) |
| `addr1,addr2` | the inclusive range from `addr1` to `addr2` |
| `addr!` | the negation — every line the address does *not* select |

## Library usage

Add it as a dependency:

```toml
[dependencies]
sed-rs = "1"
```

The crate is `sed-rs`; it's imported as `sed_rs`.

```rust
// One-shot: parse, run, return the result.
let out = sed_rs::eval("s/hello/world/", "hello there\n")?;
assert_eq!(out, "world there\n");
```

```rust
use sed_rs::Sed;

// Builder API — configure options, reuse across inputs.
let mut sed = Sed::new("s/foo/bar/g")?;
sed.quiet(true);                          // equivalent to -n

assert_eq!(sed.eval("foo foo\n")?, "");   // quiet: nothing unless an explicit `p`
```

```rust
use sed_rs::Sed;

// Streaming: read from any Read, write to any Write — never buffers the whole input.
let sed = Sed::new("s/old/new/g")?;
let mut out = Vec::new();
sed.eval_stream(&b"old old old\n"[..], &mut out)?;
assert_eq!(out, b"new new new\n");
```

`Sed::new` parses and compiles the script eagerly, so a malformed script fails fast. For the lower-level parser and engine, see the [`command`](https://docs.rs/sed-rs/latest/sed_rs/command/) and [`engine`](https://docs.rs/sed-rs/latest/sed_rs/engine/) modules.

## Regex dialect & differences from GNU sed

`sed-rs` always uses the Rust [`regex`](https://docs.rs/regex) crate (ERE/PCRE-flavored syntax). This is the main place behavior diverges from GNU `sed`:

- **ERE is always on.** `-E`/`-r` are accepted but do nothing. Group with `( )` and alternate with `|` directly.
- **No BRE constructs.** Backslash-grouping like `\(` `\)` and `\{` `\}` is *not* supported — use the bare metacharacters instead.
- **No backreferences in patterns.** The Rust `regex` engine is finite-automaton based, so `\1`-style backreferences *within a match* aren't available. Backreferences in the *replacement* text — `\1`, `&` — work as usual.

If a script relies on BRE-only behavior, port its patterns to ERE first. For the great majority of substitution, deletion, and addressing scripts, nothing changes.

## Building & testing

```bash
cargo build --release        # optimized binary (LTO, stripped) at target/release/sed
cargo test                   # unit + doc tests
cargo clippy --all-targets   # lints
```

## Acknowledgements

Built on ideas and code adapted from [`sd`](https://github.com/chmln/sd) by [Gregory](https://github.com/chmln) (MIT). `sed-rs` draws on `sd`'s regex-replacement mapping, escape-sequence handling, and atomic file writes. See [LICENSE-MIT](LICENSE-MIT) for full attribution.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.

Copyright © 2026 Pegasus Heavy Industries LLC
