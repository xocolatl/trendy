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

/// Compile `examples/<name>.trendy`, run the executable in the workspace
/// root, from which the examples' paths to files are relative, and return
/// its exit code and what it wrote to stderr.
fn run_example(name: &str) -> (i32, String) {
    run_file(
        name,
        &Path::new("examples").join(format!("{name}.trendy")),
        Path::new(""),
    )
}

/// Compile `projects/<name>/<name>.trendy`, run the executable in the
/// project's directory, from which its paths to files are relative, and
/// return its exit code and what it wrote to stderr.
fn run_project(name: &str) -> (i32, String) {
    let dir = Path::new("projects").join(name);
    run_file(name, &dir.join(format!("{name}.trendy")), &dir)
}

/// Compile `path`, relative to the workspace root, run the executable in
/// `dir`, relative to it too, and return its exit code and what it wrote
/// to stderr.
fn run_file(name: &str, path: &Path, dir: &Path) -> (i32, String) {
    let root = workspace_root();
    // Each run gets its own executable, since tests run in parallel.
    let run = RUNS.fetch_add(1, Ordering::Relaxed);
    let output = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{run}"));
    build_and_run(&root.join(path), &output, &root.join(dir))
}

/// The line a failure writes after its message: where the program is, at
/// the statement on `line` of the source `name` of a test's, indented
/// once.
fn site(name: &str, line: usize) -> String {
    format!(" --> {name}.trendy:{line}:5\n")
}

/// The note a failure writes, after its error, for each call it passed
/// through, innermost first: in the operator `operator`, called at the
/// statement on `line` of the source `name` of a test's, indented once.
fn called(name: &str, operator: &str, line: usize) -> String {
    format!("  = note: in `{operator}`, called at {name}.trendy:{line}:5\n")
}

/// Compile `input` into the executable `output`, run it in `dir`, and
/// return its exit code and what it wrote to stderr, where a failure's
/// site names the source by its file's name alone. The C library
/// scribbles over memory as it is freed (`MallocScribble` on macOS,
/// `MALLOC_PERTURB_` with glibc), so that a program that reads a value
/// whose buffer was freed writes garbage, rather than, by luck, what it
/// should.
fn build_and_run(input: &Path, output: &Path, dir: &Path) -> (i32, String) {
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
        .current_dir(dir)
        .env("MallocScribble", "1")
        .env("MallocPreScribble", "1")
        .env("MALLOC_PERTURB_", "85")
        .output()
        .unwrap();
    let path = input.to_str().unwrap();
    let file = input.file_name().unwrap().to_str().unwrap();
    let stderr = String::from_utf8_lossy(&run.stderr).replace(path, file);
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
            // Half a degree east.
            "position{latitude: -4.25e0, longitude: 4.025e1}",
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

/// What `examples/operators.trendy` writes last, of its operators with
/// `var` and `out` parameters.
const PARAMETERS: [&str; 16] = [
    // `var` parameters: a local relation with a dish added, the
    // program's with another, and a price discounted by 10%, then
    // twice more.
    "┌──────────────┬─────────┬───────┐",
    "│ country      │ dish    │ price │",
    "├──────────────┼─────────┼───────┤",
    "│ Japan        │ ramen   │   900 │",
    "│ South Africa │ bobotie │  1100 │",
    "└──────────────┴─────────┴───────┘",
    r#""dishes""#,
    "4",
    r#""discounted""#,
    "900",
    r#""discounted twice more""#,
    "729",
    // `out` parameters: the cheapest dish and the dearest, with VAT
    // at 25, khachapuri's 1187 between them.
    r#""spread with VAT""#,
    "563",
    r#""cheapest and dearest""#,
    "tuple { high: 1500, low: 937 }",
];

#[test]
fn operators() {
    let (code, stderr) = run_example("operators");
    assert_eq!(code, 0, "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    let (others, parameters) = lines.split_at(lines.len() - PARAMETERS.len());
    assert_eq!(parameters, PARAMETERS);
    assert_eq!(
        others,
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
            // Each `half`, of `vat` at 25, as the arguments' names choose.
            r#""half the VAT""#,
            "tuple { n: 12, x: 1.25e1 }",
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
position{latitude: -1.29e0, longitude: 3.682e1}
tuple { greeting: "\"say \\\"hi\\\"\\n\\ttab \\\\ é\"", nairobi: "position{latitude: -1.29e0, longitude: 3.682e1}", point: "tuple { x: 1e0, y: tuple { z: false } }", rainfall: "array [1200, 950, 220]", teams: "relation { members: relation { name: text }, team: text } { { members: relation { name: text } { { name: \"Ada\" } }, team: \"core\" } }" }
7
"count"
42
"\"say \\\"hi\\\"\\n\\ttab \\\\ é\""
"position{latitude: -1.29e0, longitude: 3.682e1}"
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
    let source = std::fs::read_to_string(workspace_root().join("examples/debug.trendy")).unwrap();
    let (_, stderr) = run_example("debug");
    let lines: Vec<&str> = stderr.lines().collect();
    let text = |line: &str| read_back(&source, line);
    let written = |written: &str| trendy_hir::Value::Text(written.to_string());
    // `greeting`, `nairobi`, `point`, `rainfall` and `teams`, in name
    // order: folded when compiling into `written`, and made when running by
    // `as_text`.
    let debug = [lines[4], lines[12], lines[5], lines[10], lines[9]];
    let trendy_hir::Value::Tuple(folded) = text(lines[13]) else {
        panic!("{}", lines[13]);
    };
    let folded: Vec<_> = folded.attributes().map(|(_, v)| v.clone()).collect();
    let run: Vec<_> = lines[17..22].iter().map(|line| text(line)).collect();
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
    trendy_sema::analyze(&parse(source), &parse(trendy_prelude::SOURCE)).unwrap()
}

/// `written`, a value the program `source` wrote, read back as an
/// expression in that program, which declares the types it can be of.
fn read_back(source: &str, written: &str) -> trendy_hir::Value {
    let program = analyze(&format!("{source}\nvar read_back := {written};\n"));
    let mut variables = program.variables.into_iter();
    variables.find(|v| v.name == "read_back").unwrap().init
}

#[test]
fn debug_output_reads_back_as_the_same_value() {
    // `debug.trendy` prints its variables first, in declaration order.
    let source = std::fs::read_to_string(workspace_root().join("examples/debug.trendy")).unwrap();
    let program = analyze(&source);
    let (_, stderr) = run_example("debug");
    for (variable, line) in program.variables.iter().zip(stderr.lines()) {
        assert_eq!(
            read_back(&source, line),
            variable.init,
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
            // `parsed` when compiling, then `N` and `TRUE` when running.
            "true",
            "false",
            "true",
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
        lines[..5],
        [
            "7",
            "9",
            "-9223372036854775808",
            "9223372036854775807",
            "-42"
        ]
    );
    // Each row: `-a`, `a + b`, `a - b`, `a * b`, `a div b`, `a mod b`.
    let rows: Vec<_> = lines[5..29].chunks(6).collect();
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
    // Parsed when running.
    assert_eq!(lines[29..], ["7", "-9223372036854775808"]);
}

#[test]
fn approximate_numbers() {
    let (code, stderr) = run_example("approximate");
    assert_eq!(code, 0, "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    // Evaluated when compiling, with π, a square root, a cosine and an
    // angle.
    assert_eq!(
        lines[..10],
        [
            "5e-1",
            "1.875e1",
            "0e0",
            "1e-310",
            "9.007199254740992e15",
            "2.5e0",
            "9.42477796076938e0",
            "5e0",
            "-1e0",
            "1.5707963267948966e0"
        ]
    );
    // Each row: `-a`, `a + b`, `a - b`, `a * b`, `a / b`, rounded as IEEE
    // 754 rounds them, except that there is one zero.
    let rows: Vec<_> = lines[10..30].chunks(5).collect();
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
        lines[30..33],
        ["3e0", "9.007199254740992e15", "-9.223372036854776e18"]
    );
    // Parsed when running, with one zero.
    assert_eq!(lines[33..36], ["6.65e3", "1.5e-3", "0e0"]);
    // Each row: the square root of a² + b², the sine and cosine of `a`,
    // and the angle of (a, b), the same on every machine, rounded: π/6 is
    // no approximate number, so its sine isn't one half.
    let rows: Vec<_> = lines[36..].chunks(4).collect();
    assert_eq!(
        rows,
        [
            [
                "5e0",
                "1.411200080598672e-1",
                "-9.899924966004454e-1",
                "9.272952180016122e-1"
            ],
            [
                "1.1287850449966272e0",
                "4.9999999999999994e-1",
                "8.660254037844387e-1",
                "-1.0884484196938717e0"
            ],
            [
                "1e0",
                "-8.414709848078965e-1",
                "5.403023058681398e-1",
                "3.141592653589793e0"
            ],
        ]
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
    // The error writes the operation, with its operands' values.
    let cases = [
        ("add", "a + a", overflow("1e308 + 1e308")),
        ("subtract", "-a - a", overflow("-1e308 - 1e308")),
        ("multiply", "a * 10e0", overflow("1e308 * 1e1")),
        ("divide", "a / 0.1e0", overflow("1e308 / 1e-1")),
        ("divide_zero", "a / b", division("1e308 / 0e0")),
        // There is one zero.
        ("divide_negative_zero", "a / -b", division("1e308 / 0e0")),
        (
            "square_root",
            "sqrt{of: -a}",
            format!("error[{}]: -1e308 has no square root", Code::NoResult.id()),
        ),
    ];
    for (name, expr, failure) in cases {
        let name = format!("approximate_{name}");
        let (code, stderr) = run_source(&name, &program(expr));
        let expected = format!(
            "1e0\n{failure}\n{}{}",
            site(&name, 3),
            called(&name, "f", 7)
        );
        assert_eq!((code, stderr), (1, expected), "{expr}");
    }
}

#[test]
fn text_that_isnt_a_value_of_the_type_stops_the_program() {
    use trendy_diag::Code;
    // `t` is known only when the program runs, so each fails then, after
    // writing 1.
    let program = |operator: &str, text: &str| {
        format!(
            "operator f{{t: text}} [\n    call debug_value{{v: 1}};\n    call debug_value{{v: {operator}{{from: t}}}};\n];\n\noperator main{{}} [\n    call f{{t: {text:?}}};\n];\n"
        )
    };
    let cases = [
        (
            "integer_fraction",
            "parse_integer",
            "4.5",
            "isn't an integer",
        ),
        ("integer_space", "parse_integer", " 42", "isn't an integer"),
        ("integer_tab", "parse_integer", "4\t2", "isn't an integer"),
        (
            "integer_range",
            "parse_integer",
            "9223372036854775808",
            "is out of an integer's range",
        ),
        (
            "approximate_infinity",
            "parse_approximate",
            "inf",
            "isn't an approximate number",
        ),
        (
            "approximate_range",
            "parse_approximate",
            "1e400",
            "is out of an approximate number's range",
        ),
        ("boolean_space", "parse_boolean", "yes ", "isn't a boolean"),
        ("boolean_word", "parse_boolean", "on", "isn't a boolean"),
    ];
    for (name, operator, text, why) in cases {
        let name = format!("parse_{name}");
        let (code, stderr) = run_source(&name, &program(operator, text));
        // Rust's `Debug` writes a text's escapes as trendy does.
        let expected = format!(
            "1\nerror[{}]: {text:?} {why}\n{}{}",
            Code::Unparsable.id(),
            site(&name, 3),
            called(&name, "f", 7)
        );
        assert_eq!((code, stderr), (1, expected), "{text:?}");
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
            // The file's lines, in its order, how many it has, and those
            // that aren't empty.
            r#"array ["Nile", "Amazon", "Yangtze", "", "Paraná", "Mekong", "Volga"]"#,
            "7",
            r#""Nile""#,
            r#""Amazon""#,
            r#""Yangtze""#,
            r#""Paraná""#,
            r#""Mekong""#,
            r#""Volga""#,
            // A file that can't be read, which gives no lines and its
            // error.
            "0",
            r#"relation { error: error } { { error: unreadable_file{path: "examples/lakes.txt", reason: "No such file or directory"} } }"#,
            // A CSV file's records, two skipped, each with its error: a
            // field that isn't a value of its type, and a value that
            // breaks its type's constraint.
            "3",
            r#"relation { message: text } { { message: "\"eighty-four\", in column 3 on line 5 of \"examples/lakes.csv\", isn't an integer, the type of `depth.m`" }, { message: "line 3 of \"examples/lakes.csv\" gives `depth` metres{m: -281}, which isn't a value of `metres`: it breaks the constraint `m > 0`" } }"#,
            // The CSV file's first record after its header, with the
            // position of its mouth read from two columns, and its basin's
            // area read in square kilometres, stored in square metres, then
            // its records as a relation.
            r#"tuple { basin: area{m2: 3.254555e12}, length_km: 6650, mouth_at: position{latitude: 3.146e1, longitude: 3.037e1}, name: "Nile", reaches_ocean: true }"#,
            "┌───────────────────────┬───────────┬─────────────────────────────────────────────────┬─────────┬───────────────┐",
            "│ basin                 │ length_km │ mouth_at                                        │ name    │ reaches_ocean │",
            "├───────────────────────┼───────────┼─────────────────────────────────────────────────┼─────────┼───────────────┤",
            "│ area{m2: 7.95e11}     │      4909 │ position{latitude: 9.7e0, longitude: 1.066e2}   │ Mekong  │ true          │",
            "│ area{m2: 1.36e12}     │      3531 │ position{latitude: 4.58e1, longitude: 4.79e1}   │ Volga   │ false         │",
            "│ area{m2: 1.8085e12}   │      6300 │ position{latitude: 3.14e1, longitude: 1.219e2}  │ Yangtze │ true          │",
            "│ area{m2: 2.582672e12} │      4880 │ position{latitude: -3.4e1, longitude: -5.84e1}  │ Paraná  │ true          │",
            "│ area{m2: 3.254555e12} │      6650 │ position{latitude: 3.146e1, longitude: 3.037e1} │ Nile    │ true          │",
            "│ area{m2: 7.05e12}     │      6400 │ position{latitude: -1.7e-1, longitude: -4.9e1}  │ Amazon  │ true          │",
            "└───────────────────────┴───────────┴─────────────────────────────────────────────────┴─────────┴───────────────┘",
        ]
    );
}

/// The columns of the CSV files the tests load, in another order than
/// their attributes', with others between them.
const CSV_COLUMNS: &str = "city: 3, people: 5, km2: 4, capital: 2";

/// A CSV file the tests load, of cities, after a header, which names the
/// columns otherwise than the attributes: quoted fields, line ends of
/// either kind, an empty line, and a byte order mark.
const CITIES_CSV: &str = "\u{FEFF}country,capital,city,area,population\r\nPeru,yes,Lima,2672.3,10883000\r\n\nVietnam,N,\"Hanoi, \"\"Hà Nội\"\"\",3358.6,8435700\n";

/// What loading `contents` as a CSV file, named for `name`, writes, with
/// or without a header, reading the columns `columns` gives, and the
/// program's exit status: its rows, in the file's order, and how many
/// there are. The columns are a variable's, known only when the program
/// runs.
fn loaded_csv(name: &str, contents: &str, header: bool, columns: &str) -> (i32, String) {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("csv");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.csv"));
    std::fs::write(&path, contents).unwrap();
    let source = format!(
        "operator main{{}} [
    var rows: array tuple {{ city: text, people: integer, km2: approximate, capital: boolean }};
    var columns := tuple {{ {columns} }};
    call load_csv{{path: {:?}, header: {header}, columns: columns, out rows: rows}};
    call debug_value{{v: rows}};
    call debug_value{{v: length{{a: rows}}}};
];
",
        path.to_str().unwrap()
    );
    let (code, stderr) = run_source(&format!("csv_{name}"), &source);
    let path = path.to_str().unwrap().to_string();
    (code, stderr.replace(&path, "PATH"))
}

/// What loading `contents` as a CSV file, named for `name`, writes, and
/// the program's exit status: the file's records, after a header, read
/// into tuples of `heading`, in a program that declares `types`, with the
/// columns `columns` gives, a variable's, known only when the program
/// runs. The file's path is written `PATH`.
fn loaded_rows(
    name: &str,
    types: &str,
    heading: &str,
    contents: &str,
    columns: &str,
) -> (i32, String) {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("csv");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.csv"));
    std::fs::write(&path, contents).unwrap();
    let source = format!(
        "{types}
operator main{{}} [
    var rows: array tuple {heading};
    var columns := {columns};
    call load_csv{{path: {:?}, header: true, columns: columns, out rows: rows}};
    call debug_value{{v: rows}};
];
",
        path.to_str().unwrap()
    );
    let (code, stderr) = run_source(&format!("csv_{name}"), &source);
    let path = path.to_str().unwrap().to_string();
    (code, stderr.replace(&path, "PATH"))
}

/// What loading `contents` as a CSV file of stops at places, named for
/// `name`, writes (see [`loaded_rows`]). A place's position must be on
/// Earth, and a stop's must have a name.
fn loaded_stops(name: &str, contents: &str, columns: &str) -> (i32, String) {
    let types = "type position {
    { latitude: approximate, longitude: approximate },
    constraint latitude >= -90e0 and latitude <= 90e0,
};
type stop { { name: text, at: position }, constraint name <> \"\" };
";
    let heading = "{ day: integer, stop: stop }";
    loaded_rows(name, types, heading, contents, columns)
}

#[test]
fn csv_files_load_values_of_a_programs_own_types() {
    let columns =
        "tuple { day: 1, stop: tuple { name: 2, at: tuple { latitude: 3, longitude: 4 } } }";
    let header = "day,stop,latitude,longitude\n";
    // Each value is selected from its components, each from its column.
    assert_eq!(
        loaded_stops("stops", &format!("{header}1,Lima,-12.05,-77.04\n"), columns),
        (
            0,
            r#"array [tuple { day: 1, stop: stop{at: position{latitude: -1.205e1, longitude: -7.704e1}, name: "Lima"} }]"#
                .to_string()
                + "\n"
        )
    );
    // Components that break a constraint stop the program, writing the
    // record's line, the value, and the constraint: a value's components
    // are checked before it, as they are selected first.
    let broken = trendy_diag::Code::BrokenConstraint.id();
    let site = site("csv_stops_broken", 10);
    assert_eq!(
        loaded_stops(
            "stops_broken",
            &format!("{header}1,Lima,-12.05,-77.04\n2,,95,0\n"),
            columns
        ),
        (
            1,
            format!(
                "error[{broken}]: line 3 of \"PATH\" gives `stop.at` position{{latitude: 9.5e1, longitude: 0e0}}, which isn't a value of `position`: it breaks the constraint `latitude >= -90e0 and latitude <= 90e0`\n{site}"
            )
        )
    );
    let site = self::site("csv_stops_unnamed", 10);
    assert_eq!(
        loaded_stops("stops_unnamed", &format!("{header}2,,9,0\n"), columns),
        (
            1,
            format!(
                "error[{broken}]: line 2 of \"PATH\" gives `stop` stop{{at: position{{latitude: 9e0, longitude: 0e0}}, name: \"\"}}, which isn't a value of `stop`: it breaks the constraint `name <> \"\"`\n{site}"
            )
        )
    );
    // A component's column, and a field that isn't of its type, are
    // named as `t.c` reads it.
    let columns_code = trendy_diag::Code::CsvColumns.id();
    let (code, stderr) = loaded_stops(
        "stops_shared",
        header,
        "tuple { day: 1, stop: tuple { name: 2, at: tuple { latitude: 3, longitude: 3 } } }",
    );
    assert_eq!(
        (code, stderr.lines().next().unwrap()),
        (
            1,
            &*format!(
                "error[{columns_code}]: `stop.at.latitude` and `stop.at.longitude` are both read from column 3"
            )
        )
    );
    let (code, stderr) = loaded_stops(
        "stops_unparsable",
        &format!("{header}1,Lima,south,-77.04\n"),
        columns,
    );
    assert_eq!(
        (code, stderr.lines().next().unwrap()),
        (
            1,
            &*format!(
                "error[{}]: \"south\", in column 3 (\"latitude\") on line 2 of \"PATH\", isn't an approximate number, the type of `stop.at.latitude`",
                trendy_diag::Code::Unparsable.id()
            )
        )
    );
}

/// A union type and its subtypes, as tests declare them.
const FIGURES: &str = "type plane_figure union;
type ellipse is plane_figure {
    { a: approximate, b: approximate },
    constraint a >= b,
    order [asc a, asc b],
};
type polygon is plane_figure { { corners: relation { n: integer, name: text } } };
";

#[test]
fn failures() {
    let (code, stderr) = run_example("failures");
    assert_eq!(
        (code, stderr.as_str()),
        (
            0,
            "1e3\n6.41e2\n\"no conversion from pinch to grams\"\n\"the relation has 2 tuples, but exactly one is taken\"\n"
        )
    );
}

#[test]
fn transactions() {
    let (code, stderr) = run_example("transactions");
    let table = "┌──────────┬──────────┬───────┐
│ city     │ native   │ sacks │
├──────────┼──────────┼───────┤
│ Accra    │ Nkran    │    30 │
│ Cusco    │ Qusqu    │    40 │
│ Dhaka    │ ঢাকা       │    25 │
│ Tashkent │ Toshkent │    40 │
└──────────┴──────────┴───────┘
";
    let collected = |warned: &str, failed: &str| {
        format!(
            "relation {{ failed: boolean, message: text }} {{ {{ failed: false, message: \"{warned}\" }}, {{ failed: true, message: \"{failed}\" }} }}\n"
        )
    };
    let moved = collected("Accra has 30 sacks left", "Tashkent has 10 sacks, not 15");
    let day = collected("Dhaka has 20 sacks left", "Tashkent has 40 sacks, not 100");
    let after = "┌──────────┬──────────┬───────┐
│ city     │ native   │ sacks │
├──────────┼──────────┼───────┤
│ Accra    │ Nkran    │    30 │
│ Cusco    │ Qusqu    │    35 │
│ Dhaka    │ ঢাকা       │    30 │
│ Tashkent │ Toshkent │    40 │
└──────────┴──────────┴───────┘
";
    assert_eq!(
        (code, stderr),
        (
            0,
            format!(
                "{moved}{table}{day}{table}warning[{}]: Cusco has 35 sacks left\n --> transactions.trendy:54:9\n{after}relation {{ failed: boolean, message: text }} {{ {{ failed: true, message: \"division by zero: 40 div 0 has no result\" }} }}\n",
                trendy_diag::Code::Warned.id()
            )
        )
    );
}

/// A failure in a transaction undoes what its block assigned: a program
/// variable, a local variable, and a `var` parameter's variable, at any
/// depth of the calls it makes, and many times over in a loop. Nested, a
/// transaction undoes its own; without `collecting`, its failure goes on
/// to the one around it. A failure of the language's own, as a division
/// by zero, it takes as it takes a `fail`, undoing what came before it.
#[test]
fn transactions_undo_what_a_failure_leaves() {
    let source = "type refused is error { { reason: text } };

operator message{error: refused}: text [
    \"refused: \" || error.reason
];

var log: relation { entry: text } := relation {};

operator adjust{var total: integer, by: integer} [
    total := total + by;
    insert log relation { { entry: \"by \" || to_text{from: by} } };
    if total > 100 then [
        fail refused{reason: \"over 100\"};
    ];
];

operator main{} [
    var total := 50;
    var label := \"start\";
    var found: relation transaction_collection;
    transaction collecting found [
        label := \"changed\";
        call adjust{var total: total, by: 70};
    ];
    call debug_value{v: tuple { total: total, label: label, errors: count{r: found} }};
    var outer: relation transaction_collection;
    transaction collecting outer [
        call adjust{var total: total, by: 10};
        var inner: relation transaction_collection;
        transaction collecting inner [
            call adjust{var total: total, by: 90};
        ];
        call debug_value{v: count{r: inner}};
    ];
    call debug_value{v: tuple { total: total, errors: count{r: outer} }};
    var both: relation transaction_collection;
    transaction collecting both [
        call adjust{var total: total, by: 5};
        transaction [
            call adjust{var total: total, by: 50};
        ];
    ];
    call debug_value{v: tuple { total: total, errors: count{r: both} }};
    var tries := 0;
    while tries < 50 loop [
        var each: relation transaction_collection;
        transaction collecting each [
            call adjust{var total: total, by: 200};
        ];
        tries := tries + count{r: each};
    ];
    call debug_table{r: log};
    var divided: relation transaction_collection;
    transaction collecting divided [
        total := total + 1;
        total := total div (total - total);
    ];
    call debug_value{v: tuple { total: total, error: message{error: value from divided{error}} }};
];
";
    assert_eq!(
        run_source("undone", source),
        (
            0,
            "tuple { errors: 1, label: \"start\", total: 50 }
1
tuple { errors: 0, total: 60 }
tuple { errors: 1, total: 60 }
┌───────┐
│ entry │
├───────┤
│ by 10 │
└───────┘
tuple { error: \"division by zero: 61 div 0 has no result\", total: 60 }
"
            .to_string()
        )
    );
}

/// The language's own failures: an error each, which a transaction
/// takes, a value of the prelude's subtype of `error` for its kind; one
/// nothing takes is written with its kind's code, as is a program's own
/// `fail` of one of those kinds.
#[test]
fn the_languages_failures_are_errors_a_transaction_takes() {
    let source = "var sacks: relation { city: text, n: integer } := relation {
    { city: \"Lagos\", n: 4 },
    { city: \"Hanoi\", n: 0 },
}
key { city };

operator main{} [
    var found: relation transaction_collection;
    var zero := 0;
    var big := 9223372036854775807;
    var shelf := array [3, 1];
    transaction collecting found [
        call debug_value{v: 7 div zero};
    ];
    transaction collecting found [
        call debug_value{v: big + 1};
    ];
    transaction collecting found [
        call debug_value{v: value from sacks{n}};
    ];
    transaction collecting found [
        call debug_value{v: shelf[zero + 3]};
    ];
    transaction collecting found [
        call debug_value{v: sum{r: relation { { n: big }, { n: 1 } }, of: n}};
    ];
    call debug_value{v: (found extend { message := message{error: error} }){message, failed}};
    call debug_value{v: shelf[zero + 5]};
];
";
    let messages = [
        "division by zero: 7 div 0 has no result",
        "overflow: 9223372036854775807 + 1 is out of its type's range",
        "overflow: the sum is out of its type's range",
        "the array has 2 elements, so none at position 3",
        "the relation has 2 tuples, but exactly one is taken",
    ];
    let taken: Vec<String> = messages
        .iter()
        .map(|message| format!("{{ failed: true, message: \"{message}\" }}"))
        .collect();
    assert_eq!(
        run_source("taken", source),
        (
            1,
            format!(
                "relation {{ failed: boolean, message: text }} {{ {} }}\nerror[{}]: the array has 2 elements, so none at position 5\n --> taken.trendy:28:5\n",
                taken.join(", "),
                trendy_diag::Code::IndexOutOfRange.id()
            )
        )
    );
    let source = "operator main{} [\n    fail division_by_zero{operation: \"1 div 0\"};\n];\n";
    assert_eq!(
        run_source("own_kind", source),
        (
            1,
            format!("{}\n --> own_kind.trendy:2:5\n", division("1 div 0"))
        )
    );
}

/// A failure a transaction collects is written nowhere, nor are the calls
/// it passed through: a failure nothing takes, later, writes its own.
#[test]
fn a_collected_failure_leaves_no_trace() {
    let source = "type refused is error { { n: integer } };

operator message{error: refused}: text [
    \"refused \" || to_text{from: error.n}
];

operator refuse{n: integer} [
    fail refused{n: n};
];

operator attempt{n: integer} [
    call refuse{n: n};
];

operator main{} [
    var found: relation transaction_collection;
    transaction collecting found [
        call attempt{n: 1};
    ];
    call debug_value{v: count{r: found}};
    call refuse{n: 2};
];
";
    assert_eq!(
        run_source("collected", source),
        (
            1,
            format!(
                "1\nerror[{}]: refused 2\n{}{}",
                trendy_diag::Code::Failed.id(),
                " --> collected.trendy:8:5\n",
                called("collected", "refuse", 21)
            )
        )
    );
}

/// Warnings, of a program that takes from its stock, and warns when it
/// runs low, in transactions and outside.
const WARNED: &str = "type running_low is error { { city: text, sacks: integer } };

operator message{error: running_low}: text [
    error.city || \" has \" || to_text{from: error.sacks} || \" sacks left\"
];

var stock: relation { city: text, sacks: integer } := relation {
    { city: \"Accra\", sacks: 12 },
    { city: \"Dhaka\", sacks: 3 },
}
key { city };

operator take{from: text, n: integer} [
    update stock { sacks := sacks - n } where city = from;
    var left := sum{r: (stock where city = from){sacks}, of: sacks};
    if left < 0 then [
        fail failure{message: from || \" has too few sacks\"};
    ];
    if left < 10 then [
        warn running_low{city: from, sacks: left};
    ];
];

operator report{found: relation transaction_collection} [
    call debug_value{v: (found extend { message := message{error: error} }){message, failed}};
];

operator main{} [
    warn running_low{city: \"Cusco\", sacks: 1};
    var found: relation transaction_collection;
    transaction collecting found [
        call take{from: \"Accra\", n: 5};
    ];
    call report{found: found};
    var both: relation transaction_collection;
    transaction collecting both [
        call take{from: \"Accra\", n: 2};
        call take{from: \"Dhaka\", n: 5};
    ];
    call report{found: both};
    call debug_value{v: stock};
    var outer: relation transaction_collection;
    transaction collecting outer [
        transaction [
            call take{from: \"Accra\", n: 1};
        ];
        var inner: relation transaction_collection;
        transaction collecting inner [
            call take{from: \"Accra\", n: 1};
            call take{from: \"Dhaka\", n: 9};
        ];
        call report{found: inner};
    ];
    call report{found: outer};
    var many: relation transaction_collection;
    var i := 0;
    while i < 50 loop [
        transaction collecting many [
            call take{from: \"Dhaka\", n: 1};
            call take{from: \"Dhaka\", n: 100};
        ];
        i := i + 1;
    ];
    call report{found: many};
    transaction [
        call take{from: \"Accra\", n: 1};
    ];
    call debug_value{v: stock};
    transaction [
        call take{from: \"Accra\", n: 1};
        call take{from: \"Dhaka\", n: 100};
    ];
];
";

/// A warning goes to the innermost transaction with `collecting` running,
/// at any depth of calls, beside its failure, surviving its rollback, and
/// through transactions without `collecting`, many times over in a loop;
/// with none running, the program writes it, as it warns, and carries on.
#[test]
fn warnings_go_to_the_innermost_collecting_transaction() {
    let collected = |tuples: &[(bool, &str)]| {
        let tuples: Vec<String> = tuples
            .iter()
            .map(|(failed, message)| format!("{{ failed: {failed}, message: \"{message}\" }}"))
            .collect();
        format!(
            "relation {{ failed: boolean, message: text }} {{ {} }}\n",
            tuples.join(", ")
        )
    };
    let warned = |message: &str, line: usize, column: usize| {
        format!(
            "warning[{}]: {message}\n --> warned.trendy:{line}:{column}\n",
            trendy_diag::Code::Warned.id()
        )
    };
    let too_few = "Dhaka has too few sacks";
    let expected = [
        warned("Cusco has 1 sacks left", 29, 5),
        collected(&[(false, "Accra has 7 sacks left")]),
        collected(&[(false, "Accra has 5 sacks left"), (true, too_few)]),
        "relation { city: text, sacks: integer } { { city: \"Accra\", sacks: 7 }, { city: \"Dhaka\", sacks: 3 } }\n".to_string(),
        collected(&[(false, "Accra has 5 sacks left"), (true, too_few)]),
        collected(&[(false, "Accra has 6 sacks left")]),
        collected(&[(false, "Dhaka has 2 sacks left"), (true, too_few)]),
        warned("Accra has 5 sacks left", 20, 9),
        "relation { city: text, sacks: integer } { { city: \"Accra\", sacks: 5 }, { city: \"Dhaka\", sacks: 3 } }\n".to_string(),
        warned("Accra has 4 sacks left", 20, 9),
        format!(
            "error[{}]: {too_few}\n --> warned.trendy:17:9\n  = note: in `take`, called at warned.trendy:71:9\n",
            trendy_diag::Code::Failed.id()
        ),
    ];
    assert_eq!(run_source("warned", WARNED), (1, expected.concat()));
}

#[test]
fn a_fail_stops_the_program_with_its_errors_message() {
    let program = |main: &str| {
        format!(
            "type no_conversion is error {{ {{ unit: text }} }};

operator message{{error: no_conversion}}: text [
    \"no conversion from \" || error.unit
];

operator grams{{unit: text}}: approximate [
    if unit = \"jin\" then 500e0 else fail no_conversion{{unit: unit}}
];

operator weigh{{unit: text}} [
    if unit = \"\" then [
        fail failure{{message: \"no unit\"}};
    ];
    call debug_value{{v: grams{{unit: unit}}}};
];

operator main{{}} [
    call weigh{{unit: \"jin\"}};
    {main}
];
"
        )
    };
    let failed = trendy_diag::Code::Failed.id();
    // A value's, where it is computed.
    assert_eq!(
        run_source("fail_value", &program("call weigh{unit: \"pinch\"};")),
        (
            1,
            format!(
                "5e2\nerror[{failed}]: no conversion from pinch\n{}{}{}",
                site("fail_value", 8),
                called("fail_value", "grams", 15),
                called("fail_value", "weigh", 20)
            )
        )
    );
    // A statement's, where it is.
    assert_eq!(
        run_source("fail_statement", &program("call weigh{unit: \"\"};")),
        (
            1,
            format!(
                "5e2\nerror[{failed}]: no unit\n --> fail_statement.trendy:13:9\n{}",
                called("fail_statement", "weigh", 20)
            )
        )
    );
}

/// A `fail`'s error waits for what takes it, and with nothing to, the
/// program writes its message as it stops: a `message` that fails then
/// has its own failure written instead, once, with its own trace, which
/// starts at the `fail` it writes the message of.
#[test]
fn a_failing_message_writes_its_own_failure() {
    let source = "type grumpy is error { { n: integer } };

operator message{error: grumpy}: text [
    to_text{from: 10 div error.n}
];

operator fails{n: integer} [
    fail grumpy{n: n};
];

operator main{} [
    call debug_value{v: 1};
    call fails{n: 0};
];
";
    assert_eq!(
        run_source("failing_message", source),
        (
            1,
            format!(
                "1\n{}\n --> failing_message.trendy:4:5\n{}",
                division("10 div 0"),
                called("failing_message", "message{error: grumpy}", 8)
            )
        )
    );
}

/// A failure several calls deep, in a loop, makes each function it is in
/// fail in turn, each giving up what its statements and local variables
/// hold, a text and a relation here, and the program stops as it did where
/// it happened, with its error and site, running nothing after, then the
/// calls it passed through, innermost first.
#[test]
fn a_failure_deep_in_calls_stops_the_program() {
    let source = "operator share{total: integer, guests: integer}: integer [
    total div guests
];

operator portion{dish: text, guests: integer} [
    var note := \"sharing \" || dish;
    var served := relation { { dish: dish } };
    call debug_value{v: note};
    call debug_value{v: share{total: 12, guests: guests}};
    call debug_value{v: served};
];

operator serve{dishes: array text, guests: integer} [
    var plates := \"\";
    for dish in dishes loop [
        plates := plates || dish;
        call portion{dish: dish, guests: guests};
    ];
    call debug_value{v: plates};
];

operator main{} [
    call serve{dishes: array [\"injera\", \"pierogi\"], guests: 3};
    call serve{dishes: array [\"plov\", \"khachapuri\"], guests: 0};
    call debug_value{v: \"not reached\"};
];
";
    let served = |dish: &str| {
        format!("\"sharing {dish}\"\n4\nrelation {{ dish: text }} {{ {{ dish: \"{dish}\" }} }}\n")
    };
    assert_eq!(
        run_source("deep_failure", source),
        (
            1,
            format!(
                "{}{}\"injerapierogi\"\n\"sharing plov\"\n{}\n{}{}  = note: in `portion`, called at deep_failure.trendy:17:9\n{}",
                served("injera"),
                served("pierogi"),
                division("12 div 0"),
                site("deep_failure", 2),
                called("deep_failure", "share", 9),
                called("deep_failure", "serve", 24)
            )
        )
    );
}

#[test]
fn a_text_file_gives_its_errors_when_asked() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("errors");
    std::fs::create_dir_all(&dir).unwrap();
    let (good, bad) = (dir.join("good.txt"), dir.join("bad.txt"));
    std::fs::write(&good, "Nile\nVolga\n").unwrap();
    std::fs::write(&bad, b"Nile\nVolga\nAmaz\xF4nas\n").unwrap();
    let source = format!(
        "operator show{{path: text}} [
    var lines: array text;
    var errors: relation {{ error: error }};
    call load_text_file{{path: path, out lines: lines, out errors: errors}};
    call debug_value{{v: lines}};
    call debug_value{{v: errors}};
    if not is_empty{{r: errors}} then [
        call debug_value{{v: message{{error: value from errors}}}};
    ];
];

operator main{{}} [
    call show{{path: {good:?}}};
    call show{{path: {bad:?}}};
];
"
    );
    let (code, stderr) = run_source("text_errors", &source);
    let bad = bad.display();
    let expected = [
        r#"array ["Nile", "Volga"]"#.to_string(),
        "relation { error: error } {}".to_string(),
        "array text []".to_string(),
        format!(
            r#"relation {{ error: error }} {{ {{ error: not_utf8{{line: 3, path: "{bad}"}} }} }}"#
        ),
        format!(r#""line 3 of \"{bad}\" isn't UTF-8 text""#),
    ];
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stderr.lines().collect::<Vec<_>>(), expected);
}

#[test]
fn a_csv_file_gives_its_errors_when_asked() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("errors");
    std::fs::create_dir_all(&dir).unwrap();
    let cities = dir.join("cities.csv");
    // A header, a good record, a field that isn't an integer, a stray
    // quote, too few fields, a good record, an integer out of range, and a
    // quoted field that isn't closed.
    std::fs::write(
        &cities,
        "city,people\nLima,10883000\nCairo,many\nsay \"hi\",1\nHanoi\nNairobi,4397000\nRio,99999999999999999999\n\"Pune,2\n",
    )
    .unwrap();
    let narrow = dir.join("narrow.csv");
    std::fs::write(&narrow, "Lima\n").unwrap();
    let source = format!(
        "heading city := {{ city: text, people: integer }};

operator show{{path: text, header: boolean}} [
    var rows: array tuple city;
    var errors: relation {{ error: error }};
    call load_csv{{
        path: path,
        header: header,
        columns: tuple {{ city: 1, people: 2 }},
        out rows: rows,
        out errors: errors,
    }};
    call debug_value{{v: rows}};
    call debug_value{{v: (errors extend {{ m := message{{error: error}} }}){{m}}}};
];

operator main{{}} [
    call show{{path: {cities:?}, header: true}};
    call show{{path: {narrow:?}, header: false}};
];
"
    );
    let (code, stderr) = run_source("csv_errors", &source);
    assert_eq!(code, 0, "{stderr}");
    let (cities, narrow) = (cities.display(), narrow.display());
    let messages = |messages: &[String]| {
        let tuples: Vec<String> = messages.iter().map(|m| format!("{{ m: {m:?} }}")).collect();
        format!("relation {{ m: text }} {{ {} }}", tuples.join(", "))
    };
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            r#"array [tuple { city: "Lima", people: 10883000 }, tuple { city: "Nairobi", people: 4397000 }]"#.to_string(),
            messages(&[
                format!(r#""99999999999999999999", in column 2 on line 7 of "{cities}", is out of an integer's range, the type of `people`"#),
                format!(r#""many", in column 2 on line 3 of "{cities}", isn't an integer, the type of `people`"#),
                format!(r#"line 4 of "{cities}" has a quote in a field that isn't quoted"#),
                format!(r#"line 5 of "{cities}" has 1 field, but the header has 2"#),
                format!(r#"line 8 of "{cities}" starts a quoted field that isn't closed"#),
            ]),
            "array tuple { city: text, people: integer } []".to_string(),
            messages(&[format!(r#"`people` is read from column 2, but "{narrow}" has 1 column"#)]),
        ]
    );
}

#[test]
fn a_csv_record_whose_value_breaks_its_type_is_skipped_when_asked() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("errors");
    std::fs::create_dir_all(&dir).unwrap();
    let (shapes, spans) = (dir.join("shapes.csv"), dir.join("spans.csv"));
    // A shape of two subtypes, a span whose days share one, and a
    // negative width.
    std::fs::write(&shapes, "w,h\n2,1\n3,3\n-1,2\n").unwrap();
    std::fs::write(&spans, "first,second\nmon,mon\nmon,tue\n").unwrap();
    let source = format!(
        "type shape {{ {{ w: integer, h: integer }}, constraint w > 0 }};
type square is shape {{ constraint w = h }};
type wide is shape {{ constraint w >= h }};

type span {{
    {{ days: relation {{ day: integer, label: text }} key {{ day }} }},
    {{ first: text, second: text }}
        to {{ days := relation {{ {{ day: 1, label: first }}, {{ day: 1, label: second }} }} }}
        from {{ first := min{{r: days, of: label}}, second := max{{r: days, of: label}} }},
}};

operator show{{shapes: text, spans: text}} [
    var shaped: array tuple {{ shape: shape }};
    var errors: relation {{ error: error }};
    call load_csv{{
        path: shapes,
        header: true,
        columns: tuple {{ shape: tuple {{ w: 1, h: 2 }} }},
        out rows: shaped,
        out errors: errors,
    }};
    call debug_value{{v: shaped}};
    call debug_value{{v: (errors extend {{ m := message{{error: error}} }}){{m}}}};
    var spanned: array tuple {{ span: span }};
    call load_csv{{
        path: spans,
        header: true,
        columns: tuple {{ span: tuple {{ first: 1, second: 2 }} }},
        out rows: spanned,
        out errors: errors,
    }};
    call debug_value{{v: length{{a: spanned}}}};
    call debug_value{{v: (errors extend {{ m := message{{error: error}} }}){{m}}}};
];

operator main{{}} [
    call show{{shapes: {shapes:?}, spans: {spans:?}}};
];
"
    );
    let (code, stderr) = run_source("csv_types", &source);
    assert_eq!(code, 0, "{stderr}");
    let (shapes, spans) = (shapes.display(), spans.display());
    let messages = |messages: &[String]| {
        let tuples: Vec<String> = messages.iter().map(|m| format!("{{ m: {m:?} }}")).collect();
        format!("relation {{ m: text }} {{ {} }}", tuples.join(", "))
    };
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            "array [tuple { shape: shape{h: 1, w: 2} }]".to_string(),
            messages(&[
                format!(
                    "line 3 of \"{shapes}\" gives `shape` shape{{h: 3, w: 3}}, which is of both `square` and `wide`, but a value has one most specific type"
                ),
                format!(
                    "line 4 of \"{shapes}\" gives `shape` shape{{h: 2, w: -1}}, which isn't a value of `shape`: it breaks the constraint `w > 0`"
                ),
            ]),
            "1".to_string(),
            messages(&[format!(
                r#"line 3 of "{spans}" gives `span` a value of `span` in which two tuples of its `days` have the same values for its key {{ day }}: tuple {{ day: 1, label: "mon" }} and tuple {{ day: 1, label: "tue" }}"#
            )]),
        ]
    );
}

#[test]
fn written_text_cant_change_the_terminal() {
    // An escape sequence that clears the screen, and an override that
    // shows what follows it backwards, in a text known only when running.
    let source = "operator show{text: text} [
    call debug_value{v: text};
    call debug_table{r: relation { { text: text } }};
    fail failure{message: \"bad: \" || text};
];

operator main{} [
    call show{text: \"\\u{1B}[2J\\u{202E}x\"};
];
";
    let failed = trendy_diag::Code::Failed.id();
    let (code, stderr) = run_source("escaped", source);
    assert_eq!(code, 1, "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines[0], r#""\u{1B}[2J\u{202E}x""#);
    assert!(
        lines.iter().any(|l| l.contains(r"│ \u{1B}[2J\u{202E}x")),
        "{stderr}"
    );
    assert_eq!(
        lines[lines.len() - 3..],
        [
            format!(r"error[{failed}]: bad: \u{{1B}}[2J\u{{202E}}x"),
            " --> escaped.trendy:4:5".to_string(),
            "  = note: in `show`, called at escaped.trendy:8:5".to_string(),
        ]
    );
    assert!(!stderr.contains('\u{1B}') && !stderr.contains('\u{202E}'));
}

#[test]
fn types_in_a_hierarchy_when_running() {
    // Each value is laid out after its own type, which it is written as,
    // compared by, and ordered by in a relation, by its components; and
    // its components are read and changed, all when running.
    let source = format!(
        "{FIGURES}
operator shapes{{a: approximate, name: text}} [
    var e := ellipse{{a: a, b: 1e0}};
    call debug_value{{v: e}};
    call debug_value{{v: e.a}};
    call debug_value{{v: e = ellipse{{a: 3e0, b: 1e0}}}};
    call debug_value{{v: e < ellipse{{a: 3e0, b: 2e0}}}};
    var p := polygon{{corners: relation {{ {{ n: 1, name: name }} }}}};
    call debug_value{{v: p}};
    call debug_value{{v: relation {{ {{ e: e }}, {{ e: ellipse{{a: 2e0, b: 2e0}} }} }}}};
    update e {{ b := 2e0 }};
    call debug_value{{v: e}};
    var broken := ellipse{{a: 1e0, b: a}};
];

operator main{{}} [
    call shapes{{a: 3e0, name: \"east\"}};
];
"
    );
    let (code, stderr) = run_source("hierarchy", &source);
    let broken = trendy_diag::Code::BrokenConstraint.id();
    assert_eq!(
        (code, stderr),
        (
            1,
            format!(
                "ellipse{{a: 3e0, b: 1e0}}
3e0
true
true
polygon{{corners: relation {{ n: integer, name: text }} {{ {{ n: 1, name: \"east\" }} }}}}
relation {{ e: ellipse }} {{ {{ e: ellipse{{a: 2e0, b: 2e0}} }}, {{ e: ellipse{{a: 3e0, b: 1e0}} }} }}
ellipse{{a: 3e0, b: 2e0}}
error[{broken}]: ellipse{{a: 1e0, b: 3e0}} isn't a value of `ellipse`: it breaks the constraint `a >= b`
{}{}{}",
                site("hierarchy", 20),
                called("hierarchy", "ellipse", 20),
                called("hierarchy", "shapes", 24)
            )
        )
    );
}

#[test]
fn a_subtypes_value_is_its_union_types_when_running() {
    // Values of either subtype where the union type's are expected:
    // compared by their own types and components, and in arrays and
    // relations of it, ordered by their types, then components.
    let source = format!(
        "{FIGURES}
operator same{{f: plane_figure, g: plane_figure}}: boolean [ f = g ];

operator shapes{{a: approximate}} [
    var e := ellipse{{a: a, b: 1e0}};
    var f: plane_figure := e;
    var p: plane_figure := polygon{{corners: relation {{ {{ n: 1, name: \"n\" }} }}}};
    call debug_value{{v: same{{f: f, g: e}}}};
    call debug_value{{v: same{{f: f, g: p}}}};
    call debug_value{{v: f = e}};
    call debug_value{{v: array [p, f]}};
    call debug_value{{v: relation {{ {{ s: p }}, {{ s: e }} }}}};
];

operator main{{}} [
    call shapes{{a: 3e0}};
];
"
    );
    let (code, stderr) = run_source("substituted", &source);
    assert_eq!(
        (code, stderr.lines().collect::<Vec<_>>()),
        (
            0,
            vec![
                "true",
                "false",
                "true",
                r#"array [polygon{corners: relation { n: integer, name: text } { { n: 1, name: "n" } }}, ellipse{a: 3e0, b: 1e0}]"#,
                r#"relation { s: plane_figure } { { s: ellipse{a: 3e0, b: 1e0} }, { s: polygon{corners: relation { n: integer, name: text } { { n: 1, name: "n" } }} } }"#,
            ]
        )
    );
}

#[test]
fn types_are_tested_and_treated_when_running() {
    // A test tells the subtypes apart, narrows the name in the rest of the
    // condition, in any order, and in a `where`'s result; a treat fails
    // for a value of another type.
    let source = format!(
        "{FIGURES}
operator describe{{f: plane_figure}}: text [
    if f is ellipse then \"ellipse \" || to_text{{from: f.a}} else \"polygon\"
];

operator shapes{{a: approximate}} [
    var e: plane_figure := ellipse{{a: a, b: 1e0}};
    var p: plane_figure := polygon{{corners: relation {{ {{ n: 1, name: \"n\" }} }}}};
    call debug_value{{v: e is ellipse}};
    call debug_value{{v: p is ellipse}};
    call debug_value{{v: describe{{f: e}}}};
    call debug_value{{v: describe{{f: p}}}};
    call debug_value{{v: e.a > 2e0 and e is ellipse}};
    var both := relation {{ {{ s: e }}, {{ s: p }} }};
    call debug_value{{v: both where s is ellipse and s.a > 2e0}};
    call debug_value{{v: (treat e as ellipse).b}};
    call debug_value{{v: treat p as ellipse}};
];

operator main{{}} [
    call shapes{{a: 3e0}};
];
"
    );
    let (code, stderr) = run_source("tested", &source);
    let not_of = trendy_diag::Code::NotOfType.id();
    assert_eq!(
        (code, stderr),
        (
            1,
            format!(
                "true
false
\"ellipse 3e0\"
\"polygon\"
true
relation {{ s: ellipse }} {{ {{ s: ellipse{{a: 3e0, b: 1e0}} }} }}
1e0
error[{not_of}]: polygon{{corners: relation {{ n: integer, name: text }} {{ {{ n: 1, name: \"n\" }} }}}} isn't a value of `ellipse`
{}{}",
                site("tested", 24),
                called("tested", "shapes", 28)
            )
        )
    );
}

#[test]
fn statements_narrow_when_running() {
    // An `if` statement's branches and a `while` loop's block read a
    // tested name's components, and an `update` changes them as the type
    // the test shows.
    let source = format!(
        "{FIGURES}
operator widen{{var f: plane_figure}} [
    if f is ellipse then [
        call debug_value{{v: f.a}};
        update f {{ a := 2e0 * a }};
        call debug_value{{v: f.a}};
    ] else if f is polygon then [
        call debug_value{{v: count{{r: f.corners}}}};
    ];
    var n := 0;
    while f is ellipse and n < 2 loop [
        call debug_value{{v: f.b + to_approximate{{from: n}}}};
        n := n + 1;
    ];
];

operator main{{}} [
    var e: plane_figure := ellipse{{a: 3e0, b: 1e0}};
    call widen{{var f: e}};
    call debug_value{{v: e}};
    var p: plane_figure := polygon{{corners: relation {{ {{ n: 1, name: \"n\" }} }}}};
    call widen{{var f: p}};
];
"
    );
    let (code, stderr) = run_source("narrowed_statements", &source);
    assert_eq!(
        (code, stderr.lines().collect::<Vec<_>>()),
        (
            0,
            vec!["3e0", "6e0", "1e0", "2e0", "ellipse{a: 6e0, b: 1e0}", "1"]
        )
    );
}

#[test]
fn subtypes_defined_by_constraints_when_running() {
    // Selecting an ellipse gives a circle if its semiaxes are equal, an
    // update can make it just an ellipse, versions and tests see which, and
    // a value of two subtypes stops the program.
    let types = "type ellipse { { a: approximate, b: approximate }, constraint a >= b };
type circle is ellipse { constraint a = b };
type small is ellipse { constraint a < 1e0 };
";
    let source = format!(
        "{types}
operator kind{{e: ellipse}}: text [ \"ellipse\" ];
operator kind{{e: circle}}: text [ \"circle\" ];

operator shapes{{a: approximate}} [
    var e := ellipse{{a: a, b: a}};
    call debug_value{{v: tuple {{ circle: e is circle, kind: kind{{e: e}} }}}};
    call debug_value{{v: e}};
    update e {{ a := 2e0 * a }};
    call debug_value{{v: tuple {{ circle: e is circle, kind: kind{{e: e}} }}}};
    var dot := ellipse{{a: a / 4e0, b: a / 4e0}};
];

operator main{{}} [
    call shapes{{a: 2e0}};
];
"
    );
    let (code, stderr) = run_source("constrained", &source);
    let overlapping = trendy_diag::Code::OverlappingSubtypes.id();
    assert_eq!(
        (code, stderr),
        (
            1,
            format!(
                "tuple {{ circle: true, kind: \"circle\" }}
ellipse{{a: 2e0, b: 2e0}}
tuple {{ circle: false, kind: \"ellipse\" }}
error[{overlapping}]: ellipse{{a: 5e-1, b: 5e-1}} is both a `circle` and a `small`, but a value has one most specific type
{}{}{}",
                site("constrained", 14),
                called("constrained", "ellipse", 14),
                called("constrained", "shapes", 18)
            )
        )
    );
    // Read from a file, a value has its most specific type too.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("csv");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("constrained.csv");
    std::fs::write(&path, "name,a,b\nwell,2,2\npond,3,2\n").unwrap();
    let source = format!(
        "{types}
operator main{{}} [
    var rows: array tuple {{ name: text, e: ellipse }};
    var columns := tuple {{ name: 1, e: tuple {{ a: 2, b: 3 }} }};
    call load_csv{{path: {:?}, header: true, columns: columns, out rows: rows}};
    call debug_value{{v: array [rows[1].e is circle, rows[2].e is circle]}};
];
",
        path.to_str().unwrap()
    );
    let (code, stderr) = run_source("constrained_csv", &source);
    assert_eq!((code, stderr.as_str()), (0, "array [true, false]\n"));
}

#[test]
fn a_subtypes_own_representation_when_running() {
    // Selected and updated by its radius; ordered by its type, as when
    // compiling, so that a relation built when running equals one folded;
    // and a circle variable's update that would make its value just an
    // ellipse names the circle.
    let source = "type ellipse { { a: approximate, b: approximate }, constraint a >= b };
type circle is ellipse {
    constraint a = b,
    { r: approximate } to { a := r, b := r } from { r := a },
};
var folded := relation { { e: ellipse{a: 5e0, b: 5e0} }, { e: ellipse{a: 3e0, b: 2e0} } };

operator run{x: approximate} [
    var c := circle{r: x};
    call debug_value{v: c.r};
    update c { r := 2e0 * x };
    call debug_value{v: c.a};
    var built := relation { { e: ellipse{a: 5e0, b: x} }, { e: ellipse{a: 3e0, b: 2e0} } };
    call debug_value{v: tuple { equal: built = folded, size: count{r: built union folded} }};
    update c { a := 3e0 * x };
];

operator main{} [
    call run{x: 5e0};
];
";
    let (code, stderr) = run_source("own_representation", source);
    let broken = trendy_diag::Code::BrokenConstraint.id();
    assert_eq!(
        (code, stderr),
        (
            1,
            format!(
                "5e0
1e1
tuple {{ equal: true, size: 2 }}
error[{broken}]: ellipse{{a: 1.5e1, b: 1e1}} isn't a value of `circle`: it breaks the constraint `a = b`
{}{}",
                site("own_representation", 15),
                called("own_representation", "run", 19)
            )
        )
    );
}

#[test]
fn versions_are_chosen_when_running() {
    // Each call computes the version for its arguments' own types: of a
    // specification without a body, of one with a body for the rest, and
    // of an update operator, and in an `extend`, for each tuple.
    let source = format!(
        "{FIGURES}
operator sides{{f: plane_figure}}: integer;
operator sides{{f: ellipse}}: integer [ 0 ];
operator sides{{f: polygon}}: integer [ count{{r: f.corners}} ];

operator name{{f: plane_figure}}: text [ \"a figure\" ];
operator name{{f: ellipse}}: text [ \"an ellipse\" ];

operator show{{f: plane_figure}};
operator show{{f: ellipse}} [ call debug_value{{v: f.a}}; ];
operator show{{f: polygon}} [ call debug_value{{v: f.corners}}; ];

operator shapes{{a: approximate}} [
    var e: plane_figure := ellipse{{a: a, b: 1e0}};
    var p: plane_figure := polygon{{corners: relation {{ {{ n: 1, name: \"n\" }} }}}};
    call debug_value{{v: tuple {{ e: sides{{f: e}}, p: sides{{f: p}} }}}};
    call debug_value{{v: tuple {{ e: name{{f: e}}, p: name{{f: p}} }}}};
    call show{{f: e}};
    call show{{f: p}};
    call debug_value{{v: relation {{ {{ s: e }}, {{ s: p }} }} extend {{ n := sides{{f: s}} }} {{n}}}};
];

operator main{{}} [
    call shapes{{a: 3e0}};
];
"
    );
    let (code, stderr) = run_source("versioned", &source);
    assert_eq!(
        (code, stderr.lines().collect::<Vec<_>>()),
        (
            0,
            vec![
                "tuple { e: 0, p: 1 }",
                r#"tuple { e: "an ellipse", p: "a figure" }"#,
                "3e0",
                r#"relation { n: integer, name: text } { { n: 1, name: "n" } }"#,
                "relation { n: integer } { { n: 0 }, { n: 1 } }",
            ]
        )
    );
}

#[test]
fn relations_of_a_hierarchy_combine_when_running() {
    // Relations whose attributes are of different types in the hierarchy
    // combine (IM Prescription 13): a union's attribute is a plane
    // figure's, an intersection's and a join's an ellipse's, which can be
    // read, and a difference's and a semijoin's the left operand's.
    let source = format!(
        "{FIGURES}
operator combined{{a: approximate}} [
    var e: plane_figure := ellipse{{a: a, b: 1e0}};
    var p: plane_figure := polygon{{corners: relation {{ {{ n: 1, name: \"n\" }} }}}};
    var figures := relation {{ {{ f: e, k: 1 }}, {{ f: p, k: 2 }} }};
    var ellipses := relation {{ {{ f: treat e as ellipse, k: 1 }} }};
    call debug_value{{v: count{{r: ellipses union figures}}}};
    call debug_value{{v: (figures intersect ellipses) extend {{ a := f.a }} {{a}}}};
    call debug_value{{v: (figures join ellipses{{f}}) extend {{ a := f.a }} {{k, a}}}};
    call debug_value{{v: (figures minus ellipses){{k}}}};
    call debug_value{{v: (ellipses minus figures){{k}}}};
    call debug_value{{v: (figures not matching ellipses){{k}}}};
    var left := ellipses;
    i_delete left figures matching ellipses;
    call debug_value{{v: left}};
];

operator main{{}} [
    call combined{{a: 3e0}};
];
"
    );
    let (code, stderr) = run_source("combined", &source);
    assert_eq!(
        (code, stderr.lines().collect::<Vec<_>>()),
        (
            0,
            vec![
                "2",
                "relation { a: approximate } { { a: 3e0 } }",
                "relation { a: approximate, k: integer } { { a: 3e0, k: 1 } }",
                "relation { k: integer } { { k: 2 } }",
                "relation { k: integer } {}",
                "relation { k: integer } { { k: 2 } }",
                "relation { f: ellipse, k: integer } {}",
            ]
        )
    );
}

#[test]
fn components_keys_are_checked_when_running() {
    // A selected value's relation-valued components satisfy their keys, of
    // the stored representation and of another, whose conversion computes
    // it, and so does a relation variable's value.
    let types = "type outline {
    { vertices: relation { v_no: integer, x: integer } key { v_no } key { x } },
};
type seg {
    { len: integer },
    { ends: relation { n: integer, x: integer } key { x } }
        to { len := max{r: ends, of: x} - min{r: ends, of: x} }
        from { ends := relation { { n: 1, x: 0 }, { n: 2, x: len } } },
};
";
    let source = format!(
        "{types}
operator run{{n: integer}} [
    var ok := outline{{vertices: relation {{ {{ v_no: 1, x: 0 }}, {{ v_no: 2, x: n }} }}}};
    call debug_value{{v: count{{r: ok.vertices}}}};
    call debug_value{{v: seg{{len: n}}.ends{{x}}}};
    var r := relation {{ {{ a: 1, b: 2 }} }} key {{ a }};
    insert r relation {{ {{ a: 2, b: n }} }};
    call debug_value{{v: count{{r: r}}}};
    var flat := seg{{len: n - 3}};
];

operator main{{}} [
    call run{{n: 3}};
];
"
    );
    let (code, stderr) = run_source("component_keys", &source);
    let violated = trendy_diag::Code::KeyViolated.id();
    assert_eq!(
        (code, stderr),
        (
            1,
            format!(
                "2
relation {{ x: integer }} {{ {{ x: 0 }}, {{ x: 3 }} }}
2
error[{violated}]: two tuples of a `seg`'s `ends` have the same values for its key {{ x }}: tuple {{ n: 1, x: 0 }} and tuple {{ n: 2, x: 0 }}
{}{}{}",
                site("component_keys", 18),
                called("component_keys", "seg{len: integer}", 18),
                called("component_keys", "run", 22)
            )
        )
    );
    let source = format!(
        "{types}
operator run{{n: integer}} [
    var bad := outline{{vertices: relation {{ {{ v_no: 1, x: 0 }}, {{ v_no: n, x: 5 }} }}}};
];

operator main{{}} [
    call run{{n: 1}};
];
"
    );
    let (code, stderr) = run_source("component_keys_stored", &source);
    assert_eq!(
        (code, stderr),
        (
            1,
            format!(
                "error[{violated}]: two tuples of an `outline`'s `vertices` have the same values for its key {{ v_no }}: tuple {{ v_no: 1, x: 0 }} and tuple {{ v_no: 1, x: 5 }}
{}{}{}",
                site("component_keys_stored", 12),
                called("component_keys_stored", "outline", 12),
                called("component_keys_stored", "run", 16)
            )
        )
    );
    // A value read from a file, which names the record.
    let (code, stderr) = loaded_rows(
        "segments",
        types,
        "{ name: text, s: seg }",
        "name,len\nlong,4\nflat,0\n",
        "tuple { name: 1, s: tuple { len: 2 } }",
    );
    assert_eq!(
        (code, stderr),
        (
            1,
            format!(
                "error[{violated}]: line 3 of \"PATH\" gives `s` a value of `seg` in which two tuples of its `ends` have the same values for its key {{ x }}: tuple {{ n: 1, x: 0 }} and tuple {{ n: 2, x: 0 }}
{}",
                site("csv_segments", 14)
            )
        )
    );
}

#[test]
fn csv_files_load_values_of_types_in_a_hierarchy() {
    // A subtype's value is read as any type's, and laid out after its own
    // type.
    let types = "type plane_figure union;
type ellipse is plane_figure { { a: approximate, b: approximate }, constraint a >= b };
type polygon is plane_figure { { corners: integer } };
";
    let heading = "{ name: text, lake: ellipse }";
    let columns = "tuple { name: 1, lake: tuple { a: 2, b: 3 } }";
    let header = "name,a,b\n";
    assert_eq!(
        loaded_rows(
            "lakes",
            types,
            heading,
            &format!("{header}Victoria,3,2\nTiticaca,5,1\n"),
            columns
        ),
        (
            0,
            r#"array [tuple { lake: ellipse{a: 3e0, b: 2e0}, name: "Victoria" }, tuple { lake: ellipse{a: 5e0, b: 1e0}, name: "Titicaca" }]"#
                .to_string()
                + "\n"
        )
    );
    let broken = trendy_diag::Code::BrokenConstraint.id();
    let site = site("csv_lakes_broken", 8);
    assert_eq!(
        loaded_rows(
            "lakes_broken",
            types,
            heading,
            &format!("{header}Victoria,1,2\n"),
            columns
        ),
        (
            1,
            format!(
                "error[{broken}]: line 2 of \"PATH\" gives `lake` ellipse{{a: 1e0, b: 2e0}}, which isn't a value of `ellipse`: it breaks the constraint `a >= b`\n{site}"
            )
        )
    );
}

#[test]
fn csv_files_load_values_in_another_representation() {
    // A point by its polar coordinates, its distance from the origin in
    // kilometres, stored in metres.
    let types = "type distance {
    { m: approximate },
    { km: approximate } to { m := km * 1e3 } from { km := m / 1e3 },
    constraint m >= 0e0,
};
type point {
    { x: approximate, y: approximate },
    { r: distance, theta: approximate }
        to { x := r.m * cos{of: theta}, y := r.m * sin{of: theta} }
        from { r := distance{m: sqrt{of: x * x + y * y}}, theta := atan2{y: y, x: x} },
    constraint x * x + y * y <= 1e8,
};
";
    let heading = "{ id: integer, at: point }";
    let columns = "tuple { id: 1, at: tuple { r: tuple { km: 2 }, theta: 3 } }";
    let header = "id,km,theta\n";
    let loaded = |name: &str, records: &str| {
        let contents = format!("{header}{records}");
        loaded_rows(name, types, heading, &contents, columns)
    };
    // Each converted to the stored representation, inner values first.
    assert_eq!(
        loaded("points", "1,2,0\n2,0.5,0\n"),
        (
            0,
            "array [tuple { at: point{x: 2e3, y: 0e0}, id: 1 }, tuple { at: point{x: 5e2, y: 0e0}, id: 2 }]\n"
                .to_string()
        )
    );
    // Each value checked against its type's constraints once converted,
    // inner values first, writing the record's line and the value's name
    // as the columns give it.
    let broken = trendy_diag::Code::BrokenConstraint.id();
    let site = site("csv_points_negative", 17);
    assert_eq!(
        loaded("points_negative", "1,2,0\n2,-1,0\n"),
        (
            1,
            format!(
                "error[{broken}]: line 3 of \"PATH\" gives `at.r` distance{{m: -1e3}}, which isn't a value of `distance`: it breaks the constraint `m >= 0e0`\n{site}"
            )
        )
    );
    let site = self::site("csv_points_far", 17);
    assert_eq!(
        loaded("points_far", "1,20,0\n"),
        (
            1,
            format!(
                "error[{broken}]: line 2 of \"PATH\" gives `at` point{{x: 2e4, y: 0e0}}, which isn't a value of `point`: it breaks the constraint `x * x + y * y <= 1e8`\n{site}"
            )
        )
    );
    // A field is named as the columns name it.
    let (code, stderr) = loaded("points_unparsable", "1,far,0\n");
    assert_eq!(
        (code, stderr.lines().next().unwrap()),
        (
            1,
            &*format!(
                "error[{}]: \"far\", in column 2 (\"km\") on line 2 of \"PATH\", isn't an approximate number, the type of `at.r.km`",
                trendy_diag::Code::Unparsable.id()
            )
        )
    );
}

#[test]
fn csv_files_are_loaded_when_running() {
    let ok = |rows: &str, count: usize| (0, format!("{rows}\n{count}\n"));
    // Columns in any order, those `columns` doesn't name skipped, the
    // header too, whatever it calls them, quoted fields, line ends of
    // either kind, and empty lines.
    let both = r#"array [tuple { capital: true, city: "Lima", km2: 2.6723e3, people: 10883000 }, tuple { capital: false, city: "Hanoi, \"Hà Nội\"", km2: 3.3586e3, people: 8435700 }]"#;
    assert_eq!(
        loaded_csv("cities", CITIES_CSV, true, CSV_COLUMNS),
        ok(both, 2)
    );
    // Without a header, every record is one.
    let headless = CITIES_CSV.split_once('\n').unwrap().1;
    assert_eq!(
        loaded_csv("headless", headless, false, CSV_COLUMNS),
        ok(both, 2)
    );
    // A header alone gives none, as does nothing at all.
    let none = "array tuple { capital: boolean, city: text, km2: approximate, people: integer } []";
    assert_eq!(
        loaded_csv(
            "header",
            "country,capital,city,area,population",
            true,
            CSV_COLUMNS
        ),
        ok(none, 0)
    );
    assert_eq!(loaded_csv("nothing", "", true, CSV_COLUMNS), ok(none, 0));
}

/// What loading the CSV file named for `name` writes, and the program's
/// exit status, if it stops with the error `code`, writing `message`, at
/// the call of `load_csv`.
fn csv_failed(name: &str, code: trendy_diag::Code, message: &str) -> (i32, String) {
    let at = site(&format!("csv_{name}"), 4);
    (1, format!("error[{}]: {message}\n{at}", code.id()))
}

#[test]
fn csv_columns_that_arent_the_files_stop_the_program() {
    use trendy_diag::Code;
    // Each attribute has a column of the file's, of its own, counting
    // from 1.
    assert_eq!(
        loaded_csv(
            "beyond",
            CITIES_CSV,
            true,
            "city: 3, people: 6, km2: 4, capital: 2"
        ),
        csv_failed(
            "beyond",
            Code::CsvColumns,
            r#"`people` is read from column 6, but "PATH" has 5 columns"#
        )
    );
    assert_eq!(
        loaded_csv(
            "zero",
            CITIES_CSV,
            true,
            "city: 0, people: 5, km2: 4, capital: 2"
        ),
        csv_failed(
            "zero",
            Code::CsvColumns,
            "`city` is read from column 0, but columns count from 1"
        )
    );
    assert_eq!(
        loaded_csv(
            "shared",
            CITIES_CSV,
            true,
            "city: 3, people: 5, km2: 4, capital: 3"
        ),
        csv_failed(
            "shared",
            Code::CsvColumns,
            "`capital` and `city` are both read from column 3"
        )
    );
}

#[test]
fn csv_files_that_cant_be_read_stop_the_program() {
    use trendy_diag::Code;
    // Each record has the first's fields, and the file is CSV.
    assert_eq!(
        loaded_csv(
            "short",
            "a,b,c,d,e\nPeru,yes,Lima,2672.3\n",
            true,
            CSV_COLUMNS
        ),
        csv_failed(
            "short",
            Code::MalformedCsv,
            r#"line 2 of "PATH" has 4 fields, but the header has 5"#
        )
    );
    assert_eq!(
        loaded_csv(
            "long",
            "Peru,yes,Lima,2672.3,1\n\nPeru,yes,Lima,2672.3,1,1\n",
            false,
            CSV_COLUMNS
        ),
        csv_failed(
            "long",
            Code::MalformedCsv,
            r#"line 3 of "PATH" has 6 fields, but line 1 has 5"#
        )
    );
    assert_eq!(
        loaded_csv("stray", "Peru,yes,Lima \"Peru\",1,1\n", false, CSV_COLUMNS),
        csv_failed(
            "stray",
            Code::MalformedCsv,
            r#"line 1 of "PATH" has a quote in a field that isn't quoted"#
        )
    );
    assert_eq!(
        loaded_csv(
            "unclosed",
            "a,b,c,d,e\n\n\"Peru,yes,Lima,1,1\n",
            true,
            CSV_COLUMNS
        ),
        csv_failed(
            "unclosed",
            Code::MalformedCsv,
            r#"line 3 of "PATH" starts a quoted field that isn't closed"#
        )
    );
    // Each field is a value of its attribute's type, without spaces, in a
    // column the header names, if there is one.
    assert_eq!(
        loaded_csv(
            "spaced",
            "a,b,c,d,population\nPeru,yes,Lima,2672.3, 10883000\n",
            true,
            CSV_COLUMNS
        ),
        csv_failed(
            "spaced",
            Code::Unparsable,
            r#"" 10883000", in column 5 ("population") on line 2 of "PATH", isn't an integer, the type of `people`"#
        )
    );
    assert_eq!(
        loaded_csv(
            "word",
            "Peru,capital,Lima,2672.3,10883000\n",
            false,
            CSV_COLUMNS
        ),
        csv_failed(
            "word",
            Code::Unparsable,
            r#""capital", in column 2 on line 1 of "PATH", isn't a boolean, the type of `capital`; if line 1 is a header, `header: true` skips it"#
        )
    );
}

#[test]
fn load_text_file_reads_a_files_lines() {
    use trendy_diag::Code;
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("load_text_file");
    std::fs::create_dir_all(&dir).unwrap();
    let file = |name: &str, contents: &[u8]| {
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    };
    // A path as a trendy text literal: Rust's `Debug` writes the same
    // escapes.
    let literal = |path: &Path| format!("{:?}", path.to_str().unwrap());
    let lines = |path: &Path| {
        let source = format!(
            "operator main{{}} [\n    var lines: array text;\n    call load_text_file{{path: {}, out lines: lines}};\n    call debug_value{{v: lines}};\n];\n",
            literal(path)
        );
        let stem = path.file_stem().unwrap().to_str().unwrap();
        run_source(&format!("load_text_file_{stem}"), &source)
    };
    // A line ends at a line feed, or a carriage return and a line feed,
    // and the last needs neither; a carriage return alone is text. The
    // lines are in the file's order, and equal ones are each there.
    for (name, contents, expected) in [
        ("lf", &b"a\nb\n"[..], r#"array ["a", "b"]"#),
        ("crlf", b"a\r\n\r\nb\r", r#"array ["a", "", "b\r"]"#),
        ("unended", b"a", r#"array ["a"]"#),
        ("empty_line", b"\n", r#"array [""]"#),
        ("empty", b"", "array text []"),
        ("repeated", "é\né\n".as_bytes(), r#"array ["é", "é"]"#),
        ("reversed", b"b\na\n", r#"array ["b", "a"]"#),
    ] {
        let path = file(&format!("{name}.txt"), contents);
        assert_eq!(lines(&path), (0, format!("{expected}\n")), "{name}");
    }

    let missing = dir.join("missing.txt");
    assert_eq!(
        lines(&missing),
        (
            1,
            format!(
                "error[{}]: can't read {}: No such file or directory\n{}",
                Code::UnreadableFile.id(),
                literal(&missing),
                site("load_text_file_missing", 3)
            )
        )
    );
    let not_utf8 = file("latin1.txt", b"Nile\nParan\xe1\n");
    assert_eq!(
        lines(&not_utf8),
        (
            1,
            format!(
                "error[{}]: line 2 of {} isn't UTF-8 text\n{}",
                Code::NotUtf8.id(),
                literal(&not_utf8),
                site("load_text_file_latin1", 3)
            )
        )
    );
}

#[test]
fn loads_make_relations_of_arrays_when_running() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("unload");
    std::fs::create_dir_all(&dir).unwrap();
    // A file's lines, read when running, into relations: numbered, each
    // line is there, and, without positions, equal ones are one. The
    // relations keep their texts once the array is gone. An array of
    // tuples, loaded back, gives their own tuples, or a tuple-valued
    // attribute.
    let unloaded = |name: &str, contents: &str| {
        let path = dir.join(format!("{name}.txt"));
        std::fs::write(&path, contents).unwrap();
        let source = format!(
            "operator main{{}} [
    var lines: array text;
    call load_text_file{{path: {:?}, out lines: lines}};
    var numbered: relation {{ line: text, n: integer }};
    load numbered from lines as line numbered n;
    var distinct: relation {{ line: text }};
    load distinct from lines as line where line <> \"\";
    lines := array text [];
    call debug_value{{v: numbered}};
    call debug_value{{v: distinct}};
    var pairs: array tuple {{ line: text, n: integer }};
    load pairs from numbered order [desc n];
    var again: relation {{ line: text, n: integer, at: integer }};
    load again from pairs numbered at where at < 4;
    call debug_value{{v: again}};
    var nested: relation {{ t: tuple {{ line: text, n: integer }} }};
    load nested from pairs as t;
    call debug_value{{v: count{{r: nested}}}};
];
",
            path.to_str().unwrap()
        );
        let (code, stderr) = run_source(&format!("unload_{name}"), &source);
        assert_eq!(code, 0, "{stderr}");
        stderr.lines().map(str::to_string).collect::<Vec<_>>()
    };
    assert_eq!(
        unloaded("lines", "b\na\n\nb\n"),
        [
            r#"relation { line: text, n: integer } { { line: "", n: 3 }, { line: "a", n: 2 }, { line: "b", n: 1 }, { line: "b", n: 4 } }"#,
            r#"relation { line: text } { { line: "a" }, { line: "b" } }"#,
            r#"relation { at: integer, line: text, n: integer } { { at: 1, line: "b", n: 4 }, { at: 2, line: "", n: 3 }, { at: 3, line: "a", n: 2 } }"#,
            "4",
        ]
    );
    assert_eq!(
        unloaded("empty", ""),
        [
            "relation { line: text, n: integer } {}",
            "relation { line: text } {}",
            "relation { at: integer, line: text, n: integer } {}",
            "0",
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
    assert_eq!(bits[..4], ['1', '1', '1', '1']);
    // Each scalar call: `=`, `<>`, `<`, `<=`, `>`, `>=`.
    let scalars: Vec<String> = bits[4..64]
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
    // Each tuple, relation and position call: `=`, `<>`.
    let values: Vec<String> = bits[64..]
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
            "10", // components compare as their types' values do
            "01", // components compare by name
        ]
    );
}

#[test]
fn orders_in_another_representation_compare_when_running() {
    // Points by their distance from the origin, in kilometres, descending,
    // then their angle, descending: each comparison converts the points,
    // and their distances, to the representations their orders compare.
    // `compare` computes when running what `main` folds.
    let source = "type distance {
    { m: approximate },
    { km: approximate } to { m := km * 1e3 } from { km := m / 1e3 },
    order [desc km],
};
type point {
    { x: approximate, y: approximate },
    { r: distance, theta: approximate }
        to { x := r.m * cos{of: theta}, y := r.m * sin{of: theta} }
        from { r := distance{m: sqrt{of: x * x + y * y}}, theta := atan2{y: y, x: x} },
    order [asc r, desc theta],
};
type site { { name: text, at: point }, order [asc at, asc name] };

var points := relation {
    { name: \"far\", at: point{x: 3e0, y: 4e0} },
    { name: \"west\", at: point{x: -1e0, y: 0e0} },
    { name: \"north\", at: point{x: 0e0, y: 1e0} },
};
var sites := relation {
    { s: site{name: \"b\", at: point{x: 0e0, y: 1e0}} },
    { s: site{name: \"a\", at: point{x: 0e0, y: 1e0}} },
    { s: site{name: \"c\", at: point{x: -1e0, y: 0e0}} },
};

operator outward{r: relation { name: text, at: point }}: array text [
    var names: array text;
    load names value from r order [asc at] remove {at};
    names
];

operator ranked{r: relation { s: site }}: array site [
    var all: array site;
    load all value from r order [desc s];
    all
];

operator compare{r: relation { name: text, at: point }, s: relation { s: site }} [
    call debug_value{v: min{r: r, of: at}};
    call debug_value{v: max{r: r, of: at}};
    call debug_value{v: r where at < point{x: 0e0, y: 1e0}};
    call debug_value{v: outward{r: r}};
    call debug_value{v: ranked{r: s}};
];

operator main{} [
    call debug_value{v: min{r: points, of: at}};
    call debug_value{v: max{r: points, of: at}};
    call debug_value{v: points where at < point{x: 0e0, y: 1e0}};
    call debug_value{v: outward{r: points}};
    call debug_value{v: ranked{r: sites}};
    call compare{r: points, s: sites};
];
";
    let (code, stderr) = run_source("orders_in_another_representation", source);
    assert_eq!(code, 0, "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    let (folded, run) = lines.split_at(lines.len() / 2);
    assert_eq!(
        folded, run,
        "folded when compiling, and computed when running"
    );
    assert_eq!(
        folded,
        [
            "point{x: 3e0, y: 4e0}",
            "point{x: 0e0, y: 1e0}",
            r#"relation { at: point, name: text } { { at: point{x: -1e0, y: 0e0}, name: "west" }, { at: point{x: 3e0, y: 4e0}, name: "far" } }"#,
            r#"array ["far", "west", "north"]"#,
            r#"array [site{at: point{x: 0e0, y: 1e0}, name: "b"}, site{at: point{x: 0e0, y: 1e0}, name: "a"}, site{at: point{x: -1e0, y: 0e0}, name: "c"}]"#,
        ]
    );
}

/// Compile `source`, run it in the workspace root, and return its exit
/// code and stderr.
fn run_source(name: &str, source: &str) -> (i32, String) {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let input = dir.join(format!("{name}.trendy"));
    std::fs::write(&input, source).unwrap();
    build_and_run(&input, &dir.join(name), &workspace_root())
}

/// The error an overflow writes, of `operation`.
fn overflow(operation: &str) -> String {
    format!(
        "error[{}]: overflow: {operation} is out of its type's range",
        trendy_diag::Code::Overflow.id()
    )
}

/// The error a division by zero writes, of `operation`.
fn division(operation: &str) -> String {
    format!(
        "error[{}]: division by zero: {operation} has no result",
        trendy_diag::Code::DivisionByZero.id()
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
    // The error writes the operation, with its operands' values, and a
    // negative right operand in parentheses.
    let cases = [
        ("add", "a + 1", overflow("9223372036854775807 + 1")),
        ("subtract", "-a - 2", overflow("-9223372036854775807 - 2")),
        ("multiply", "a * 2", overflow("9223372036854775807 * 2")),
        ("negate", "-(-a - 1)", overflow("-(-9223372036854775808)")),
        // `x div -1` is computed as `-x`, which fails for the same `x`,
        // and the error writes the operation computed.
        (
            "divide_min",
            "(-a - 1) div -1",
            overflow("-(-9223372036854775808)"),
        ),
        (
            "divide_min_by",
            "(-a - 1) div (b - 1)",
            overflow("-9223372036854775808 div (-1)"),
        ),
        (
            "divide_zero",
            "a div b",
            division("9223372036854775807 div 0"),
        ),
        ("mod_zero", "a mod b", division("9223372036854775807 mod 0")),
        // In a condition, computed for each tuple.
        (
            "where",
            "relation { { n: a } } where n + 1 > 0",
            overflow("9223372036854775807 + 1"),
        ),
    ];
    for (name, expr, failure) in cases {
        let name = format!("fail_{name}");
        let (code, stderr) = run_source(&name, &program(expr));
        let expected = format!("1\n{failure}\n{}{}", site(&name, 3), called(&name, "f", 8));
        assert_eq!((code, stderr), (1, expected), "{expr}");
    }
    // The smallest integer `mod -1` has a result.
    let (code, stderr) = run_source("mod_min", &program("(-a - 1) mod -1"));
    assert_eq!((code, &stderr[..]), (0, "1\n0\n2\n"));
}

/// Taking a relation's only tuple, combining relations, indexing an array
/// and joining tuples that fail stop the program, writing the values
/// involved.
#[test]
fn values_that_arent_there_stop_the_program() {
    use trendy_diag::Code;
    let cases = [
        // A relation with no tuples, or several, has no only tuple.
        (
            "fail_no_tuple",
            "tuple from (relation { { n: a } } join relation { { n: b } })",
            Code::NotOneTuple,
            "the relation has no tuples, but exactly one is taken",
        ),
        (
            "fail_several_tuples",
            "value from relation { { n: a }, { n: b } }",
            Code::NotOneTuple,
            "the relation has 2 tuples, but exactly one is taken",
        ),
        // A disjoint union whose operands share a tuple, and an included
        // minus that removes one that isn't there.
        (
            "fail_d_union",
            "relation { { n: a } } d_union relation { { n: a } }",
            Code::SharedTuple,
            "d_union or d_insert adds a tuple that is already there: tuple { n: 9223372036854775807 }",
        ),
        (
            "fail_i_minus",
            "relation { { n: a } } i_minus relation { { n: b } }",
            Code::MissingTuple,
            "i_minus or i_delete removes a tuple that isn't there: tuple { n: 0 }",
        ),
        // An array has no element at position 0, nor beyond its length.
        (
            "fail_index_zero",
            "array [a][b]",
            Code::IndexOutOfRange,
            "an array has no element at position 0: positions count from 1",
        ),
        (
            "fail_index_beyond",
            "array [a][b + 2]",
            Code::IndexOutOfRange,
            "the array has 1 element, so none at position 2",
        ),
        (
            "fail_index_empty",
            "(array integer [])[b + 1]",
            Code::IndexOutOfRange,
            "the array is empty, so it has no element at position 1",
        ),
        // Tuples that disagree on a common attribute have no join.
        (
            "fail_tuple_join",
            "tuple { n: a, m: 1 } join tuple { n: b }",
            Code::TupleMismatch,
            "the tuples disagree on `n`, so they have no join: `n` is 9223372036854775807 in one and 0 in the other",
        ),
    ];
    for (name, expr, code, message) in cases {
        let (status, stderr) = run_source(name, &program(expr));
        let expected = format!(
            "1\nerror[{}]: {message}\n{}{}",
            code.id(),
            site(name, 3),
            called(name, "f", 8)
        );
        assert_eq!((status, stderr), (1, expected), "{expr}");
    }
}

/// A value whose components break a constraint of its type stops the
/// program where it is selected, by its selector, `replace` or `update`,
/// writing the value and the first constraint it breaks.
#[test]
fn values_that_break_their_types_constraints_stop_the_program() {
    let program = |body: &str| {
        format!(
            "type percentage {{ {{ value: integer }}, constraint value >= 0, constraint value <= 100 }};\n\noperator f{{b: integer}} [\n    var p := percentage{{value: b}};\n    {body}\n    call debug_value{{v: p}};\n];\n\noperator main{{}} [\n    call f{{b: 0}};\n];\n"
        )
    };
    let broken = |value: i64, constraint: &str| {
        format!(
            "error[{}]: percentage{{value: {value}}} isn't a value of `percentage`: it breaks the constraint `{constraint}`",
            trendy_diag::Code::BrokenConstraint.id()
        )
    };
    // A selector's failure passes through the selector, called where the
    // value is, then `f`; a replace checks the constraints in `f`.
    let selector = |name: &str| called(name, "percentage", 5);
    let cases = [
        (
            "constraint_selector",
            "p := percentage{value: b - 1};",
            broken(-1, "value >= 0"),
            true,
        ),
        (
            "constraint_replace",
            "p := p replace { value := value + 101 };",
            broken(101, "value <= 100"),
            false,
        ),
        (
            "constraint_update",
            "update p { value := value - 1 };",
            broken(-1, "value >= 0"),
            false,
        ),
    ];
    for (name, body, message, selected) in cases {
        let (status, stderr) = run_source(name, &program(body));
        let expected = format!(
            "{message}\n{}{}{}",
            site(name, 5),
            if selected {
                selector(name)
            } else {
                String::new()
            },
            called(name, "f", 10)
        );
        assert_eq!((status, stderr), (1, expected), "{body}");
    }
    // Components that satisfy them select the value.
    let (status, stderr) = run_source(
        "constraint_kept",
        &program("update p { value := value + 100 };"),
    );
    assert_eq!((status, &stderr[..]), (0, "percentage{value: 100}\n"));
}

/// A value selected in a possible representation other than the stored
/// one is stored as its conversion computes, and read in any when the
/// program runs; one that breaks a constraint naming another's component
/// stops the program at the statement that selects it, not in the
/// constraint, whose conversion sets a site of its own.
#[test]
fn values_are_converted_between_representations_when_running() {
    let source = |celsius: &str| {
        format!(
            "type temperature {{
    {{ kelvin: approximate }},
    {{ celsius: approximate }}
        to {{ kelvin := celsius + 273.15e0 }}
        from {{ celsius := kelvin - 273.15e0 }},
    constraint celsius < 1000e0,
}};

operator show{{celsius: approximate}} [
    var t := temperature{{celsius: celsius}};
    call debug_value{{v: t}};
    call debug_value{{v: t.celsius}};
];

operator main{{}} [
    call show{{celsius: {celsius}}};
];
"
        )
    };
    let (status, stderr) = run_source("converted", &source("100e0"));
    assert_eq!(
        (status, &stderr[..]),
        (0, "temperature{kelvin: 3.7315e2}\n1e2\n")
    );
    let (status, stderr) = run_source("converted_broken", &source("2000e0"));
    assert_eq!(
        (status, stderr),
        (
            1,
            format!(
                "error[{}]: temperature{{kelvin: 2.27315e3}} isn't a value of `temperature`: it breaks the constraint `celsius < 1000e0`\n{}{}{}",
                trendy_diag::Code::BrokenConstraint.id(),
                site("converted_broken", 10),
                called("converted_broken", "temperature{celsius: approximate}", 10),
                called("converted_broken", "show", 16)
            )
        )
    );
}

/// Values of a type of the program's own with an order compare by it when
/// the program runs, as when compiling: its components in turn, a
/// descending one the other way, and a value inside by its own type's.
#[test]
fn ordered_values_compare_by_their_types_order_when_running() {
    let source = "type score { { points: integer, name: text }, order [desc points, asc name] };
type entry { { at: score, round: integer }, order [asc round, asc at] };

operator compare{a: integer, b: integer} [
    var high := score{points: a, name: \"Ama\"};
    var low := score{points: b, name: \"Zhu\"};
    call debug_value{v: high < low};
    var entries := relation {
        { e: entry{at: high, round: 2} },
        { e: entry{at: low, round: 1} },
        { e: entry{at: score{points: a, name: \"Bo\"}, round: 2} },
    };
    call debug_value{v: min{r: entries, of: e}};
    var ordered: array entry;
    load ordered value from entries order [desc e];
    call debug_value{v: ordered};
];

operator main{} [
    call compare{a: 9, b: 3};
];
";
    let (status, stderr) = run_source("ordered_values", source);
    assert_eq!(
        (status, stderr.lines().collect::<Vec<_>>()),
        (
            0,
            vec![
                "true",
                r#"entry{at: score{name: "Zhu", points: 3}, round: 1}"#,
                r#"array [entry{at: score{name: "Bo", points: 9}, round: 2}, entry{at: score{name: "Ama", points: 9}, round: 2}, entry{at: score{name: "Zhu", points: 3}, round: 1}]"#,
            ]
        )
    );
}

#[test]
fn an_if_fails_only_if_the_branch_it_takes_does() {
    let overflow = format!(
        "1\n{}\n{}{}",
        overflow("9223372036854775807 + 1"),
        site("fail_if", 3),
        called("fail_if", "f", 8)
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
                    "1\nerror[{}]: the relation has no tuples, so it has no {which}\n{}{}",
                    Code::NoMinOrMax.id(),
                    site(name, 3),
                    called(name, "f", 8)
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

/// Assignments that break a rule stop the program, writing the tuple or
/// the positions: two changes that remove the same tuple, a disjoint
/// insert of a tuple already there, an included delete of one that isn't,
/// two changes to one element of an array, or to an element and one in
/// it, and two tuples with the same key, which the error writes, with the
/// variable and the key.
#[test]
fn assignments_that_break_a_rule_stop_the_program() {
    use trendy_diag::Code;
    let assigning = |statement: &str| {
        format!(
            "var poets := relation {{ {{ name: \"Ada\", born: 1815 }} }}\nkey {{ name }};\nvar grid := array [array [1, 2], array [3, 4]];\noperator f{{a: integer}} [\n    call debug_value{{v: 1}};\n    {statement};\n    call debug_value{{v: 2}};\n];\n\noperator main{{}} [\n    call f{{a: 1815}};\n];\n"
        )
    };
    for (name, statement, failure) in [
        (
            // `variables.trendy`'s example of changes that conflict.
            "fail_conflict",
            r#"assign { update poets { born := a } where name = "Ada"; delete poets where born < 1900; }"#,
            format!(
                r#"error[{}]: two changes in an assign block remove the same tuple: tuple {{ born: 1815, name: "Ada" }}"#,
                Code::ConflictingChanges.id()
            ),
        ),
        (
            "fail_d_insert",
            r#"d_insert poets relation { { name: "Ada", born: a } }"#,
            format!(
                r#"error[{}]: d_union or d_insert adds a tuple that is already there: tuple {{ born: 1815, name: "Ada" }}"#,
                Code::SharedTuple.id()
            ),
        ),
        (
            "fail_i_delete",
            r#"i_delete poets relation { { name: "Ada", born: a + 1 } }"#,
            format!(
                r#"error[{}]: i_minus or i_delete removes a tuple that isn't there: tuple {{ born: 1816, name: "Ada" }}"#,
                Code::MissingTuple.id()
            ),
        ),
        // `a - 1814` is 1, which is known only when running.
        (
            "fail_same_element",
            "assign { grid[a - 1814][1] := 0; grid[1][1] := 5; }",
            format!(
                "error[{}]: two changes in an assign block set the same element, at [1][1]",
                Code::ConflictingChanges.id()
            ),
        ),
        (
            "fail_element_in_element",
            "assign { grid[a - 1814] := array [0, 0]; grid[1][2] := 5; }",
            format!(
                "error[{}]: one change in an assign block sets the element at [1], and another an element in it",
                Code::ConflictingChanges.id()
            ),
        ),
        (
            "fail_key",
            r#"insert poets relation { { name: "Ada", born: a + 1 } }"#,
            format!(
                r#"error[{}]: two tuples of `poets` have the same values for its key {{ name }}: tuple {{ born: 1815, name: "Ada" }} and tuple {{ born: 1816, name: "Ada" }}"#,
                Code::KeyViolated.id()
            ),
        ),
    ] {
        let (code, stderr) = run_source(name, &assigning(statement));
        assert_eq!(
            (code, stderr),
            (
                1,
                format!("1\n{failure}\n{}{}", site(name, 6), called(name, "f", 11))
            ),
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
fn types() {
    assert_eq!(
        folded_and_run("types"),
        [
            "position{latitude: -1.29e0, longitude: 3.682e1}",
            "journey{destination: position{latitude: -1.205e1, longitude: -7.704e1}, origin: position{latitude: -1.29e0, longitude: 3.682e1}}",
            "true",
            "-7.704e1",
            // The antipode.
            "position{latitude: 1.29e0, longitude: -1.4318e2}",
            // The flight back: the components swapped.
            "journey{destination: position{latitude: -1.29e0, longitude: 3.682e1}, origin: position{latitude: -1.205e1, longitude: -7.704e1}}",
            // South of the equator.
            "┌───────────────────────────────────────────────────┬────────────────┐",
            "│ at                                                │ name           │",
            "├───────────────────────────────────────────────────┼────────────────┤",
            "│ position{latitude: -2.027e1, longitude: 3.093e1}  │ Great Zimbabwe │",
            "│ position{latitude: -1.316e1, longitude: -7.255e1} │ Machu Picchu   │",
            "└───────────────────────────────────────────────────┴────────────────┘",
            // On the equator.
            "┌──────────────────────────────────────────────┬────────────────┐",
            "│ at                                           │ name           │",
            "├──────────────────────────────────────────────┼────────────────┤",
            "│ position{latitude: 0e0, longitude: -7.255e1} │ Machu Picchu   │",
            "│ position{latitude: 0e0, longitude: 3.093e1}  │ Great Zimbabwe │",
            "│ position{latitude: 0e0, longitude: 3.544e1}  │ Petra          │",
            "│ position{latitude: 0e0, longitude: 1.3573e2} │ Kinkaku-ji     │",
            "└──────────────────────────────────────────────┴────────────────┘",
            // The first festival, those before June, and all of them, in
            // order, by the calendar day's order.
            "calendar_day{day: 17, month: 2, year: 2026}",
            "┌────────────────┬─────────────────────────────────────────────┐",
            "│ name           │ on                                          │",
            "├────────────────┼─────────────────────────────────────────────┤",
            "│ Lunar New Year │ calendar_day{day: 17, month: 2, year: 2026} │",
            "│ Nowruz         │ calendar_day{day: 20, month: 3, year: 2026} │",
            "└────────────────┴─────────────────────────────────────────────┘",
            r#"array ["Lunar New Year", "Nowruz", "Inti Raymi", "Diwali"]"#,
            // A temperature selected in degrees Celsius, stored in kelvins,
            // one in degrees Fahrenheit read in Celsius, and compared, and
            // each changed in its own representation: 10 degrees Celsius
            // warmer, and 18 degrees Fahrenheit cooler.
            "temperature{kelvin: 2.7115e2}",
            "4e1",
            "true",
            "temperature{kelvin: 2.8115e2}",
            "temperature{kelvin: 3.0315e2}",
            "┌───────────────────────────────────────────────────┬─────────────┐",
            "│ at                                                │ stop        │",
            "├───────────────────────────────────────────────────┼─────────────┤",
            "│ position{latitude: -1.205e1, longitude: -7.704e1} │ destination │",
            "│ position{latitude: -1.29e0, longitude: 3.682e1}   │ origin      │",
            "└───────────────────────────────────────────────────┴─────────────┘",
        ]
    );
}

#[test]
fn inheritance() {
    assert_eq!(
        folded_and_run("inheritance"),
        [
            // The Third Manifesto's point reflected, its polar coordinates,
            // half a turn from the point's, the distance between the two,
            // points selected by their polar coordinates, the origin's at
            // any angle, the point twice as far from the origin, by its
            // polar `r`, and also a quarter turn further round, by updates
            // of `r` and `theta` combined, then moved, a segment's begin
            // moved, an ellipse's
            // semiaxes changed together, a polygon's corners, its vertex
            // nearest the origin, by the points' polar order, and its
            // vertices' numbers in that order.
            "point{x: -5e0, y: -2.5e0}",
            "tuple { r: distance{m: 5.5901699437494745e0}, theta: angle{rho: 3.6052402625905993e0} }",
            "distance{m: 1.1180339887498949e1}",
            "point{x: 1.8369701987210297e-16, y: 3e0}",
            "point{x: 0e0, y: 0e0}",
            "point{x: 1e1, y: 5e0}",
            "point{x: -5e0, y: 1e1}",
            "point{x: 2e0, y: 2.5e0}",
            "lineseg{begin: point{x: 6.5e0, y: 2.5e0}, end: point{x: -5e0, y: -2.5e0}}",
            "ellipse{a: distance{m: 3e0}, b: distance{m: 2e0}, ctr: point{x: 5e0, y: 2.5e0}}",
            "3",
            "point{x: 0e0, y: 0e0}",
            "array [1, 3, 2]",
            // Plane figures of both kinds, as values of their union type.
            "┌─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┬──────┐",
            "│ figure                                                                                                                                                                                              │ name │",
            "├─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┼──────┤",
            "│ ellipse{a: distance{m: 3e0}, b: distance{m: 2e0}, ctr: point{x: 5e0, y: 2.5e0}}                                                                                                                     │ pond │",
            "│ polygon{vertices: relation { v_no: integer, vertex: point } { { v_no: 1, vertex: point{x: 0e0, y: 0e0} }, { v_no: 2, vertex: point{x: 4e0, y: 0e0} }, { v_no: 3, vertex: point{x: 0e0, y: 3e0} } }} │ plot │",
            "└─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┴──────┘",
            // The ellipses among them, narrowed by `is`, and what each is.
            "┌──────────────────┬──────┐",
            "│ major            │ name │",
            "├──────────────────┼──────┤",
            "│ distance{m: 3e0} │ pond │",
            "└──────────────────┴──────┘",
            // The ellipses and polygons, combined, are the figures, and
            // the polygons' vertices, read once a difference of plane
            // figures is intersected with them.
            "true",
            "┌───┬──────┐",
            "│ n │ name │",
            "├───┼──────┤",
            "│ 3 │ plot │",
            "└───┴──────┘",
            r#""ellipse""#,
            r#""3-gon""#,
            // An ellipse with equal semiaxes is a circle, and widened, just
            // an ellipse again; a circle selected by its radius has it.
            r#""circle""#,
            r#""ellipse""#,
            "distance{m: 2e0}",
            // Their areas, each computed by its kind's version.
            "┌──────┬─────────────────────┐",
            "│ name │ size                │",
            "├──────┼─────────────────────┤",
            "│ plot │                 6e0 │",
            "│ pond │ 1.884955592153876e1 │",
            "└──────┴─────────────────────┘",
            // Each figure, narrowed in an `if` statement's branch: the
            // plot's vertices, and the pond's major semiaxis.
            "3",
            "distance{m: 3e0}",
        ]
    );
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
    // Restricted as it is joined.
    "┌──────┬────────┬──────┬────────┐",
    "│ born │ city   │ name │ place  │",
    "├──────┼────────┼──────┼────────┤",
    "│ 1815 │ London │ Ada  │ London │",
    "│ 1816 │ London │ Ada  │ London │",
    "└──────┴────────┴──────┴────────┘",
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
            // Loaded: the wettest first, the names alphabetically, and the
            // rain least first, by an operator generic over its type.
            r#"array [tuple { city: "Mumbai", rain: 2200 }, tuple { city: "Lagos", rain: 1500 }, tuple { city: "Cairo", rain: 25 }, tuple { city: "Lima", rain: 15 }]"#,
            r#"array ["Cairo", "Lagos", "Lima", "Mumbai"]"#,
            "array [15, 25, 1500, 2200]",
            // Ordered by year, then without it: teff twice.
            r#"array ["teff", "sorghum", "teff"]"#,
            // Loaded back into relations: each city with its place in the
            // array, and the crops, teff once, without sorghum.
            "┌────────┬──────┬──────┐",
            "│ city   │ rain │ rank │",
            "├────────┼──────┼──────┤",
            "│ Cairo  │   25 │    3 │",
            "│ Lagos  │ 1500 │    2 │",
            "│ Lima   │   15 │    4 │",
            "│ Mumbai │ 2200 │    1 │",
            "└────────┴──────┴──────┘",
            "┌──────┐",
            "│ crop │",
            "├──────┤",
            "│ teff │",
            "└──────┘",
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
