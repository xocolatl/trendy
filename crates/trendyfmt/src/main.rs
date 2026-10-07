use std::path::PathBuf;
use std::process::ExitCode;

use trendy_diag::Format;

const USAGE: &str = "usage: trendyfmt [--check] [--error-format=human|json] <file>...";

/// Format files in place, or with `--check`, list the files that are not
/// formatted and exit non-zero.
fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(message) => {
            eprint!("{message}");
            if !message.ends_with('\n') {
                eprintln!();
            }
            ExitCode::FAILURE
        }
    }
}

/// Returns whether every file was already formatted (always true without `--check`).
fn run() -> Result<bool, String> {
    let mut check = false;
    let mut format = Format::Human;
    let mut files = Vec::new();
    for arg in std::env::args().skip(1) {
        if let Some(value) = arg.strip_prefix("--error-format=") {
            format = Format::from_flag(value)
                .ok_or_else(|| format!("unknown error format `{value}`\n{USAGE}"))?;
            continue;
        }
        match arg.as_str() {
            "--check" => check = true,
            _ if arg.starts_with('-') => return Err(USAGE.to_string()),
            _ => files.push(PathBuf::from(arg)),
        }
    }
    if files.is_empty() {
        return Err(USAGE.to_string());
    }

    let mut all_formatted = true;
    for file in files {
        let path = file.display().to_string();
        let source = std::fs::read_to_string(&file).map_err(|e| format!("{path}: {e}"))?;
        let parse = trendy_parser::parse(&source, trendy_lexer::lex(&source));
        if !parse.errors.is_empty() {
            return Err(trendy_diag::render_all(
                &parse.errors,
                format,
                &path,
                &source,
            ));
        }
        let formatted = trendy_fmt::format(&parse.tree);

        if formatted == source {
            continue;
        }
        if check {
            println!("{path}");
            all_formatted = false;
        } else {
            std::fs::write(&file, formatted).map_err(|e| format!("{path}: {e}"))?;
        }
    }
    Ok(all_formatted)
}
