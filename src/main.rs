use clap::Parser;
use std::process;

use sed_rs::cli;
use sed_rs::command;
use sed_rs::engine;
use sed_rs::{Error, Result};

fn main() {
    if let Err(e) = try_main() {
        eprintln!("sed: {e}");
        process::exit(2);
    }
}

fn try_main() -> Result<()> {
    let args = cli::preprocess_args(std::env::args());
    let options = cli::Options::parse_from(args);
    let (script, files) = options.script_and_files()?;

    if script.is_empty() {
        return Err(Error::Parse("empty script".into()));
    }

    let commands = command::parse(&script)?;
    let engine = engine::Engine::new(commands, &options)?;

    if let Some(ref suffix) = options.in_place {
        if files.is_empty() {
            return Err(Error::Parse(
                "-i/--in-place requires at least one file argument".into(),
            ));
        }
        engine.run_in_place(&files, suffix)?;
    } else {
        engine.run(&files)?;
    }

    Ok(())
}
