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
    let (code, stderr) = run_example("variables");
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            r#""kwaheri""#,
            // Inserted, deleted, updated; inserted and deleted again.
            r#"relation { born: integer, name: text } { { born: 701, name: "李白" }, { born: 1644, name: "Bashō" }, { born: 1815, name: "Ada" }, { born: 1923, name: "Wisława" } }"#,
            r#"relation { born: integer, name: text } { { born: 701, name: "李白" }, { born: 1644, name: "Bashō" }, { born: 1815, name: "Ada" } }"#,
            r#"relation { born: integer, name: text } { { born: 701, name: "李白" }, { born: 1644, name: "Bashō" }, { born: 1814, name: "Ada" } }"#,
            r#"relation { born: integer, name: text } { { born: 701, name: "李白" }, { born: 1644, name: "Bashō" }, { born: 1814, name: "Ada" } }"#,
            r#"tuple { city: "Nairobi", postal_code: "00200" }"#,
            // Swapped, and two guests swapped between rooms.
            "tuple { x: 2, y: 1 }",
            r#"relation { guest: text, room: integer } { { guest: "Amara", room: 2 }, { guest: "Kenji", room: 1 } }"#,
            // A check-in, with local variables, one of them hiding `x`.
            r#""karibu, Wanjiru""#,
            r#"relation { guest: text, room: integer } { { guest: "Wanjiru", room: 3 } }"#,
            r#"relation { guest: text, room: integer } { { guest: "Amara", room: 2 }, { guest: "Kenji", room: 1 }, { guest: "Wanjiru", room: 3 } }"#,
            "2",
            "relation { born: integer, name: text } {}",
        ]
    );
}

#[test]
fn operators() {
    let (code, stderr) = run_example("operators");
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            // Each dish's price with VAT, by a call for each tuple.
            r#"relation { country: text, dish: text, total: integer } { { country: "Ghana", dish: "jollof rice", total: 1392 }, { country: "Japan", dish: "ramen", total: 1044 }, { country: "Mexico", dish: "pozole", total: 870 } }"#,
            r#"relation { dish: text } { { dish: "pozole" } }"#,
            r#""half of 1200""#,
            "600",
            // The second argument is `previous`, which `rotate` assigns.
            r#""ramen → spicy ramen""#,
            r#""spicy ramen""#,
            r#""spicy ramen → ramen""#,
            r#""ramen""#,
            // Calls made when running, with `vat` at 16, then at 25.
            r#"relation { dish: text } { { dish: "pozole" }, { dish: "ramen" } }"#,
            r#"relation { dish: text } { { dish: "pozole" } }"#,
            r#"relation { country: text, dish: text, total: integer } { { country: "Ghana", dish: "jollof rice", total: 1500 }, { country: "Japan", dish: "ramen", total: 1125 }, { country: "Mexico", dish: "pozole", total: 937 } }"#,
            // Generic operators, for texts, relations and relations of
            // relations.
            r#""specials""#,
            r#"relation { v: text } { { v: "ramen" }, { v: "spicy ramen" } }"#,
            r#""affordable on the menu""#,
            "true",
            r#""pairs""#,
            "relation { v: relation { v: integer } } { { v: relation { v: integer } { { v: 1 }, { v: 2 } } }, { v: relation { v: integer } { { v: 3 }, { v: 4 } } } }",
            // `per` arguments, for each dish, with `vat` at 25.
            r#""with VAT""#,
            "relation { v: integer } { { v: 937 }, { v: 1125 }, { v: 1500 } }",
            r#""where from""#,
            r#"relation { v: text } { { v: "jollof rice, Ghana" }, { v: "pozole, Mexico" }, { v: "ramen, Japan" } }"#,
            // Only pozole, at 937, is under 1000.
            r#""how many under 1000""#,
            "1",
            r#""before p""#,
            r#"relation { v: text } { { v: "jollof rice" } }"#,
            r#""some under 1000""#,
            "true",
            r#""every under 1000""#,
            "false",
            // `min` and `max`, of integers and of texts.
            r#""prices with VAT""#,
            "tuple { high: 1500, low: 937 }",
            r#""names""#,
            r#"tuple { high: "ramen", low: "jollof rice" }"#,
            // `cost` of integers, 937, 1125 and 1500 twice and 300 more, and
            // of approximate numbers, 1125 one and a half times and 300.
            r#""two of each, delivered""#,
            "relation { v: integer } { { v: 2174 }, { v: 2550 }, { v: 3300 } }",
            r#""one and a half ramen, delivered""#,
            "1.9875e3",
            // With `vat` at 25: pozole at 937, and ramen at 1125.
            r#""under 1200""#,
            "tuple { dishes: 2, lowest: 937 }",
        ]
    );
}

#[test]
fn debug_value_prints_every_type() {
    let (code, stderr) = run_example("debug");
    assert_eq!(code, 0);
    assert_eq!(
        stderr,
        r#"true
42
1.5e-3
1e300
"say \"hi\"\n\ttab \\ é"
tuple { x: 1e0, y: tuple { z: false } }
tuple {}
relation { name: text } {}
relation { born: integer, name: text } { { born: 1815, name: "Ada" }, { born: 1923, name: "Edgar" } }
relation { members: relation { name: text }, team: text } { { members: relation { name: text } { { name: "Ada" } }, team: "core" } }
array [1200, 950, 220]
array text []
tuple { greeting: "\"say \\\"hi\\\"\\n\\ttab \\\\ é\"", point: "tuple { x: 1e0, y: tuple { z: false } }", rainfall: "array [1200, 950, 220]", teams: "relation { members: relation { name: text }, team: text } { { members: relation { name: text } { { name: \"Ada\" } }, team: \"core\" } }" }
7
"count"
42
"\"say \\\"hi\\\"\\n\\ttab \\\\ é\""
"tuple { x: 1e0, y: tuple { z: false } }"
"array [1200, 950, 220]"
"relation { members: relation { name: text }, team: text } { { members: relation { name: text } { { name: \"Ada\" } }, team: \"core\" } }"
"#
    );
}

#[test]
fn to_text_is_what_debug_value_writes() {
    let (_, stderr) = run_example("debug");
    let lines: Vec<&str> = stderr.lines().collect();
    let text = |line: &str| {
        let program = analyze(&format!("var x := {line};\noperator main{{}} [];"));
        program.variables[0].init.clone()
    };
    let written = |written: &str| trendy_hir::Value::Text(written.to_string());
    // `greeting`, `point`, `rainfall` and `teams`, in name order: folded
    // when compiling into `written`, and made when running by `as_text`.
    let debug = [lines[4], lines[5], lines[10], lines[9]];
    let trendy_hir::Value::Tuple(folded) = text(lines[12]) else {
        panic!("{}", lines[12]);
    };
    let folded: Vec<_> = folded.attributes().map(|(_, v)| v.clone()).collect();
    let run: Vec<_> = lines[16..].iter().map(|line| text(line)).collect();
    let expected: Vec<_> = debug.iter().map(|line| written(line)).collect();
    assert_eq!(folded, expected);
    assert_eq!(run, expected);
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
fn approximate_numbers() {
    let (code, stderr) = run_example("approximate");
    assert_eq!(code, 0, "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    // Evaluated when compiling.
    assert_eq!(
        lines[..5],
        ["5e-1", "1.875e1", "-0e0", "1e-310", "9.007199254740992e15"]
    );
    // Each row: `-a`, `a + b`, `a - b`, `a * b`, `a / b`, rounded as IEEE
    // 754 rounds them, signed zeros included.
    let rows: Vec<_> = lines[5..25].chunks(5).collect();
    assert_eq!(
        rows,
        [
            ["-7e0", "9e0", "5e0", "1.4e1", "3.5e0"],
            [
                "-1e-1",
                "3.0000000000000004e-1",
                "-1e-1",
                "2.0000000000000004e-2",
                "5e-1"
            ],
            ["-0e0", "-1e0", "1e0", "-0e0", "-0e0"],
            ["-1e300", "1e300", "1e300", "1e292", "1e308"],
        ]
    );
    // Integers widened when running: beyond 2^53, to the nearest.
    assert_eq!(
        lines[25..],
        ["3e0", "9.007199254740992e15", "-9.223372036854776e18"]
    );
}

#[test]
fn approximate_failures_stop_the_program() {
    use trendy_diag::Code;
    let program = |call: &str| {
        format!(
            "operator f{{a: approximate, b: approximate}} [\n    call debug_value{{v: 1e0}};\n    call debug_value{{v: {call}}};\n];\n\noperator main{{}} [\n    call f{{a: 1e308, b: 0e0}};\n];\n"
        )
    };
    let overflow = format!(
        "1e0\nerror[{}]: approximate overflow\n",
        Code::Overflow.id()
    );
    let division = format!(
        "1e0\nerror[{}]: division by zero\n",
        Code::DivisionByZero.id()
    );
    let cases = [
        ("add", "a + a", &overflow),
        ("subtract", "-a - a", &overflow),
        ("multiply", "a * 10e0", &overflow),
        ("divide", "a / 0.1e0", &overflow),
        ("divide_zero", "a / b", &division),
        ("divide_negative_zero", "a / -b", &division),
    ];
    for (name, expr, expected) in cases {
        let (code, stderr) = run_source(&format!("approximate_{name}"), &program(expr));
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

/// A program that writes 1, then `call`, computed from `a`, the largest
/// integer, and `b`, 0, when it runs, then 2.
fn program(call: &str) -> String {
    format!(
        "operator f{{a: integer, b: integer}} [\n    call debug_value{{v: 1}};\n    call debug_value{{v: {call}}};\n    call debug_value{{v: 2}};\n];\n\noperator main{{}} [\n    call f{{a: 9223372036854775807, b: 0}};\n];\n"
    )
}

#[test]
fn run_time_failures_stop_the_program() {
    use trendy_diag::Code;
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
        // In a condition, computed for each tuple.
        ("where", "relation { { n: a } } where n + 1 > 0", &overflow),
    ];
    for (name, expr, expected) in cases {
        let (code, stderr) = run_source(&format!("fail_{name}"), &program(expr));
        assert_eq!((code, &stderr), (1, expected), "{expr}");
    }
    // The smallest integer `mod -1` has a result.
    let (code, stderr) = run_source("mod_min", &program("(-a - 1) mod -1"));
    assert_eq!((code, &stderr[..]), (0, "1\n0\n2\n"));
    // A relation with no tuples, or several, has no only tuple.
    for (name, expr, what) in [
        (
            "fail_no_tuple",
            "tuple from (relation { { n: a } } join relation { { n: b } })",
            "no tuples",
        ),
        (
            "fail_several_tuples",
            "value from relation { { n: a }, { n: b } }",
            "several tuples",
        ),
    ] {
        let (code, stderr) = run_source(name, &program(expr));
        assert_eq!(
            (code, &stderr[..]),
            (
                1,
                &*format!(
                    "1\nerror[{}]: the relation has {what}, but exactly one was taken\n",
                    Code::NotOneTuple.id()
                )
            ),
            "{expr}"
        );
    }
    // A disjoint union whose operands share a tuple, and an included minus
    // that removes one that isn't there.
    for (name, expr, failure) in [
        (
            "fail_d_union",
            "relation { { n: a } } d_union relation { { n: a } }",
            format!(
                "error[{}]: d_union or d_insert adds a tuple that is already there",
                Code::SharedTuple.id()
            ),
        ),
        (
            "fail_i_minus",
            "relation { { n: a } } i_minus relation { { n: b } }",
            format!(
                "error[{}]: i_minus or i_delete removes a tuple that isn't there",
                Code::MissingTuple.id()
            ),
        ),
    ] {
        let (code, stderr) = run_source(name, &program(expr));
        assert_eq!((code, stderr), (1, format!("1\n{failure}\n")), "{expr}");
    }
    // An array has no element at position 0, nor beyond its length.
    for (name, expr) in [
        ("fail_index_zero", "array [a][b]"),
        ("fail_index_beyond", "array [a][b + 2]"),
    ] {
        let (code, stderr) = run_source(name, &program(expr));
        assert_eq!(
            (code, &stderr[..]),
            (
                1,
                &*format!(
                    "1\nerror[{}]: an array has no element at that position\n",
                    Code::IndexOutOfRange.id()
                )
            ),
            "{expr}"
        );
    }
    // Tuples that disagree on a common attribute have no join.
    let (code, stderr) = run_source(
        "fail_tuple_join",
        &program("tuple { n: a, m: 1 } join tuple { n: b }"),
    );
    assert_eq!(
        (code, &stderr[..]),
        (
            1,
            &*format!(
                "1\nerror[{}]: tuples disagree on a common attribute\n",
                Code::TupleMismatch.id()
            )
        )
    );
}

#[test]
fn min_and_max_of_no_tuples_stop_the_program() {
    use trendy_diag::Code;
    for (name, expr, which) in [
        (
            "fail_no_min",
            "min{r: relation { { n: a } } where n = b, of: n}",
            "min",
        ),
        (
            "fail_no_max",
            "max{r: relation { { n: a } } where n = b, of: n}",
            "max",
        ),
    ] {
        let (code, stderr) = run_source(name, &program(expr));
        assert_eq!(
            (code, stderr),
            (
                1,
                format!(
                    "1\nerror[{}]: the relation has no tuples, so it has no {which}\n",
                    Code::NoMinOrMax.id()
                )
            ),
            "{expr}"
        );
    }
}

/// Operators and variables have separate namespaces, so one name can be
/// both, and each compiles to its own symbol: even `main`, the entry point.
#[test]
fn a_variable_can_have_an_operators_name() {
    let source = r#"var greet := "Hello";
var debug_value := relation { { name: "Ada" } };
var main := 7;

operator greet{name: text} [
    call debug_value{v: greet || ", " || name};
];

operator main{} [
    call greet{name: "Amara"};
    call debug_value{v: (debug_value){}};
    call debug_value{v: main};
    greet := "Habari";
    call greet{name: "Kenji"};
];
"#;
    let (code, stderr) = run_source("namespaces", source);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            r#""Hello, Amara""#,
            "relation {} { {} }",
            "7",
            r#""Habari, Kenji""#
        ]
    );
}

/// Assignments that break a rule stop the program: two changes that
/// remove the same tuple, a disjoint insert of a tuple already there, an
/// included delete of one that isn't, and two tuples with the same key.
#[test]
fn assignments_that_break_a_rule_stop_the_program() {
    use trendy_diag::Code;
    let assigning = |statement: &str| {
        format!(
            "var poets := relation {{ {{ name: \"Ada\", born: 1815 }} }}\nkey {{ name }};\n\noperator f{{a: integer}} [\n    call debug_value{{v: 1}};\n    {statement};\n    call debug_value{{v: 2}};\n];\n\noperator main{{}} [\n    call f{{a: 1815}};\n];\n"
        )
    };
    for (name, statement, failure) in [
        (
            // `variables.trendy`'s example of changes that conflict.
            "fail_conflict",
            r#"assign { update poets { born := a } where name = "Ada"; delete poets where born < 1900; }"#,
            format!(
                "error[{}]: two changes in an assign block remove the same tuple",
                Code::ConflictingChanges.id()
            ),
        ),
        (
            "fail_d_insert",
            r#"d_insert poets relation { { name: "Ada", born: a } }"#,
            format!(
                "error[{}]: d_union or d_insert adds a tuple that is already there",
                Code::SharedTuple.id()
            ),
        ),
        (
            "fail_i_delete",
            r#"i_delete poets relation { { name: "Ada", born: a + 1 } }"#,
            format!(
                "error[{}]: i_minus or i_delete removes a tuple that isn't there",
                Code::MissingTuple.id()
            ),
        ),
        (
            "fail_key",
            r#"insert poets relation { { name: "Ada", born: a + 1 } }"#,
            format!(
                "error[{}]: two tuples of a variable have the same values for one of its keys",
                Code::KeyViolated.id()
            ),
        ),
    ] {
        let (code, stderr) = run_source(name, &assigning(statement));
        assert_eq!(
            (code, stderr),
            (1, format!("1\n{failure}\n")),
            "{statement}"
        );
    }
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
            "tuple { x: 2e0, y: 1e0 }",
            // Built from run-time values.
            r#"tuple { number: tuple { n: 13 }, street: "Невский проспект!" }"#,
            // Attributes' values.
            r#""75001""#,
            "false",
            "3e0",
            // Extended.
            r#"tuple { city: "Paris", country: "France", floor: 2, line: "1 Rue de Rivoli, Paris", postal_code: "75001", street: "1 Rue de Rivoli" }"#,
            "tuple { outer: tuple { inner: 1e0 }, total: 3e0, x: 1e0, y: 2e0 }",
            // Replaced, each value computed from the old tuple.
            r#"tuple { city: "Paris", country: "France", postal_code: "75002", street: "1 Rue de Rivoli" }"#,
            "tuple { x: 4e0, y: 1e0 }",
            // Wrapped, wrapped again into another, and unwrapped back.
            r#"tuple { country: "France", place: tuple { city: "Paris", street: "1 Rue de Rivoli" }, postal_code: "75001" }"#,
            r#"tuple { location: tuple { country: "France", place: tuple { city: "Paris", street: "1 Rue de Rivoli" } }, postal_code: "75001" }"#,
            r#"tuple { city: "Paris", country: "France", postal_code: "75001", street: "1 Rue de Rivoli" }"#,
            // Joined, composed, times, and a list joined.
            r#"tuple { city: "Paris", country: "France", district: "1st", postal_code: "75001", street: "1 Rue de Rivoli" }"#,
            r#"tuple { city: "Paris", country: "France", district: "1st", street: "1 Rue de Rivoli" }"#,
            r#"tuple { city: "Paris", country: "France", postal_code: "75001", street: "1 Rue de Rivoli", year: 2024 }"#,
            r#"tuple { city: "Paris", country: "France", district: "1st", postal_code: "75001", region: "Île-de-France", street: "1 Rue de Rivoli" }"#,
            // The only tuple, and the only value of it.
            r#"tuple { city: "Paris", country: "France" }"#,
            r#""France""#,
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
            // Approximate numbers by totalOrder.
            "relation { x: approximate } { { x: -1.5e0 }, { x: -0e0 }, { x: 0e0 }, { x: 2.5e0 } }",
            // Relations by their tuples: the empty one first.
            r#"relation { members: relation { name: text } } { { members: relation { name: text } {} }, { members: relation { name: text } { { name: "Ada" }, { name: "Edgar" } } } }"#,
            "relation {} {}",
            // Renamed, and sorted again by the new first attribute.
            r#"relation { alias: text, born: integer, place: text } { { alias: "Ada", born: 1815, place: "London" }, { alias: "Ada", born: 1816, place: "London" }, { alias: "Edgar", born: 1923, place: "Portland" }, { alias: "Édouard", born: 1815, place: "Paris" } }"#,
            "relation { n: approximate, x: integer } { { n: -1.5e0, x: 3 }, { n: -1.5e0, x: 5 }, { n: -0e0, x: 2 }, { n: 0e0, x: 4 }, { n: 2.5e0, x: 1 } }",
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
            // Composed from the left: on `name`, then on `city`.
            r#"relation { born: integer, country: text } { { born: 1815, country: "United Kingdom" }, { born: 1923, country: "United States" } }"#,
            // Extended, each tuple computing its own; a relation-valued
            // attribute extended in turn, empty or not.
            r#"relation { age: integer, born: integer, greeting: text, name: text } { { age: 101, born: 1923, greeting: "Hi, Edgar", name: "Edgar" }, { age: 209, born: 1815, greeting: "Hi, Ada", name: "Ada" } }"#,
            r#"relation { members: relation { name: text }, roster: relation { name: text, role: text }, team: text } { { members: relation { name: text } {}, roster: relation { name: text, role: text } {}, team: "docs" }, { members: relation { name: text } { { name: "Ada" }, { name: "Edgar" } }, roster: relation { name: text, role: text } { { name: "Ada", role: "member" }, { name: "Edgar", role: "member" } }, team: "core" } }"#,
            // Replaced, each tuple computing its own; tuples that become
            // equal are one.
            r#"relation { born: integer, name: text } { { born: 1816, name: "Ada" }, { born: 1924, name: "Edgar" } }"#,
            "relation { born: integer } { { born: 1 } }",
            // Image relations: Nyeri's is empty; and a relation comparison
            // of each.
            r#"relation { city: text, natives: relation { born: integer, name: text } } { { city: "London", natives: relation { born: integer, name: text } { { born: 1815, name: "Ada" }, { born: 1816, name: "Ada" } } }, { city: "Nyeri", natives: relation { born: integer, name: text } {} }, { city: "Portland", natives: relation { born: integer, name: text } { { born: 1923, name: "Edgar" } } } }"#,
            r#"relation { name: text, only_london: boolean } { { name: "Ada", only_london: true }, { name: "Edgar", only_london: false }, { name: "Édouard", only_london: false } }"#,
            // Grouped: one tuple per place; two groupings at once, each of
            // the same births; and every attribute of each team's members,
            // so one tuple, or none for a team without members.
            r#"relation { births: relation { born: integer, name: text }, place: text } { { births: relation { born: integer, name: text } { { born: 1815, name: "Ada" }, { born: 1816, name: "Ada" } }, place: "London" }, { births: relation { born: integer, name: text } { { born: 1815, name: "Édouard" } }, place: "Paris" }, { births: relation { born: integer, name: text } { { born: 1923, name: "Edgar" } }, place: "Portland" } }"#,
            r#"relation { names: relation { name: text }, place: text, years: relation { born: integer } } { { names: relation { name: text } { { name: "Ada" } }, place: "London", years: relation { born: integer } { { born: 1815 }, { born: 1816 } } }, { names: relation { name: text } { { name: "Edgar" } }, place: "Portland", years: relation { born: integer } { { born: 1923 } } }, { names: relation { name: text } { { name: "Édouard" } }, place: "Paris", years: relation { born: integer } { { born: 1815 } } } }"#,
            r#"relation { members: relation { name: text }, roster: relation { names: relation { name: text } }, team: text } { { members: relation { name: text } {}, roster: relation { names: relation { name: text } } {}, team: "docs" }, { members: relation { name: text } { { name: "Ada" }, { name: "Edgar" } }, roster: relation { names: relation { name: text } } { { names: relation { name: text } { { name: "Ada" }, { name: "Edgar" } } } }, team: "core" } }"#,
            // Ungrouped: the docs team, with no members, has no tuples; and
            // grouping then ungrouping gives the births back.
            r#"relation { name: text, team: text } { { name: "Ada", team: "core" }, { name: "Edgar", team: "core" } }"#,
            r#"relation { born: integer, name: text, place: text } { { born: 1815, name: "Ada", place: "London" }, { born: 1815, name: "Édouard", place: "Paris" }, { born: 1816, name: "Ada", place: "London" }, { born: 1923, name: "Edgar", place: "Portland" } }"#,
            // Wrapped, and unwrapped back.
            r#"relation { place: text, who: tuple { born: integer, name: text } } { { place: "London", who: tuple { born: 1815, name: "Ada" } }, { place: "London", who: tuple { born: 1816, name: "Ada" } }, { place: "Paris", who: tuple { born: 1815, name: "Édouard" } }, { place: "Portland", who: tuple { born: 1923, name: "Edgar" } } }"#,
            r#"relation { born: integer, name: text, place: text } { { born: 1815, name: "Ada", place: "London" }, { born: 1815, name: "Édouard", place: "Paris" }, { born: 1816, name: "Ada", place: "London" }, { born: 1923, name: "Edgar", place: "Portland" } }"#,
            // Restricted, by a condition, by two, and of a join.
            r#"relation { born: integer, name: text, place: text } { { born: 1815, name: "Ada", place: "London" }, { born: 1815, name: "Édouard", place: "Paris" }, { born: 1816, name: "Ada", place: "London" } }"#,
            r#"relation { born: integer, name: text, place: text } { { born: 1815, name: "Édouard", place: "Paris" } }"#,
            r#"relation { born: integer, city: text, name: text, place: text } { { born: 1815, city: "London", name: "Ada", place: "London" }, { born: 1816, city: "London", name: "Ada", place: "London" }, { born: 1923, city: "Portland", name: "Edgar", place: "Portland" } }"#,
            // Matching, and not matching without the common attribute.
            r#"relation { born: integer, name: text } { { born: 1815, name: "Ada" }, { born: 1923, name: "Edgar" } }"#,
            r#"relation { born: integer, name: text } { { born: 1815, name: "Édouard" } }"#,
            // Union, intersection, difference, of lists, an exclusive union,
            // and inclusion.
            r#"relation { name: text } { { name: "Ada" }, { name: "Edgar" }, { name: "Wangari" }, { name: "Édouard" } }"#,
            r#"relation { name: text } { { name: "Ada" }, { name: "Edgar" } }"#,
            r#"relation { name: text } { { name: "Édouard" } }"#,
            r#"relation { name: text } { { name: "Ada" }, { name: "Edgar" }, { name: "Wangari" }, { name: "Édouard" } }"#,
            r#"relation { name: text } { { name: "Édouard" } }"#,
            r#"relation { name: text } { { name: "Wangari" }, { name: "Édouard" } }"#,
            // Included, as sets: everyone in `people` is in `cities`, but
            // not everyone in `births`.
            "tuple { born: false, listed: true }",
            // A disjoint union, and an included minus.
            r#"relation { name: text } { { name: "Ada" }, { name: "Edgar" }, { name: "Wangari" }, { name: "Édouard" } }"#,
            r#"relation { name: text } { { name: "Édouard" } }"#,
            // Aggregates: -1.5 counts twice in the sum of `x`.
            "4",
            "15",
            "-5e-1",
            "1923",
            // 5!, and the rates compounded, exactly, then rounded once.
            "120",
            "1.0804500000000001e0",
            // Per group, Puno with none; and the two born in 1815.
            r#"relation { crops: integer, region: text, tonnes: integer } { { crops: 0, region: "Puno", tonnes: 0 }, { crops: 1, region: "Mendoza", tonnes: 300 }, { crops: 2, region: "Nyeri", tonnes: 165 }, { crops: 2, region: "Punjab", tonnes: 1900 } }"#,
            r#"relation { born: integer, name: text, place: text } { { born: 1815, name: "Ada", place: "London" }, { born: 1815, name: "Édouard", place: "Paris" } }"#,
        ]
    );
}

#[test]
fn arrays() {
    assert_eq!(
        folded_and_run("arrays"),
        [
            "array [1200, 950, 220]",
            // Empty, with its elements' type.
            "array text []",
            "array [array [1, 2], array [3]]",
            // Cairo's empty array took its type from Mumbai's.
            r#"relation { city: text, rainy: array text } { { city: "Cairo", rainy: array text [] }, { city: "Mumbai", rainy: array ["Jun", "Jul", "Aug", "Sep"] } }"#,
            // Element by element, a prefix first.
            "relation { m: array integer } { { m: array integer [] }, { m: array [1, 9] }, { m: array [2] } }",
            // Positions count from 1.
            "950",
            "2",
            "3",
            // Equal only in the same order.
            "tuple { copied: true, reordered: false }",
            // Generic over the elements' type.
            "1200",
            r#""Amara""#,
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
            // No borshch this week, so no beetroot, cabbage, carrot or
            // potato; each ingredient with the recipes that need it.
            r#"relation { ingredient: text, recipes: relation { recipe: text }, unit: text } { { ingredient: "Sichuan pepper", recipes: relation { recipe: text } { { recipe: "mapo doufu" } }, unit: "g" }, { ingredient: "cumin", recipes: relation { recipe: text } { { recipe: "dal tadka" }, { recipe: "shakshuka" } }, unit: "g" }, { ingredient: "doubanjiang", recipes: relation { recipe: text } { { recipe: "mapo doufu" } }, unit: "g" }, { ingredient: "egg", recipes: relation { recipe: text } { { recipe: "shakshuka" } }, unit: "piece" }, { ingredient: "garlic", recipes: relation { recipe: text } { { recipe: "dal tadka" }, { recipe: "mapo doufu" }, { recipe: "shakshuka" } }, unit: "clove" }, { ingredient: "ghee", recipes: relation { recipe: text } { { recipe: "dal tadka" } }, unit: "g" }, { ingredient: "ginger", recipes: relation { recipe: text } { { recipe: "dal tadka" }, { recipe: "mapo doufu" } }, unit: "g" }, { ingredient: "minced pork", recipes: relation { recipe: text } { { recipe: "mapo doufu" } }, unit: "g" }, { ingredient: "olive oil", recipes: relation { recipe: text } { { recipe: "shakshuka" } }, unit: "ml" }, { ingredient: "onion", recipes: relation { recipe: text } { { recipe: "dal tadka" }, { recipe: "shakshuka" } }, unit: "piece" }, { ingredient: "red lentils", recipes: relation { recipe: text } { { recipe: "dal tadka" } }, unit: "g" }, { ingredient: "tofu", recipes: relation { recipe: text } { { recipe: "mapo doufu" } }, unit: "g" }, { ingredient: "tomato", recipes: relation { recipe: text } { { recipe: "dal tadka" }, { recipe: "shakshuka" } }, unit: "g" } }"#,
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
  = note: an operator definition has the form `operator name{} [];`, or, for a read-only operator, `operator name{}: type [return value;];`
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
