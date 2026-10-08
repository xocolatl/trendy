use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// How many examples have been compiled, to name each executable uniquely.
static RUNS: AtomicUsize = AtomicUsize::new(0);

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Compile `examples/<name>.trendy`, run the executable, and return its exit
/// code.
fn compile_and_run(name: &str) -> i32 {
    run_example(name).0
}

/// Compile `examples/<name>.trendy`, run the executable, and return its exit
/// code and what it wrote to stderr.
fn run_example(name: &str) -> (i32, String) {
    let input = workspace_root()
        .join("examples")
        .join(format!("{name}.trendy"));
    // Each run gets its own executable, since tests run in parallel.
    let run = RUNS.fetch_add(1, Ordering::Relaxed);
    let output = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{run}"));

    let compile = Command::new(env!("CARGO_BIN_EXE_trendyc"))
        .arg(&input)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "trendyc failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&output).output().unwrap();
    (
        run.status.code().unwrap(),
        String::from_utf8(run.stderr).unwrap(),
    )
}

#[test]
fn empty_main() {
    assert_eq!(compile_and_run("empty_main"), 0);
}

#[test]
fn variables() {
    assert_eq!(compile_and_run("variables"), 0);
}

#[test]
fn debug_value_prints_every_type() {
    let (code, stderr) = run_example("debug");
    assert_eq!(code, 0);
    assert_eq!(
        stderr,
        r#"true
42
0.0015
1e300
"say \"hi\"\n\ttab \\ é"
tuple { x: 1.0, y: tuple { z: false } }
tuple {}
relation { name: text } {}
relation { born: integer, name: text } { { born: 1815, name: "Ada" }, { born: 1923, name: "Edgar" } }
relation { members: relation { name: text }, team: text } { { members: relation { name: text } { { name: "Ada" } }, team: "core" } }
7
"count"
42
"#
    );
}

/// The program `source` analyzed, with the prelude.
fn analyze(source: &str) -> trendy_hir::Program {
    let parse = |source: &str| {
        let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
        assert!(parse.errors.is_empty(), "{source}: {:#?}", parse.errors);
        parse.tree
    };
    let prelude = trendy_sema::Prelude::load(&parse(trendy_prelude::SOURCE)).unwrap();
    trendy_sema::analyze(&prelude, &parse(source)).unwrap()
}

#[test]
fn debug_output_reads_back_as_the_same_value() {
    // `debug.trendy` prints its variables first, in declaration order.
    let source = std::fs::read_to_string(workspace_root().join("examples/debug.trendy")).unwrap();
    let program = analyze(&source);
    let (_, stderr) = run_example("debug");
    for (variable, line) in program.variables.iter().zip(stderr.lines()) {
        let printed = analyze(&format!("var x := {line};\noperator main{{}} [];"));
        assert_eq!(
            printed.variables[0].init, variable.init,
            "`{}` printed as {line}",
            variable.name
        );
    }
}

#[test]
fn booleans() {
    let (code, stderr) = run_example("booleans");
    assert_eq!(code, 0);
    let lines: Vec<&str> = stderr.lines().collect();
    // `u` and `chain`, evaluated when compiling.
    assert_eq!(lines[..2], ["true", "true"]);
    // Each row: `not a`, `a and b`, `a or b`, `a xor b`, `a implies b`.
    let rows: Vec<_> = lines[2..22].chunks(5).collect();
    assert_eq!(
        rows,
        [
            ["true", "false", "false", "false", "true"],
            ["true", "false", "true", "true", "true"],
            ["false", "false", "true", "true", "false"],
            ["false", "true", "true", "false", "true"],
        ]
    );
    assert_eq!(lines[22..], ["false"]);
}

#[test]
fn integers() {
    let (code, stderr) = run_example("integers");
    assert_eq!(code, 0, "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    // Evaluated when compiling.
    assert_eq!(
        lines[..4],
        ["7", "9", "-9223372036854775808", "9223372036854775807"]
    );
    // Each row: `-a`, `a + b`, `a - b`, `a * b`, `a div b`, `a mod b`.
    let rows: Vec<_> = lines[4..].chunks(6).collect();
    assert_eq!(
        rows,
        [
            ["-7", "9", "5", "14", "3", "1"],
            ["7", "-5", "-9", "-14", "-3", "-1"],
            ["-7", "5", "9", "-14", "-3", "1"],
            [
                "9223372036854775807",
                "-9223372036854775808",
                "-9223372036854775806",
                "9223372036854775807",
                "9223372036854775807",
                "0"
            ],
        ]
    );
}

/// Compile `source`, run it, and return its exit code and stderr.
fn run_source(name: &str, source: &str) -> (i32, String) {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let input = dir.join(format!("{name}.trendy"));
    std::fs::write(&input, source).unwrap();
    let output = dir.join(name);
    let compile = Command::new(env!("CARGO_BIN_EXE_trendyc"))
        .arg(&input)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "trendyc failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&output).output().unwrap();
    (
        run.status.code().unwrap(),
        String::from_utf8(run.stderr).unwrap(),
    )
}

#[test]
fn run_time_failures_stop_the_program() {
    use trendy_diag::Code;
    let program = |call: &str| {
        format!(
            "operator f{{a: integer, b: integer}} [\n    call debug_value{{v: 1}};\n    call debug_value{{v: {call}}};\n    call debug_value{{v: 2}};\n];\n\noperator main{{}} [\n    call f{{a: 9223372036854775807, b: 0}};\n];\n"
        )
    };
    let overflow = format!(
        "1\nerror[{}]: integer overflow\n",
        Code::IntegerOverflow.id()
    );
    let division = format!(
        "1\nerror[{}]: division by zero\n",
        Code::DivisionByZero.id()
    );
    let cases = [
        ("add", "a + 1", &overflow),
        ("subtract", "-a - 2", &overflow),
        ("multiply", "a * 2", &overflow),
        ("negate", "-(-a - 1)", &overflow),
        ("divide_min", "(-a - 1) div -1", &overflow),
        ("divide_zero", "a div b", &division),
        ("mod_zero", "a mod b", &division),
    ];
    for (name, expr, expected) in cases {
        let (code, stderr) = run_source(&format!("fail_{name}"), &program(expr));
        assert_eq!((code, &stderr), (1, expected), "{expr}");
    }
    // The smallest integer `mod -1` has a result.
    let (code, stderr) = run_source("mod_min", &program("(-a - 1) mod -1"));
    assert_eq!((code, &stderr[..]), (0, "1\n0\n2\n"));
}

#[test]
fn tuples() {
    assert_eq!(compile_and_run("tuples"), 0);
}

#[test]
fn relations() {
    assert_eq!(compile_and_run("relations"), 0);
}

/// Run `trendyc` on `source` with `args`, returning (success, stdout, stderr).
fn trendyc(name: &str, source: &str, args: &[&str]) -> (bool, String, String) {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let input = dir.join(format!("{name}.trendy"));
    std::fs::write(&input, source).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_trendyc"))
        .arg(&input)
        .arg("-o")
        .arg(dir.join(name))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[test]
fn human_errors() {
    let (ok, _, stderr) = trendyc("human", "operator main{}\n  [;\n", &[]);
    assert!(!ok);
    let stderr = stderr.replace(env!("CARGO_TARGET_TMPDIR"), "$TMP");
    assert_eq!(
        stderr,
        "\
error[E0002]: expected `]`, found `;`
 --> $TMP/human.trendy:2:4
  |
2 |   [;
  |    ^ expected `]` here
  = note: an operator definition has the form `operator name{} [];`
help: insert `]`
  |
2 |   [];
  |    +

error: $TMP/human.trendy has 1 error; `trendyc --explain <code>` describes each code
"
    );
}

#[test]
fn json_errors() {
    let (ok, _, stderr) = trendyc(
        "json",
        "operator main() [];\noperator main{} [];\n",
        &["--error-format=json"],
    );
    assert!(!ok);
    let errors: Vec<serde_json::Value> = stderr
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["code"], "E0003");
    let edits = &errors[0]["suggestions"][0]["edits"];
    assert_eq!(edits[0]["replacement"], "{");
    assert_eq!(edits[0]["start"]["offset"], 13);
    assert_eq!(edits[1]["replacement"], "}");
}

#[test]
fn explain() {
    let out = Command::new(env!("CARGO_BIN_EXE_trendyc"))
        .args(["--explain", "e0003"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .starts_with("# E0003: wrong delimiters\n")
    );

    let out = Command::new(env!("CARGO_BIN_EXE_trendyc"))
        .args(["--explain", "E9999"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("known codes: E0001,")
    );
}
