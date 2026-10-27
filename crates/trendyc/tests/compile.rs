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
    build_and_run(&input, &output)
}

/// Compile `input` into the executable `output`, run it, and return its
/// exit code and what it wrote to stderr. The C library scribbles over
/// memory as it is freed (`MallocScribble` on macOS, `MALLOC_PERTURB_`
/// with glibc), so that a program that reads a value whose buffer was
/// freed writes garbage, rather than, by luck, what it should.
fn build_and_run(input: &Path, output: &Path) -> (i32, String) {
    let compile = Command::new(env!("CARGO_BIN_EXE_trendyc"))
        .arg(input)
        .arg("-o")
        .arg(output)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "trendyc failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(output)
        .env("MallocScribble", "1")
        .env("MallocPreScribble", "1")
        .env("MALLOC_PERTURB_", "85")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&run.stderr).into_owned();
    let code = run
        .status
        .code()
        .unwrap_or_else(|| panic!("the program crashed ({}): {stderr}", run.status));
    (code, stderr)
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
            "┌──────┬─────────┐",
            "│ born │ name    │",
            "├──────┼─────────┤",
            "│  701 │ 李白    │",
            "│ 1644 │ Bashō   │",
            "│ 1815 │ Ada     │",
            "│ 1923 │ Wisława │",
            "└──────┴─────────┘",
            "┌──────┬───────┐",
            "│ born │ name  │",
            "├──────┼───────┤",
            "│  701 │ 李白  │",
            "│ 1644 │ Bashō │",
            "│ 1815 │ Ada   │",
            "└──────┴───────┘",
            "┌──────┬───────┐",
            "│ born │ name  │",
            "├──────┼───────┤",
            "│  701 │ 李白  │",
            "│ 1644 │ Bashō │",
            "│ 1814 │ Ada   │",
            "└──────┴───────┘",
            "┌──────┬───────┐",
            "│ born │ name  │",
            "├──────┼───────┤",
            "│  701 │ 李白  │",
            "│ 1644 │ Bashō │",
            "│ 1814 │ Ada   │",
            "└──────┴───────┘",
            r#"tuple { city: "Nairobi", postal_code: "00200" }"#,
            // Swapped, and two guests swapped between rooms.
            "tuple { x: 2, y: 1 }",
            "┌───────┬──────┐",
            "│ guest │ room │",
            "├───────┼──────┤",
            "│ Amara │    2 │",
            "│ Kenji │    1 │",
            "└───────┴──────┘",
            // A check-in, with local variables, one of them hiding `x`.
            r#""karibu, Wanjiru""#,
            "┌─────────┬──────┐",
            "│ guest   │ room │",
            "├─────────┼──────┤",
            "│ Wanjiru │    3 │",
            "└─────────┴──────┘",
            "┌─────────┬──────┐",
            "│ guest   │ room │",
            "├─────────┼──────┤",
            "│ Amara   │    2 │",
            "│ Kenji   │    1 │",
            "│ Wanjiru │    3 │",
            "└─────────┴──────┘",
            "2",
            // Checked out, and an `if` for a guest without a room.
            r#""Kenji leaves room 1""#,
            r#""Bashō has no room""#,
            "┌─────────┬──────┐",
            "│ guest   │ room │",
            "├─────────┼──────┤",
            "│ Amara   │    2 │",
            "│ Wanjiru │    3 │",
            "└─────────┴──────┘",
            // A roll call, by a `while` over the guests' positions, three
            // knocks, by a `loop`, and goodbyes, by a `for` over the guests.
            r#""1. Amara""#,
            r#""2. Wanjiru""#,
            "3",
            r#""kwaheri, Amara""#,
            r#""kwaheri, Wanjiru""#,
            "┌──────┬──────┐",
            "│ born │ name │",
            "├──────┼──────┤",
            "└──────┴──────┘",
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
            "┌─────────┬─────────────┬───────┐",
            "│ country │ dish        │ total │",
            "├─────────┼─────────────┼───────┤",
            "│ Ghana   │ jollof rice │  1392 │",
            "│ Japan   │ ramen       │  1044 │",
            "│ Mexico  │ pozole      │   870 │",
            "└─────────┴─────────────┴───────┘",
            "┌────────┐",
            "│ dish   │",
            "├────────┤",
            "│ pozole │",
            "└────────┘",
            r#""half of 1200""#,
            "600",
            // The second argument is `previous`, which `rotate` assigns.
            r#""ramen → spicy ramen""#,
            r#""spicy ramen""#,
            r#""spicy ramen → ramen""#,
            r#""ramen""#,
            // Calls made when running, with `vat` at 16, then at 25.
            "┌────────┐",
            "│ dish   │",
            "├────────┤",
            "│ pozole │",
            "│ ramen  │",
            "└────────┘",
            "┌────────┐",
            "│ dish   │",
            "├────────┤",
            "│ pozole │",
            "└────────┘",
            "┌─────────┬─────────────┬───────┐",
            "│ country │ dish        │ total │",
            "├─────────┼─────────────┼───────┤",
            "│ Ghana   │ jollof rice │  1500 │",
            "│ Japan   │ ramen       │  1125 │",
            "│ Mexico  │ pozole      │   937 │",
            "└─────────┴─────────────┴───────┘",
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
            // `if`s: only pozole, at 937, is under 1000, and none under
            // 500, whose mean is 0, with no division by zero.
            r#""mean under 1000""#,
            "937",
            r#""mean under 500""#,
            "0",
            r#""how many under 1200""#,
            r#""several""#,
            r#""the menu's""#,
            r#""dishes""#,
            // Value blocks: the spread at 16, folded, and at 25, when
            // running, 1500 less 937; pozole's band, at 937.
            r#""spread""#,
            "522",
            r#""spread now""#,
            "563",
            r#""pozole""#,
            r#""under 1000 by 63""#,
        ]
    );
}

#[test]
fn debug_value_prints_every_type_and_debug_table_relations() {
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
┌──────┬───────┐
│ born │ name  │
├──────┼───────┤
│ 1815 │ Ada   │
│ 1923 │ Edgar │
└──────┴───────┘
┌──────┐
│ name │
├──────┤
└──────┘
┌─────────────────────────────────────────────┬──────┐
│ members                                     │ team │
├─────────────────────────────────────────────┼──────┤
│ relation { name: text } { { name: "Ada" } } │ core │
└─────────────────────────────────────────────┴──────┘
┌───────┬──────────┬────────┐
│ city  │ millions │ native │
├───────┼──────────┼────────┤
│ Kyiv  │        3 │ Київ   │
│ Lima  │       10 │ Lima   │
│ Xi'an │       13 │ 西安   │
└───────┴──────────┴────────┘
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
    let run: Vec<_> = lines[16..20].iter().map(|line| text(line)).collect();
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
    // `u`, `chain`, `every_one` and `odd`, evaluated when compiling.
    assert_eq!(lines[..4], ["true", "true", "true", "false"]);
    // Each row: `not a`, `a and b`, `a or b`, `a xor b`, `a implies b`,
    // `xor{a, b, true}`.
    let rows: Vec<_> = lines[4..28].chunks(6).collect();
    assert_eq!(
        rows,
        [
            ["true", "false", "false", "false", "true", "true"],
            ["true", "false", "true", "true", "true", "false"],
            ["false", "false", "true", "true", "false", "false"],
            ["false", "true", "true", "false", "true", "true"],
        ]
    );
    assert_eq!(lines[28], "false");
    // Guards, wherever they are written: `steep` when compiling, then the
    // same, `implies`, a call, an `or` and an `and{…}` when running, with
    // no division by zero.
    let steep = [
        "┌───┬───┐",
        "│ x │ y │",
        "├───┼───┤",
        "│ 2 │ 6 │",
        "└───┴───┘",
    ];
    assert_eq!(lines[29..34], steep);
    assert_eq!(lines[34..39], steep);
    assert_eq!(
        lines[39..],
        [
            "┌───┬───┐",
            "│ x │ y │",
            "├───┼───┤",
            "│ 0 │ 5 │",
            "│ 2 │ 6 │",
            "└───┴───┘",
            "false",
            "true",
            "false",
        ]
    );
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
        ["5e-1", "1.875e1", "0e0", "1e-310", "9.007199254740992e15"]
    );
    // Each row: `-a`, `a + b`, `a - b`, `a * b`, `a / b`, rounded as IEEE
    // 754 rounds them, except that there is one zero.
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
            ["0e0", "-1e0", "1e0", "0e0", "0e0"],
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
            "100101", // -0e0, 0e0: one zero
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
            "10", // `0e0` and `-0e0` are one value, even nested
            "10", // nested relations compare as sets
        ]
    );
}

/// Compile `source`, run it, and return its exit code and stderr.
fn run_source(name: &str, source: &str) -> (i32, String) {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let input = dir.join(format!("{name}.trendy"));
    std::fs::write(&input, source).unwrap();
    build_and_run(&input, &dir.join(name))
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
fn an_if_fails_only_if_the_branch_it_takes_does() {
    let overflow = format!(
        "1\nerror[{}]: integer overflow\n",
        trendy_diag::Code::Overflow.id()
    );
    let (code, stderr) = run_source("if_safe", &program("if b = 0 then 0 else a div b"));
    assert_eq!((code, &stderr[..]), (0, "1\n0\n2\n"));
    let (code, stderr) = run_source("fail_if", &program("if b = 0 then a + 1 else 0"));
    assert_eq!((code, stderr), (1, overflow));
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
    call debug_table{r: (debug_value){}};
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
            "┌┐",
            "││",
            "├┤",
            "││",
            "└┘",
            "7",
            r#""Habari, Kenji""#
        ]
    );
}

/// A value lasts as long as something holds it: an argument once the
/// variable it came from, the only other holder, is assigned; variables
/// swapped; a local variable declared again each time a loop runs; and
/// texts made in a statement, in an array and a relation assigned there.
/// Freed memory is scribbled over, so a value read after it is freed
/// writes garbage.
#[test]
fn values_last_as_long_as_something_holds_them() {
    let source = r#"var greeting := "hello";
var left := "kushoto";
var right := "kulia";
var shouts := array ["none"];
var cities := relation { { city: "Lagos" }, { city: "Lima" } };
var loud := relation { { city: "none", shout: "none" } };

operator say_goodbye{old: text} [
    greeting := "goodbye";
    call debug_value{v: old};
];

operator main{} [
    greeting := greeting || ", world";
    call say_goodbye{old: greeting};
    left := left || "!";
    right := right || "!";
    assign {
        left := right;
        right := left;
    };
    call debug_value{v: left || " " || right};
    var i := 0;
    var all := "";
    while i < 3 loop [
        var t := to_text{from: i} || ",";
        all := all || t;
        i := i + 1;
    ];
    call debug_value{v: all};
    shouts := array [greeting || "!", left || "?"];
    call debug_value{v: shouts};
    loud := (cities extend { shout := city || "!" }) where shout <> "Lima!";
    call debug_table{r: loud};
];
"#;
    let (code, stderr) = run_source("lifetimes", source);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            r#""hello, world""#,
            r#""kulia! kushoto!""#,
            r#""0,1,2,""#,
            r#"array ["goodbye!", "kulia!?"]"#,
            "┌───────┬────────┐",
            "│ city  │ shout  │",
            "├───────┼────────┤",
            "│ Lagos │ Lagos! │",
            "└───────┴────────┘",
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
            // The only tuple, and the only value of it, and that value
            // joined to more text.
            r#"tuple { city: "Paris", country: "France" }"#,
            r#""France""#,
            r#""Kumasi, Ghana""#,
        ]
    );
}

/// The lines `relations.trendy` prints, each result's once, as
/// [`folded_and_run`] gives them.
const RELATIONS: &[&str] = &[
    // Duplicates dropped; text by code point, so `É` after `E`.
    "┌─────────┐",
    "│ name    │",
    "├─────────┤",
    "│ Ada     │",
    "│ Edgar   │",
    "│ Édouard │",
    "└─────────┘",
    "┌──────────┐",
    "│ place    │",
    "├──────────┤",
    "│ London   │",
    "│ Paris    │",
    "│ Portland │",
    "└──────────┘",
    "┌──────┬──────────┐",
    "│ born │ place    │",
    "├──────┼──────────┤",
    "│ 1815 │ London   │",
    "│ 1815 │ Paris    │",
    "│ 1816 │ London   │",
    "│ 1923 │ Portland │",
    "└──────┴──────────┘",
    "┌┐",
    "││",
    "├┤",
    "││",
    "└┘",
    // Approximate numbers numerically, and `-0e0` is `0e0`.
    "┌────────┐",
    "│ x      │",
    "├────────┤",
    "│ -1.5e0 │",
    "│    0e0 │",
    "│  2.5e0 │",
    "└────────┘",
    // Relations by their tuples: the empty one first.
    "┌────────────────────────────────────────────────────────────────┐",
    "│ members                                                        │",
    "├────────────────────────────────────────────────────────────────┤",
    "│ relation { name: text } {}                                     │",
    r#"│ relation { name: text } { { name: "Ada" }, { name: "Edgar" } } │"#,
    "└────────────────────────────────────────────────────────────────┘",
    "┌┐",
    "││",
    "├┤",
    "└┘",
    // Renamed, and sorted again by the new first attribute.
    "┌─────────┬──────┬──────────┐",
    "│ alias   │ born │ place    │",
    "├─────────┼──────┼──────────┤",
    "│ Ada     │ 1815 │ London   │",
    "│ Ada     │ 1816 │ London   │",
    "│ Edgar   │ 1923 │ Portland │",
    "│ Édouard │ 1815 │ Paris    │",
    "└─────────┴──────┴──────────┘",
    "┌────────┬───┐",
    "│ n      │ x │",
    "├────────┼───┤",
    "│ -1.5e0 │ 3 │",
    "│ -1.5e0 │ 5 │",
    "│    0e0 │ 2 │",
    "│    0e0 │ 4 │",
    "│  2.5e0 │ 1 │",
    "└────────┴───┘",
    // Built from run-time values, sorted, and with the tuples that
    // turn out equal made one.
    "┌──────┬──────┐",
    "│ born │ name │",
    "├──────┼──────┤",
    "│  701 │ 李白 │",
    "│ 1815 │ Ada  │",
    "└──────┴──────┘",
    "┌───┐",
    "│ x │",
    "├───┤",
    "│ 1 │",
    "└───┘",
    r#"tuple { members: relation { name: text } { { name: "Ada" }, { name: "李白" } }, who: "core" }"#,
    // Joined on `name`, which only Ada and Edgar have in both, and
    // composed, without it.
    "┌──────┬──────────┬───────┐",
    "│ born │ city     │ name  │",
    "├──────┼──────────┼───────┤",
    "│ 1815 │ London   │ Ada   │",
    "│ 1923 │ Portland │ Edgar │",
    "└──────┴──────────┴───────┘",
    "┌──────┬──────────┐",
    "│ born │ city     │",
    "├──────┼──────────┤",
    "│ 1815 │ London   │",
    "│ 1923 │ Portland │",
    "└──────┴──────────┘",
    "┌──────────┬──────┐",
    "│ city     │ year │",
    "├──────────┼──────┤",
    "│ London   │ 2024 │",
    "│ London   │ 2025 │",
    "│ Nyeri    │ 2024 │",
    "│ Nyeri    │ 2025 │",
    "│ Portland │ 2024 │",
    "│ Portland │ 2025 │",
    "└──────────┴──────┘",
    // Joins and times of lists.
    "┌──────┬──────────┬───────┐",
    "│ born │ city     │ name  │",
    "├──────┼──────────┼───────┤",
    "│ 1815 │ London   │ Ada   │",
    "│ 1923 │ Portland │ Edgar │",
    "└──────┴──────────┴───────┘",
    "┌──────┬──────────┬──────┐",
    "│ born │ city     │ year │",
    "├──────┼──────────┼──────┤",
    "│ 1815 │ London   │ 2024 │",
    "│ 1815 │ London   │ 2025 │",
    "│ 1815 │ Nyeri    │ 2024 │",
    "│ 1815 │ Nyeri    │ 2025 │",
    "│ 1815 │ Portland │ 2024 │",
    "│ 1815 │ Portland │ 2025 │",
    "│ 1923 │ London   │ 2024 │",
    "│ 1923 │ London   │ 2025 │",
    "│ 1923 │ Nyeri    │ 2024 │",
    "│ 1923 │ Nyeri    │ 2025 │",
    "│ 1923 │ Portland │ 2024 │",
    "│ 1923 │ Portland │ 2025 │",
    "└──────┴──────────┴──────┘",
    // Composed from the left: on `name`, then on `city`.
    "┌──────┬────────────────┐",
    "│ born │ country        │",
    "├──────┼────────────────┤",
    "│ 1815 │ United Kingdom │",
    "│ 1923 │ United States  │",
    "└──────┴────────────────┘",
    // Extended, each tuple computing its own; a relation-valued
    // attribute extended in turn, empty or not.
    "┌─────┬──────┬───────────┬───────┐",
    "│ age │ born │ greeting  │ name  │",
    "├─────┼──────┼───────────┼───────┤",
    "│ 101 │ 1923 │ Hi, Edgar │ Edgar │",
    "│ 209 │ 1815 │ Hi, Ada   │ Ada   │",
    "└─────┴──────┴───────────┴───────┘",
    "┌────────────────────────────────────────────────────────────────┬────────────────────────────────────────────────────────────────────────────────────────────────────────────┬──────┐",
    "│ members                                                        │ roster                                                                                                     │ team │",
    "├────────────────────────────────────────────────────────────────┼────────────────────────────────────────────────────────────────────────────────────────────────────────────┼──────┤",
    "│ relation { name: text } {}                                     │ relation { name: text, role: text } {}                                                                     │ docs │",
    r#"│ relation { name: text } { { name: "Ada" }, { name: "Edgar" } } │ relation { name: text, role: text } { { name: "Ada", role: "member" }, { name: "Edgar", role: "member" } } │ core │"#,
    "└────────────────────────────────────────────────────────────────┴────────────────────────────────────────────────────────────────────────────────────────────────────────────┴──────┘",
    // Replaced, each tuple computing its own; tuples that become
    // equal are one.
    "┌──────┬───────┐",
    "│ born │ name  │",
    "├──────┼───────┤",
    "│ 1816 │ Ada   │",
    "│ 1924 │ Edgar │",
    "└──────┴───────┘",
    "┌──────┐",
    "│ born │",
    "├──────┤",
    "│    1 │",
    "└──────┘",
    // Image relations: Nyeri's is empty; and a relation comparison
    // of each.
    "┌──────────┬─────────────────────────────────────────────────────────────────────────────────────────────────────┐",
    "│ city     │ natives                                                                                             │",
    "├──────────┼─────────────────────────────────────────────────────────────────────────────────────────────────────┤",
    r#"│ London   │ relation { born: integer, name: text } { { born: 1815, name: "Ada" }, { born: 1816, name: "Ada" } } │"#,
    "│ Nyeri    │ relation { born: integer, name: text } {}                                                           │",
    r#"│ Portland │ relation { born: integer, name: text } { { born: 1923, name: "Edgar" } }                            │"#,
    "└──────────┴─────────────────────────────────────────────────────────────────────────────────────────────────────┘",
    "┌─────────┬─────────────┐",
    "│ name    │ only_london │",
    "├─────────┼─────────────┤",
    "│ Ada     │ true        │",
    "│ Edgar   │ false       │",
    "│ Édouard │ false       │",
    "└─────────┴─────────────┘",
    // Grouped: one tuple per place; two groupings at once, each of
    // the same births; and every attribute of each team's members,
    // so one tuple, or none for a team without members.
    "┌─────────────────────────────────────────────────────────────────────────────────────────────────────┬──────────┐",
    "│ births                                                                                              │ place    │",
    "├─────────────────────────────────────────────────────────────────────────────────────────────────────┼──────────┤",
    r#"│ relation { born: integer, name: text } { { born: 1815, name: "Ada" }, { born: 1816, name: "Ada" } } │ London   │"#,
    r#"│ relation { born: integer, name: text } { { born: 1815, name: "Édouard" } }                          │ Paris    │"#,
    r#"│ relation { born: integer, name: text } { { born: 1923, name: "Edgar" } }                            │ Portland │"#,
    "└─────────────────────────────────────────────────────────────────────────────────────────────────────┴──────────┘",
    "┌─────────────────────────────────────────────────┬──────────┬───────────────────────────────────────────────────────────────┐",
    "│ names                                           │ place    │ years                                                         │",
    "├─────────────────────────────────────────────────┼──────────┼───────────────────────────────────────────────────────────────┤",
    r#"│ relation { name: text } { { name: "Ada" } }     │ London   │ relation { born: integer } { { born: 1815 }, { born: 1816 } } │"#,
    r#"│ relation { name: text } { { name: "Edgar" } }   │ Portland │ relation { born: integer } { { born: 1923 } }                 │"#,
    r#"│ relation { name: text } { { name: "Édouard" } } │ Paris    │ relation { born: integer } { { born: 1815 } }                 │"#,
    "└─────────────────────────────────────────────────┴──────────┴───────────────────────────────────────────────────────────────┘",
    "┌────────────────────────────────────────────────────────────────┬───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┬──────┐",
    "│ members                                                        │ roster                                                                                                                    │ team │",
    "├────────────────────────────────────────────────────────────────┼───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┼──────┤",
    "│ relation { name: text } {}                                     │ relation { names: relation { name: text } } {}                                                                            │ docs │",
    r#"│ relation { name: text } { { name: "Ada" }, { name: "Edgar" } } │ relation { names: relation { name: text } } { { names: relation { name: text } { { name: "Ada" }, { name: "Edgar" } } } } │ core │"#,
    "└────────────────────────────────────────────────────────────────┴───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┴──────┘",
    // Ungrouped: the docs team, with no members, has no tuples; and
    // grouping then ungrouping gives the births back.
    "┌───────┬──────┐",
    "│ name  │ team │",
    "├───────┼──────┤",
    "│ Ada   │ core │",
    "│ Edgar │ core │",
    "└───────┴──────┘",
    "┌──────┬─────────┬──────────┐",
    "│ born │ name    │ place    │",
    "├──────┼─────────┼──────────┤",
    "│ 1815 │ Ada     │ London   │",
    "│ 1815 │ Édouard │ Paris    │",
    "│ 1816 │ Ada     │ London   │",
    "│ 1923 │ Edgar   │ Portland │",
    "└──────┴─────────┴──────────┘",
    // Wrapped, and unwrapped back.
    "┌──────────┬───────────────────────────────────────┐",
    "│ place    │ who                                   │",
    "├──────────┼───────────────────────────────────────┤",
    r#"│ London   │ tuple { born: 1815, name: "Ada" }     │"#,
    r#"│ London   │ tuple { born: 1816, name: "Ada" }     │"#,
    r#"│ Paris    │ tuple { born: 1815, name: "Édouard" } │"#,
    r#"│ Portland │ tuple { born: 1923, name: "Edgar" }   │"#,
    "└──────────┴───────────────────────────────────────┘",
    "┌──────┬─────────┬──────────┐",
    "│ born │ name    │ place    │",
    "├──────┼─────────┼──────────┤",
    "│ 1815 │ Ada     │ London   │",
    "│ 1815 │ Édouard │ Paris    │",
    "│ 1816 │ Ada     │ London   │",
    "│ 1923 │ Edgar   │ Portland │",
    "└──────┴─────────┴──────────┘",
    // Restricted, by a condition, by two, and of a join.
    "┌──────┬─────────┬────────┐",
    "│ born │ name    │ place  │",
    "├──────┼─────────┼────────┤",
    "│ 1815 │ Ada     │ London │",
    "│ 1815 │ Édouard │ Paris  │",
    "│ 1816 │ Ada     │ London │",
    "└──────┴─────────┴────────┘",
    "┌──────┬─────────┬───────┐",
    "│ born │ name    │ place │",
    "├──────┼─────────┼───────┤",
    "│ 1815 │ Édouard │ Paris │",
    "└──────┴─────────┴───────┘",
    "┌──────┬──────────┬───────┬──────────┐",
    "│ born │ city     │ name  │ place    │",
    "├──────┼──────────┼───────┼──────────┤",
    "│ 1815 │ London   │ Ada   │ London   │",
    "│ 1816 │ London   │ Ada   │ London   │",
    "│ 1923 │ Portland │ Edgar │ Portland │",
    "└──────┴──────────┴───────┴──────────┘",
    // Matching, and not matching without the common attribute.
    "┌──────┬───────┐",
    "│ born │ name  │",
    "├──────┼───────┤",
    "│ 1815 │ Ada   │",
    "│ 1923 │ Edgar │",
    "└──────┴───────┘",
    "┌──────┬─────────┐",
    "│ born │ name    │",
    "├──────┼─────────┤",
    "│ 1815 │ Édouard │",
    "└──────┴─────────┘",
    // Union, intersection, difference, of lists, an exclusive union,
    // and inclusion.
    "┌─────────┐",
    "│ name    │",
    "├─────────┤",
    "│ Ada     │",
    "│ Edgar   │",
    "│ Wangari │",
    "│ Édouard │",
    "└─────────┘",
    "┌───────┐",
    "│ name  │",
    "├───────┤",
    "│ Ada   │",
    "│ Edgar │",
    "└───────┘",
    "┌─────────┐",
    "│ name    │",
    "├─────────┤",
    "│ Édouard │",
    "└─────────┘",
    "┌─────────┐",
    "│ name    │",
    "├─────────┤",
    "│ Ada     │",
    "│ Edgar   │",
    "│ Wangari │",
    "│ Édouard │",
    "└─────────┘",
    "┌─────────┐",
    "│ name    │",
    "├─────────┤",
    "│ Édouard │",
    "└─────────┘",
    "┌─────────┐",
    "│ name    │",
    "├─────────┤",
    "│ Wangari │",
    "│ Édouard │",
    "└─────────┘",
    // Included, as sets: everyone in `people` is in `cities`, but
    // not everyone in `births`.
    "tuple { born: false, listed: true }",
    // A disjoint union, and an included minus.
    "┌─────────┐",
    "│ name    │",
    "├─────────┤",
    "│ Ada     │",
    "│ Edgar   │",
    "│ Wangari │",
    "│ Édouard │",
    "└─────────┘",
    "┌─────────┐",
    "│ name    │",
    "├─────────┤",
    "│ Édouard │",
    "└─────────┘",
    // Aggregates: -1.5 counts twice in the sum of `x`.
    "4",
    "15",
    "-5e-1",
    "1923",
    // 5!, and the rates compounded, exactly, then rounded once.
    "120",
    "1.0804500000000001e0",
    // Per group, Puno with none; and the two born in 1815.
    "┌───────┬─────────┬────────┐",
    "│ crops │ region  │ tonnes │",
    "├───────┼─────────┼────────┤",
    "│     0 │ Puno    │      0 │",
    "│     1 │ Mendoza │    300 │",
    "│     2 │ Nyeri   │    165 │",
    "│     2 │ Punjab  │   1900 │",
    "└───────┴─────────┴────────┘",
    "┌──────┬─────────┬────────┐",
    "│ born │ name    │ place  │",
    "├──────┼─────────┼────────┤",
    "│ 1815 │ Ada     │ London │",
    "│ 1815 │ Édouard │ Paris  │",
    "└──────┴─────────┴────────┘",
];

#[test]
fn relations() {
    assert_eq!(folded_and_run("relations"), RELATIONS);
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
            "┌────────┬────────────────────────────────────┐",
            "│ city   │ rainy                              │",
            "├────────┼────────────────────────────────────┤",
            "│ Cairo  │ array text []                      │",
            r#"│ Mumbai │ array ["Jun", "Jul", "Aug", "Sep"] │"#,
            "└────────┴────────────────────────────────────┘",
            // Element by element, a prefix first.
            "┌──────────────────┐",
            "│ m                │",
            "├──────────────────┤",
            "│ array integer [] │",
            "│ array [1, 9]     │",
            "│ array [2]        │",
            "└──────────────────┘",
            // Positions count from 1.
            "950",
            "2",
            "3",
            // Equal only in the same order.
            "tuple { copied: true, reordered: false }",
            // Generic over the elements' type.
            "1200",
            r#""Amara""#,
            // Loaded: the wettest first, and the names alphabetically.
            r#"array [tuple { city: "Mumbai", rain: 2200 }, tuple { city: "Lagos", rain: 1500 }, tuple { city: "Cairo", rain: 25 }, tuple { city: "Lima", rain: 15 }]"#,
            r#"array ["Cairo", "Lagos", "Lima", "Mumbai"]"#,
            // Elements changed, and swapped, in a copy.
            "tuple { grid: array [array [1, 2], array [30]], kept: array [1200, 950, 220], rainfall: array [950, 1200, 250] }",
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
            "┌────────┬────────────┐",
            "│ people │ recipe     │",
            "├────────┼────────────┤",
            "│      4 │ dal tadka  │",
            "│      4 │ shakshuka  │",
            "│      6 │ mapo doufu │",
            "└────────┴────────────┘",
            // No borshch this week, so no beetroot, cabbage, carrot or
            // potato; each ingredient scaled to the people it's cooked for,
            // summed across recipes, cumin 5 g in each of two, and with the
            // recipes that need it.
            "┌────────────────┬──────────────────────────────────────────────────────────────────────────────────────────────────────────┬───────┬───────┐",
            "│ ingredient     │ recipes                                                                                                  │ total │ unit  │",
            "├────────────────┼──────────────────────────────────────────────────────────────────────────────────────────────────────────┼───────┼───────┤",
            r#"│ Sichuan pepper │ relation { recipe: text } { { recipe: "mapo doufu" } }                                                   │   6e0 │ g     │"#,
            r#"│ cumin          │ relation { recipe: text } { { recipe: "dal tadka" }, { recipe: "shakshuka" } }                           │   1e1 │ g     │"#,
            r#"│ doubanjiang    │ relation { recipe: text } { { recipe: "mapo doufu" } }                                                   │   6e1 │ g     │"#,
            r#"│ egg            │ relation { recipe: text } { { recipe: "shakshuka" } }                                                    │   6e0 │ piece │"#,
            r#"│ garlic         │ relation { recipe: text } { { recipe: "dal tadka" }, { recipe: "mapo doufu" }, { recipe: "shakshuka" } } │ 1.3e1 │ clove │"#,
            r#"│ ghee           │ relation { recipe: text } { { recipe: "dal tadka" } }                                                    │   3e1 │ g     │"#,
            r#"│ ginger         │ relation { recipe: text } { { recipe: "dal tadka" }, { recipe: "mapo doufu" } }                          │   4e1 │ g     │"#,
            r#"│ minced pork    │ relation { recipe: text } { { recipe: "mapo doufu" } }                                                   │   3e2 │ g     │"#,
            r#"│ olive oil      │ relation { recipe: text } { { recipe: "shakshuka" } }                                                    │   3e1 │ ml    │"#,
            r#"│ onion          │ relation { recipe: text } { { recipe: "dal tadka" }, { recipe: "shakshuka" } }                           │   2e0 │ piece │"#,
            r#"│ red lentils    │ relation { recipe: text } { { recipe: "dal tadka" } }                                                    │ 2.5e2 │ g     │"#,
            r#"│ tofu           │ relation { recipe: text } { { recipe: "mapo doufu" } }                                                   │   8e2 │ g     │"#,
            r#"│ tomato         │ relation { recipe: text } { { recipe: "dal tadka" }, { recipe: "shakshuka" } }                           │   1e3 │ g     │"#,
            "└────────────────┴──────────────────────────────────────────────────────────────────────────────────────────────────────────┴───────┴───────┘",
        ]
    );
}

#[test]
fn zebra() {
    let (code, stderr) = run_project("zebra");
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            // The only street the clues allow, then who drinks water, and
            // who owns the zebra.
            "┌────────┬─────────────┬──────────────┬───────┬───────────┬────────┐",
            "│ color  │ dish        │ drink        │ house │ nation    │ pet    │",
            "├────────┼─────────────┼──────────────┼───────┼───────────┼────────┤",
            "│ blue   │ jollof rice │ tea          │     2 │ Ukrainian │ horse  │",
            "│ green  │ shakshuka   │ coffee       │     5 │ Japanese  │ zebra  │",
            "│ ivory  │ dal tadka   │ orange juice │     4 │ Spanish   │ dog    │",
            "│ red    │ borshch     │ milk         │     3 │ English   │ snails │",
            "│ yellow │ mapo doufu  │ water        │     1 │ Norwegian │ fox    │",
            "└────────┴─────────────┴──────────────┴───────┴───────────┴────────┘",
            r#""Norwegian""#,
            r#""Japanese""#,
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
  = note: an operator definition has the form `operator name{} [];`, or, for a read-only operator, `operator name{}: type [value];`
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
