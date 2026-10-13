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
    run_file(name, &Path::new("examples").join(format!("{name}.trendy")))
}

/// Compile `projects/<name>/<name>.trendy`, run the executable, and return
/// its exit code and what it wrote to stderr.
fn run_project(name: &str) -> (i32, String) {
    run_file(
        name,
        &Path::new("projects")
            .join(name)
            .join(format!("{name}.trendy")),
    )
}

/// Compile `path`, relative to the workspace root, run the executable, and
/// return its exit code and what it wrote to stderr.
fn run_file(name: &str, path: &Path) -> (i32, String) {
    let input = workspace_root().join(path);
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

#[test]
fn rationals() {
    let (code, stderr) = run_example("rationals");
    assert_eq!(code, 0, "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    // Evaluated when compiling.
    assert_eq!(lines[..4], ["0.5", "18.75", "-0.0", "1e-310"]);
    // Each row: `-a`, `a + b`, `a - b`, `a * b`, `a / b`, rounded as IEEE
    // 754 rounds them, signed zeros included.
    let rows: Vec<_> = lines[4..].chunks(5).collect();
    assert_eq!(
        rows,
        [
            ["-7.0", "9.0", "5.0", "14.0", "3.5"],
            [
                "-0.1",
                "0.30000000000000004",
                "-0.1",
                "0.020000000000000004",
                "0.5"
            ],
            ["-0.0", "-1.0", "1.0", "-0.0", "-0.0"],
            ["-1e300", "1e300", "1e300", "1e292", "1e308"],
        ]
    );
}

#[test]
fn rational_failures_stop_the_program() {
    use trendy_diag::Code;
    let program = |call: &str| {
        format!(
            "operator f{{a: rational, b: rational}} [\n    call debug_value{{v: 1.0}};\n    call debug_value{{v: {call}}};\n];\n\noperator main{{}} [\n    call f{{a: 1e308, b: 0.0}};\n];\n"
        )
    };
    let overflow = format!("1.0\nerror[{}]: rational overflow\n", Code::Overflow.id());
    let division = format!(
        "1.0\nerror[{}]: division by zero\n",
        Code::DivisionByZero.id()
    );
    let cases = [
        ("add", "a + a", &overflow),
        ("subtract", "-a - a", &overflow),
        ("multiply", "a * 10.0", &overflow),
        ("divide", "a / 0.1", &overflow),
        ("divide_zero", "a / b", &division),
        ("divide_negative_zero", "a / -b", &division),
    ];
    for (name, expr, expected) in cases {
        let (code, stderr) = run_source(&format!("rational_{name}"), &program(expr));
        assert_eq!((code, &stderr), (1, expected), "{expr}");
    }
}

#[test]
fn text() {
    let (code, stderr) = run_example("text");
    assert_eq!(code, 0, "{stderr}");
    let folded = r#"tuple { t: "constant parts fold" }"#;
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            r#""hello, world""#,
            r#""""#,
            r#""hello, Ada!""#,
            r#""Ada""#,
            folded,
            r#""hello, é😀\"\n""#,
            r#""é😀\"""#,
            folded,
            r#""hello, ""#,
            r#""""#,
            folded,
        ]
    );
}

#[test]
fn comparisons() {
    let (code, stderr) = run_example("comparisons");
    assert_eq!(code, 0, "{stderr}");
    let bits: Vec<char> = stderr
        .lines()
        .map(|b| if b == "true" { '1' } else { '0' })
        .collect();
    // Evaluated when compiling.
    assert_eq!(bits[..3], ['1', '1', '1']);
    // Each scalar call: `=`, `<>`, `<`, `<=`, `>`, `>=`.
    let scalars: Vec<String> = bits[3..63]
        .chunks(6)
        .map(|row| row.iter().collect())
        .collect();
    assert_eq!(
        scalars,
        [
            "011100", // false, true
            "100101", // true, true
            "011100", // -2, 1
            "100101", // 7, 7
            "011100", // -0.0, 0.0: totalOrder
            "010011", // 2.5, -1e300
            "011100", // "Z", "a": by code point
            "011100", // "é", "😀"
            "010011", // "ab", "a"
            "100101", // "", ""
        ]
    );
    // Each tuple and relation call: `=`, `<>`.
    let values: Vec<String> = bits[63..]
        .chunks(2)
        .map(|row| row.iter().collect())
        .collect();
    assert_eq!(
        values,
        [
            "10", // equal tuples
            "01", // another attribute value
            "01", // another text
            "10", // the same tuples, listed in another order
            "01", // a subset
            "01", // against the empty relation
            "10", // two empty relations
            "01", // `0.0` and `-0.0` are different values, even nested
            "10", // nested relations compare as sets
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
    let overflow = format!("1\nerror[{}]: integer overflow\n", Code::Overflow.id());
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

/// The lines `name` prints, which are each result twice: folded when
/// compiling, then computed when running. The two halves must be equal.
fn folded_and_run(name: &str) -> Vec<String> {
    let (code, stderr) = run_example(name);
    assert_eq!(code, 0, "{stderr}");
    let lines: Vec<String> = stderr.lines().map(str::to_string).collect();
    let (folded, run) = lines.split_at(lines.len() / 2);
    assert_eq!(
        folded, run,
        "folded when compiling, and computed when running"
    );
    folded.to_vec()
}

#[test]
fn tuples() {
    assert_eq!(
        folded_and_run("tuples"),
        [
            r#"tuple { country: "France", postal_code: "75001" }"#,
            r#"tuple { country: "France", postal_code: "75001" }"#,
            "tuple {}",
            "tuple { a: tuple { b: false } }",
            r#"tuple { city: "Paris", code: "75001", country: "France", street: "1 Rue de Rivoli" }"#,
            // All at once: a swap.
            "tuple { x: 2.0, y: 1.0 }",
            // Built from run-time values.
            r#"tuple { number: tuple { n: 13 }, street: "Невский проспект!" }"#,
            // Attributes' values.
            r#""75001""#,
            "false",
            "3.0",
        ]
    );
}

#[test]
fn relations() {
    assert_eq!(
        folded_and_run("relations"),
        [
            // Duplicates dropped; text by code point, so `É` after `E`.
            r#"relation { name: text } { { name: "Ada" }, { name: "Edgar" }, { name: "Édouard" } }"#,
            r#"relation { place: text } { { place: "London" }, { place: "Paris" }, { place: "Portland" } }"#,
            r#"relation { born: integer, place: text } { { born: 1815, place: "London" }, { born: 1815, place: "Paris" }, { born: 1816, place: "London" }, { born: 1923, place: "Portland" } }"#,
            "relation {} { {} }",
            // Rationals by totalOrder.
            "relation { x: rational } { { x: -1.5 }, { x: -0.0 }, { x: 0.0 }, { x: 2.5 } }",
            // Relations by their tuples: the empty one first.
            r#"relation { members: relation { name: text } } { { members: relation { name: text } {} }, { members: relation { name: text } { { name: "Ada" }, { name: "Edgar" } } } }"#,
            "relation {} {}",
            // Renamed, and sorted again by the new first attribute.
            r#"relation { alias: text, born: integer, place: text } { { alias: "Ada", born: 1815, place: "London" }, { alias: "Ada", born: 1816, place: "London" }, { alias: "Edgar", born: 1923, place: "Portland" }, { alias: "Édouard", born: 1815, place: "Paris" } }"#,
            "relation { n: rational, x: integer } { { n: -1.5, x: 3 }, { n: -1.5, x: 5 }, { n: -0.0, x: 2 }, { n: 0.0, x: 4 }, { n: 2.5, x: 1 } }",
            // Built from run-time values, sorted, and with the tuples that
            // turn out equal made one.
            r#"relation { born: integer, name: text } { { born: 701, name: "李白" }, { born: 1815, name: "Ada" } }"#,
            "relation { x: integer } { { x: 1 } }",
            r#"tuple { members: relation { name: text } { { name: "Ada" }, { name: "李白" } }, who: "core" }"#,
            // Joined on `name`, which only Ada and Edgar have in both, and
            // composed, without it.
            r#"relation { born: integer, city: text, name: text } { { born: 1815, city: "London", name: "Ada" }, { born: 1923, city: "Portland", name: "Edgar" } }"#,
            r#"relation { born: integer, city: text } { { born: 1815, city: "London" }, { born: 1923, city: "Portland" } }"#,
            r#"relation { city: text, year: integer } { { city: "London", year: 2024 }, { city: "London", year: 2025 }, { city: "Nyeri", year: 2024 }, { city: "Nyeri", year: 2025 }, { city: "Portland", year: 2024 }, { city: "Portland", year: 2025 } }"#,
            // Joins and times of lists.
            r#"relation { born: integer, city: text, name: text } { { born: 1815, city: "London", name: "Ada" }, { born: 1923, city: "Portland", name: "Edgar" } }"#,
            r#"relation { born: integer, city: text, year: integer } { { born: 1815, city: "London", year: 2024 }, { born: 1815, city: "London", year: 2025 }, { born: 1815, city: "Nyeri", year: 2024 }, { born: 1815, city: "Nyeri", year: 2025 }, { born: 1815, city: "Portland", year: 2024 }, { born: 1815, city: "Portland", year: 2025 }, { born: 1923, city: "London", year: 2024 }, { born: 1923, city: "London", year: 2025 }, { born: 1923, city: "Nyeri", year: 2024 }, { born: 1923, city: "Nyeri", year: 2025 }, { born: 1923, city: "Portland", year: 2024 }, { born: 1923, city: "Portland", year: 2025 } }"#,
        ]
    );
}

#[test]
fn shopping() {
    let (code, stderr) = run_project("shopping");
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            r#"relation { people: integer, recipe: text } { { people: 4, recipe: "dal tadka" }, { people: 4, recipe: "shakshuka" }, { people: 6, recipe: "mapo doufu" } }"#,
            // No borshch this week, so no beetroot, cabbage, carrot or potato.
            r#"relation { ingredient: text, unit: text } { { ingredient: "Sichuan pepper", unit: "g" }, { ingredient: "cumin", unit: "g" }, { ingredient: "doubanjiang", unit: "g" }, { ingredient: "egg", unit: "piece" }, { ingredient: "garlic", unit: "clove" }, { ingredient: "ghee", unit: "g" }, { ingredient: "ginger", unit: "g" }, { ingredient: "minced pork", unit: "g" }, { ingredient: "olive oil", unit: "ml" }, { ingredient: "onion", unit: "piece" }, { ingredient: "red lentils", unit: "g" }, { ingredient: "tofu", unit: "g" }, { ingredient: "tomato", unit: "g" } }"#,
        ]
    );
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
error[P0002]: expected `]`, found `;`
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
    assert_eq!(errors[0]["code"], "P0003");
    let edits = &errors[0]["suggestions"][0]["edits"];
    assert_eq!(edits[0]["replacement"], "{");
    assert_eq!(edits[0]["start"]["offset"], 13);
    assert_eq!(edits[1]["replacement"], "}");
}

#[test]
fn explain() {
    let out = Command::new(env!("CARGO_BIN_EXE_trendyc"))
        .args(["--explain", "p0003"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .starts_with("# P0003: wrong delimiters\n")
    );

    let out = Command::new(env!("CARGO_BIN_EXE_trendyc"))
        .args(["--explain", "P9999"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("known codes: P0001,")
    );
}
