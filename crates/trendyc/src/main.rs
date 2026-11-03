use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use trendy_diag::{Code, Diagnostic, Format, Sources};

const USAGE: &str = "\
usage: trendyc <input> [-o <output>] [--error-format=human|json]
       trendyc --explain <code>";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprint!("{message}");
            if !message.ends_with('\n') {
                eprintln!();
            }
            ExitCode::FAILURE
        }
    }
}

enum Mode {
    Build {
        input: PathBuf,
        output: PathBuf,
        format: Format,
    },
    Explain(Code),
}

fn run() -> Result<(), String> {
    match parse_args(std::env::args().skip(1))? {
        Mode::Explain(code) => {
            print!("{}", code.explanation());
            Ok(())
        }
        Mode::Build {
            input,
            output,
            format,
        } => build(&input, &output, format),
    }
}

fn build(input: &Path, output: &Path, format: Format) -> Result<(), String> {
    let path = input.display().to_string();
    let source = std::fs::read_to_string(input).map_err(|e| format!("{path}: {e}"))?;

    // Main's unit and those it uses, each from its file, whose spans are
    // in their texts one after another.
    let program = trendy_load::program(input, source);
    if let Some((unread, e)) = program.unreadable.first() {
        return Err(format!("{unread}: {e}"));
    }
    let errors = program.errors();
    if !errors.is_empty() {
        let sources = Sources::new(program.sources());
        return Err(report(&errors, format, &path, &sources));
    }
    // The units that come with the compiler, which are part of it, so one
    // that doesn't parse is a compiler bug; their texts follow the
    // program's units' in sema's spans.
    let library = trendy_load::library();
    if let Some(unit) = library.iter().find(|unit| !unit.parse.errors.is_empty()) {
        let sources = Sources::one(&unit.path, &unit.source);
        let rendered = trendy_diag::render_all(&unit.parse.errors, Format::Human, &sources);
        return Err(format!(
            "error[X0000]: internal compiler error: `{}` doesn't parse\n{rendered}",
            unit.path
        ));
    }
    let hir = trendy_sema::analyze(&files(&program.units), &files(&library)).map_err(|errors| {
        let sources = Sources::new(
            program.sources().chain(
                library
                    .iter()
                    .map(|unit| (unit.path.as_str(), unit.source.as_str())),
            ),
        );
        report(&errors, format, &path, &sources)
    })?;

    // Past sema, every failure is a compiler bug, not a user error.
    let hir = trendy_opt::optimized(hir);
    let lir = trendy_lower::lower(&hir);
    let object = trendy_codegen::compile(&lir)
        .map_err(|e| format!("error[X0000]: internal compiler error: {e}"))?;
    link(&object, output)
}

/// The files of `units`, as sema takes them.
fn files(units: &[trendy_load::Unit]) -> Vec<trendy_sema::File<'_>> {
    units
        .iter()
        .map(|unit| trendy_sema::File {
            name: &unit.name,
            path: &unit.path,
            tree: &unit.parse.tree,
        })
        .collect()
}

/// Rendered diagnostics, whose spans are in `sources`, plus for people a
/// summary that says where to learn more about the program at `path`.
fn report(errors: &[Diagnostic], format: Format, path: &str, sources: &Sources) -> String {
    let rendered = trendy_diag::render_all(errors, format, sources);
    match format {
        Format::Json => rendered,
        Format::Human => {
            let count = match errors.len() {
                1 => "1 error".to_string(),
                n => format!("{n} errors"),
            };
            format!(
                "{rendered}\nerror: {path} has {count}; `trendyc --explain <code>` describes each code\n"
            )
        }
    }
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Mode, String> {
    let mut input = None;
    let mut output = None;
    let mut format = Format::Human;
    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--error-format=") {
            format = Format::from_flag(value)
                .ok_or_else(|| format!("unknown error format `{value}`\n{USAGE}"))?;
            continue;
        }
        match arg.as_str() {
            "--explain" => {
                let id = args.next().ok_or(USAGE)?;
                let code = Code::from_id(&id).ok_or_else(|| {
                    let known: Vec<_> = Code::ALL.iter().map(|c| c.id()).collect();
                    format!(
                        "unknown error code `{id}`; known codes: {}",
                        known.join(", ")
                    )
                })?;
                return Ok(Mode::Explain(code));
            }
            "-o" => output = Some(PathBuf::from(args.next().ok_or(USAGE)?)),
            _ if input.is_none() && !arg.starts_with('-') => input = Some(PathBuf::from(arg)),
            _ => return Err(USAGE.to_string()),
        }
    }
    let input: PathBuf = input.ok_or(USAGE)?;
    let output = output.unwrap_or_else(|| PathBuf::from(input.file_stem().unwrap_or_default()));
    Ok(Mode::Build {
        input,
        output,
        format,
    })
}

/// The runtime library, built by `build.rs` and linked into every executable.
const RUNTIME: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/libtrendy_runtime.a"));

/// Link the object and the runtime with the system C compiler driver, which
/// supplies the platform startup code and libc.
fn link(object: &[u8], output: &Path) -> Result<(), String> {
    let temp = std::env::temp_dir();
    let id = std::process::id();
    let object_path = temp.join(format!("trendyc-{id}.o"));
    let runtime_path = temp.join(format!("trendyc-{id}-runtime.a"));
    std::fs::write(&object_path, object).map_err(|e| format!("writing object file: {e}"))?;
    std::fs::write(&runtime_path, RUNTIME).map_err(|e| format!("writing runtime: {e}"))?;

    let status = Command::new("cc")
        .arg(&object_path)
        .arg(&runtime_path)
        .arg("-o")
        .arg(output)
        .status();
    let _ = std::fs::remove_file(&object_path);
    let _ = std::fs::remove_file(&runtime_path);

    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("linker failed: {s}")),
        Err(e) => Err(format!("running linker `cc`: {e}")),
    }
}
