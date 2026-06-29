# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

`sed-rs` is a GNU-compatible `sed` (stream editor) shipped as both a CLI binary (`sed`) and a library crate (`sed_rs`). It uses the Rust `regex` engine for all matching — there is no BRE mode; ERE-style syntax is always on and `-E`/`-r` are accepted but no-ops. Regex-replacement mapping, escape handling, and atomic file writes are adapted from [`sd`](https://github.com/chmln/sd).

## Commands

```bash
cargo build                          # debug build
cargo build --release                # release binary at target/release/sed (LTO + stripped)
cargo test                           # all unit tests
cargo test eval_global               # single test by name
cargo test --doc                     # run the doctests in lib.rs / README examples
cargo clippy --all-targets           # lint
cargo fmt                            # format
```

Tests live inline in `#[cfg(test)] mod tests` at the bottom of each source file (no `tests/` integration dir). `engine.rs` holds the bulk (~86 tests) and uses `run_sed(script, input)` / `run_sed_opts(...)` helpers — add new behavioral tests there alongside them.

## Architecture

The pipeline is three stages, mirrored by three modules:

1. **`command.rs` — parse.** `command::parse(script) -> Vec<SedCommand>` hand-rolls a char-by-char `Parser` (no parser library). Produces an AST: each `SedCommand` is an `AddressRange` + a `Command` enum variant. Blocks (`{...}`) nest as `Command::Block(Vec<SedCommand>)`. This is pure syntax — no regexes are compiled here, patterns stay as `String`.

2. **`engine.rs` — compile + execute.** `Engine::new(commands, &Options)` runs `flatten_and_compile`, which (a) compiles every regex/address into a `Compiled*` form once, and (b) **flattens nested `Block`s into a flat `Vec<CompiledCommand>` using `ScopeStart`/`ScopeEnd` markers** rather than keeping the tree. This flat vector with an instruction pointer is what makes branching (`b`/`t`/`T` + labels in `HashMap<String,usize>`) and the `D` "restart script" semantics implementable as index jumps.

   Execution is the classic sed cycle in `process_stream`: read line → pattern space → walk commands → auto-print (unless `quiet`) → flush `append_queue`. Per-line mutable state (`pattern_space`, `hold_space`, `line_number`, `last_sub_success`, and `range_active` for tracking open `addr1,addr2` ranges) lives in `State`, separate from the immutable compiled `Engine`. Control flow between commands is the `Flow` enum (`Continue` / `Restart` (`d`) / `RestartScript` (`D`) / `Branch` / `Quit` / `QuitNoPrint`).

   Entry points: `run(&files)` (stream to stdout), `run_in_place(&files, suffix)` (per-file, writes via `write_atomic` using a `tempfile::NamedTempFile` in the same dir, with optional backup), and `process_stream<R,W>` (the library core).

3. **`cli.rs` — argument handling.** clap `derive` `Options`, but `-i`/`-iSUFFIX` can't be expressed in clap (optional attached value on a short flag), so **`preprocess_args` rewrites `argv` before clap sees it** (`-i` → `--in-place`, `-iSUFFIX` → `--in-place=SUFFIX`), respecting `--`. `script_and_files()` resolves the script: from `-e`/`-f` if present (joined with `\n`), otherwise the first positional arg is the script and the rest are files.

`lib.rs` wraps stages 1–2 in the public API: `eval(script, input)` one-shot, and the `Sed` builder (`.quiet()`, `.null_data()`, `.eval()`/`.eval_bytes()`/`.eval_stream()`). `Sed::new` parses + compiles eagerly to surface script errors immediately, but re-parses on each `eval*` call. `unescape.rs` handles `\n`/`\t`/`\\` etc. in replacement text and `y///` arguments; `error.rs` is a `thiserror` enum. The binary exits `2` on any error (GNU sed convention).

## Conventions

- Edition 2024, MSRV 1.85 (`Cargo.toml`). Keep both in sync if bumping.
- When adding a new sed command: add the `Command` variant + parsing in `command.rs`, a matching `CompiledCommand` variant + compile arm in `flatten_and_compile`, the execution arm in `process_stream`, a row in the README command table, and a test in `engine.rs`.
- Dual-licensed MIT OR Apache-2.0; `sd`-derived code must retain attribution (see README acknowledgements + LICENSE-MIT).
