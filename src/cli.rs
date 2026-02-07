use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug, Default)]
#[command(
    name = "sed",
    version,
    about = "A GNU-compatible stream editor implemented in Rust",
    long_about = "A GNU-compatible stream editor implemented in Rust.\n\n\
        Uses Rust regex syntax (similar to PCRE/ERE). The -E/-r flags are \
        accepted for compatibility but are no-ops since extended regex \
        syntax is always used.",
    max_term_width = 100
)]
pub struct Options {
    /// Suppress automatic printing of pattern space
    #[arg(short = 'n', long = "quiet", alias = "silent")]
    pub quiet: bool,

    /// Add the script to the commands to be executed
    #[arg(short = 'e', long = "expression", value_name = "SCRIPT")]
    pub expressions: Vec<String>,

    /// Add the contents of script-file to the commands to be executed
    #[arg(short = 'f', long = "file", value_name = "SCRIPT-FILE")]
    pub script_files: Vec<PathBuf>,

    /// Edit files in place (makes backup if SUFFIX supplied).
    /// Use --in-place=SUFFIX or -iSUFFIX (no space) for backups.
    #[arg(
        long = "in-place",
        value_name = "SUFFIX",
        num_args = 0..=1,
        default_missing_value = "",
        require_equals = true
    )]
    pub in_place: Option<String>,

    /// Use extended regular expressions (accepted for compatibility; always enabled)
    #[arg(short = 'E', short_alias = 'r', long = "regexp-extended")]
    pub extended_regexp: bool,

    /// Consider files as separate rather than as a single continuous stream
    #[arg(short = 's', long = "separate")]
    pub separate: bool,

    /// Separate lines by NUL characters
    #[arg(short = 'z', long = "null-data")]
    pub null_data: bool,

    /// [SCRIPT] [INPUT-FILE...]
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

impl Options {
    /// Extract the sed script and input file paths from the parsed options.
    ///
    /// If -e or -f was used, all positional args are files.
    /// Otherwise, the first positional arg is the script and the rest are files.
    pub fn script_and_files(&self) -> crate::Result<(String, Vec<PathBuf>)> {
        let mut scripts = self.expressions.clone();

        for path in &self.script_files {
            let content = std::fs::read_to_string(path).map_err(|e| {
                crate::Error::Parse(format!(
                    "couldn't read script file '{}': {}",
                    path.display(),
                    e
                ))
            })?;
            scripts.push(content);
        }

        if scripts.is_empty() {
            // First positional arg is the script, rest are files
            if let Some((first, rest)) = self.args.split_first() {
                Ok((
                    first.clone(),
                    rest.iter().map(PathBuf::from).collect(),
                ))
            } else {
                Err(crate::Error::Parse(
                    "no script provided. Usage: sed [OPTIONS] SCRIPT [FILE...]"
                        .into(),
                ))
            }
        } else {
            Ok((
                scripts.join("\n"),
                self.args.iter().map(PathBuf::from).collect(),
            ))
        }
    }
}

/// Pre-process raw CLI arguments to handle GNU sed's `-i[SUFFIX]` syntax.
///
/// Transforms:
///   -i        → --in-place
///   -i.bak    → --in-place=.bak
///   -iSUFFIX  → --in-place=SUFFIX
///
/// This is needed because clap can't natively handle optional short-flag
/// values that must be attached (no space) like GNU sed's -i.
pub fn preprocess_args(raw: impl Iterator<Item = String>) -> Vec<String> {
    let mut result = Vec::new();
    let mut saw_double_dash = false;

    for arg in raw {
        if saw_double_dash {
            result.push(arg);
            continue;
        }

        if arg == "--" {
            saw_double_dash = true;
            result.push(arg);
            continue;
        }

        if arg == "-i" {
            result.push("--in-place".to_string());
        } else if arg.starts_with("-i") && !arg.starts_with("--") {
            let suffix = &arg[2..];
            result.push(format!("--in-place={suffix}"));
        } else {
            result.push(arg);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn debug_assert() {
        Options::command().debug_assert();
    }

    #[test]
    fn preprocess_i_flag() {
        let args = vec![
            "sed".into(),
            "-i".into(),
            "s/foo/bar/".into(),
            "file".into(),
        ];
        let processed = preprocess_args(args.into_iter());
        assert_eq!(
            processed,
            vec!["sed", "--in-place", "s/foo/bar/", "file"]
        );
    }

    #[test]
    fn preprocess_i_suffix() {
        let args =
            vec!["sed".into(), "-i.bak".into(), "s/foo/bar/".into()];
        let processed = preprocess_args(args.into_iter());
        assert_eq!(
            processed,
            vec!["sed", "--in-place=.bak", "s/foo/bar/"]
        );
    }
}
