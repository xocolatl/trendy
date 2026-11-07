use std::fmt::Write as _;
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
            // A constant in a loop, computed each time round.
            "3",
            "2",
            "1",
        ]
    );
}

/// What `examples/operators.trendy` writes last, of its operators with
/// `var` and `out` parameters.
const PARAMETERS: [&str; 29] = [
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
    // Recursion: a hundred million calls in constant stack.
    "\"an even number\"",
    "true",
    "\"left\"",
    "0",
    "┌──────┬───────────┐",
    "│ km   │ town      │",
    "├──────┼───────────┤",
    "│    0 │ Xi'an     │",
    "│ 1700 │ Dunhuang  │",
    "│ 3600 │ Kashgar   │",
    "│ 4400 │ Samarkand │",
    "│ 4500 │ Samarkand │",
    "└──────┴───────────┘",
];

/// What `operators` writes before `PARAMETERS`.
const OPERATORS: &[&str] = &[
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
    // A read-only operator's statements, run when it is called.
    "┌───────┐",
    "│ value │",
    "├───────┤",
    "│     1 │",
    "│     3 │",
    "│     9 │",
    "│    27 │",
    "└───────┘",
    // A call its operator's constraint is true of.
    "400",
    // With names alone.
    "300",
    // A value block's, for each tuple.
    "┌─────────┬─────────────┬───────┬───────┐",
    "│ country │ dish        │ price │ rises │",
    "├─────────┼─────────────┼───────┼───────┤",
    "│ Ghana   │ jollof rice │  1200 │    27 │",
    "│ Japan   │ ramen       │   900 │    24 │",
    "│ Mexico  │ pozole      │   750 │    22 │",
    "└─────────┴─────────────┴───────┴───────┘",
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
];

#[test]
fn operators() {
    let (code, stderr) = run_example("operators");
    assert_eq!(code, 0, "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    let (others, parameters) = lines.split_at(lines.len() - PARAMETERS.len());
    assert_eq!(parameters, PARAMETERS);
    assert_eq!(others, OPERATORS);
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

/// The program `source` analyzed, with the units that come with the
/// compiler.
fn analyze(source: &str) -> trendy_hir::Program {
    let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
    assert!(parse.errors.is_empty(), "{source}: {:#?}", parse.errors);
    let unit = trendy_sema::File {
        name: "program",
        path: "program.trendy",
        tree: &parse.tree,
    };
    let library = trendy_load::library();
    let library: Vec<trendy_sema::File> = library
        .iter()
        .map(|unit| trendy_sema::File {
            name: &unit.name,
            path: &unit.path,
            tree: &unit.parse.tree,
        })
        .collect();
    trendy_sema::analyze(&[unit], &library).unwrap()
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
    // Evaluated when compiling, and literals in other radixes.
    assert_eq!(
        lines[..9],
        [
            "7",
            "9",
            "-9223372036854775808",
            "9223372036854775807",
            "-42",
            "37468000",
            "493",
            "65535",
            "165",
        ]
    );
    // Each row: `-a`, `a + b`, `a - b`, `a * b`, `a div b`, `a mod b`.
    let rows: Vec<_> = lines[9..33].chunks(6).collect();
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
    assert_eq!(lines[33..35], ["7", "-9223372036854775808"]);
    // Each row: `not a`, `a and b`, `a or b`, `a xor b`.
    let rows: Vec<_> = lines[35..43].chunks(4).collect();
    assert_eq!(
        rows,
        [["-13", "8", "14", "6"], ["0", "65535", "-1", "-65536"]]
    );
    // Each row: `a shl n`, `a shr n`.
    let rows: Vec<_> = lines[43..49].chunks(2).collect();
    assert_eq!(rows, [["20", "1"], ["-18", "-5"], ["1", "6"]]);
    // `0x8000_0001` rotated 4 places as a 32-bit word.
    assert_eq!(lines[49], "24");
    // The integers from one end to another, one apart and in twos down, and
    // computed with.
    assert_eq!(
        lines[50..],
        [
            "relation { value: integer } { { value: 1 }, { value: 2 }, { value: 3 } }",
            "relation { value: integer } { { value: 1 }, { value: 3 } }",
            "relation { cube: integer, value: integer } { { cube: 1, value: 1 }, { cube: 8, value: 2 }, { cube: 27, value: 3 } }",
        ]
    );
}

/// `series` gives what Postgres's `generate_series` gives, the integers
/// `range` gives over wider numbers here: with `to` on the way or not, in
/// either direction, none where `to` is the other way, and at the limits of
/// the integers, which `to - from` and `value * step` would each pass.
#[test]
fn series_is_generate_series() {
    let (min, max) = (i64::MIN, i64::MAX);
    let half = 1i64 << 62;
    let cases = [
        (1, 5, 1),
        (1, 10, 3),
        (1, 10, 4),
        (5, 1, -2),
        (5, 1, 1),
        (1, 5, -1),
        (3, 3, 1),
        (3, 3, -1),
        (-5, 5, 2),
        (5, -5, -2),
        (-5, 5, -2),
        (5, -5, 2),
        (-1, 0, 5),
        (0, -1, -5),
        (0, 5, 7),
        (0, 0, 7),
        (max - 1, max, 1),
        (min + 1, min, -1),
        (max - 2, max, 2),
        (max, max, max),
        (min, min, min),
        (max, min, min),
        (min, max, max),
        (min, max, half),
        (max, min, -half),
        (min, 0, half),
        (0, min, -half),
        (0, min, min),
        (0, max, max),
        (min, -1, half),
        (-1, max, max),
        (-1, min, min),
        (1, min, min),
        (min + 1, max, max),
        (min, max, 3 * (half / 2)),
        (max, min, -3 * (half / 2)),
    ];
    let mut calls = String::new();
    let mut expected = String::new();
    for (from, to, step) in cases {
        writeln!(
            calls,
            "    call debug_value{{v: series{{from: {from}, to: {to}, step: {step}}}}};"
        )
        .unwrap();
        let (first, last, by) = (i128::from(from), i128::from(to), i128::from(step));
        let mut values: Vec<i128> = std::iter::successors(Some(first), |at| Some(at + by))
            .take_while(|at| if by > 0 { *at <= last } else { *at >= last })
            .collect();
        values.sort_unstable();
        let tuples: Vec<String> = values
            .iter()
            .map(|value| format!("{{ value: {value} }}"))
            .collect();
        if tuples.is_empty() {
            writeln!(expected, "relation {{ value: integer }} {{}}").unwrap();
        } else {
            writeln!(
                expected,
                "relation {{ value: integer }} {{ {} }}",
                tuples.join(", ")
            )
            .unwrap();
        }
    }
    // One apart, and as many as make adding them one at a time show.
    let source = format!(
        r"operator main{{}} [
{calls}    call debug_value{{v: series{{from: 2, to: 4}}}};
    call debug_value{{v: series{{from: 4, to: 2}}}};
    call debug_value{{v: count{{r: series{{from: 1, to: 2000000}}}}}};
    call debug_value{{v: sum{{r: series{{from: 2000000, to: 1, step: -1}}, of: value}}}};
];
"
    );
    expected.push_str(
        "relation { value: integer } { { value: 2 }, { value: 3 }, { value: 4 } }
relation { value: integer } {}
2000000
2000001000000
",
    );
    let (code, stderr) = run_source("series", &source);
    assert_eq!((code, stderr), (0, expected));
}

/// A step of 0 breaks `series`'s constraint: when the program runs, saying
/// the argument it reads and where the call is, which a transaction can take,
/// and when compiling, if the step is known then, whatever else isn't.
#[test]
fn a_series_with_a_step_of_zero_fails() {
    let (code, stderr) = run_source(
        "series_zero",
        "var zero := 0;\n\noperator main{} [\n    var failed: relation transaction_collection;\n    var n := 0;\n    transaction collecting failed [\n        n := count{r: series{from: 1, to: 5, step: zero}};\n    ];\n    call debug_value{v: count{r: failed}};\n    call debug_value{v: series{from: 1, to: 5, step: zero}};\n];\n",
    );
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(code, 1, "{stderr}");
    assert_eq!(lines[0], "1", "{stderr}");
    assert_eq!(
        lines[1],
        "error[R0024]: series{step: 0} can't be called: it breaks the constraint `step <> 0`",
        "{stderr}"
    );
    assert!(lines[2].contains("series_zero.trendy:10:"), "{stderr}");

    let (ok, _, stderr) = trendyc(
        "series_known_zero",
        "operator main{} [\n    var last := 5;\n    call debug_value{v: series{from: 1, to: last, step: 0}};\n];\n",
        &[],
    );
    assert!(!ok);
    assert!(
        stderr.contains("error[R0024]: `series{step: 0}` can't be called"),
        "{stderr}"
    );
    assert!(stderr.contains("series_known_zero.trendy:3:25"), "{stderr}");
}

/// `insert`, which changes a variable's own buffer where nothing else holds
/// it, is the same as `r := relation { … } union r`, which never does: for
/// tuples in order, out of it and repeated, by the thousand, of integers, and
/// of texts, which their buffers hold references to.
const INSERTS: &str = r#"operator main{} [
    var r: relation { a: integer, t: text };
    var s: relation { a: integer, t: text };
    var x := 1;
    var i := 0;
    var wrong := 0;
    while i < 3000 loop [
        x := x * 48271 mod 2147483647;
        var a := x mod 500;
        var t := to_text{from: x mod 7};
        var more := relation { { a, t } };
        /* Two at a time, some of them. */
        if i mod 5 = 0 then [
            more := more union relation { { a: a + 1, t: "two" } };
        ];
        insert r more;
        s := more union s;
        if r <> s then [
            wrong := wrong + 1;
        ];
        i := i + 1;
    ];
    /* In order, which goes last, and back, which goes first. */
    var up: relation { n: integer };
    var down: relation { n: integer };
    i := 1;
    while i <= 2000 loop [
        insert up relation { { n: i } };
        insert down relation { { n: 2001 - i } };
        i := i + 1;
    ];
    call debug_value{v: wrong};
    call debug_value{v: count{r}};
    call debug_value{v: up = down};
    call debug_value{v: sum{r: up, of: n}};
    call debug_value{v: min{r: down, of: n}};
];
"#;

#[test]
fn inserts_are_what_unions_are() {
    let (code, stderr) = run_source("inserts", INSERTS);
    assert_eq!(code, 0, "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines[0], "0", "{stderr}");
    let count: usize = lines[1].parse().unwrap();
    assert!(count > 500, "{stderr}");
    assert_eq!(lines[2..], ["true", "2001000", "1"], "{stderr}");
}

/// A value something else holds doesn't change when its variable's `insert`
/// does: another variable's, a parameter's, a transaction's that it restores,
/// a constant's, and a statement's own, which reads the variable as it was.
const INSERTS_AND_VALUES: &str = r"var constant := relation { { n: 1 } };
var global: relation { n: integer };

operator grown{r: relation { n: integer }, n: integer}: relation { n: integer } [
    var mine := r;
    insert mine relation { { n } };
    mine
];

operator into{var r: relation { n: integer }, n: integer} [
    insert r relation { { n } };
];

operator given_then_changed{r: relation { n: integer }} [
    insert global relation { { n: 0 } };
    call debug_value{v: min{r, of: n}};
];

operator made{out r: relation { n: integer }} [
    r := relation { { n: 7 } };
    insert r relation { { n: 8 } };
];

operator main{} [
    /* Built as it runs, so that the buffer is no constant's, and each holds
       the same one, where a tuple that goes first or between would show. */
    var built: relation { n: integer };
    insert built relation { { n: 10 } };
    insert built relation { { n: 20 } };
    var shared := built;
    insert built relation { { n: 5 } };
    call debug_value{v: shared};
    call debug_value{v: built};
    var between := grown{r: shared, n: 15};
    call debug_value{v: shared};
    call debug_value{v: between};
    var a := relation { { n: 1 }, { n: 2 } };
    var b := a;
    insert a relation { { n: 3 } };
    call debug_value{v: tuple { a: count{r: a}, b: count{r: b} }};
    /* Each again, now that each is the only holder. */
    insert b relation { { n: 4 } };
    insert a relation { { n: 5 } };
    call debug_value{v: a};
    call debug_value{v: b};
    /* A constant isn't changed. */
    insert constant relation { { n: 2 } };
    var fresh := relation { { n: 9 } };
    insert fresh relation { { n: 10 } };
    call debug_value{v: tuple { c: count{r: constant}, f: count{r: fresh} }};
    /* Through an operator that keeps its own copy, or changes the caller's. */
    var c := grown{r: a, n: 6};
    call debug_value{v: tuple { a: count{r: a}, c: count{r: c} }};
    call into{var r: a, n: 6};
    call debug_value{v: count{r: a}};
    var o: relation { n: integer };
    call made{out r: o};
    call debug_value{v: o};
    /* An argument kept while what it came from changes. */
    insert global relation { { n: 1 } };
    call given_then_changed{r: global};
    call debug_value{v: count{r: global}};
    /* What it holds already, and what comes from itself. */
    var same := a;
    insert a same;
    insert a a where n > 2;
    insert a a replace { n := n + 100 };
    call debug_value{v: count{r: a}};
    /* With others at once, each reads it as it was. */
    var d := relation { { n: 1 } };
    var e: relation { n: integer };
    assign {
        insert d relation { { n: 2 } };
        e := d;
    };
    call debug_value{v: tuple { d: count{r: d}, e: count{r: e} }};
    /* A failure undoes it, once and after the first has its own buffer. */
    var f := relation { { n: 1 } };
    var failed: relation transaction_collection;
    transaction collecting failed [
        insert f relation { { n: 2 } };
        insert f relation { { n: 3 } };
        d_insert f relation { { n: 3 } };
    ];
    call debug_value{v: f};
    call debug_value{v: count{r: failed}};
    var g: relation { n: integer };
    insert g relation { { n: 2 } };
    transaction collecting failed [
        insert g relation { { n: 1 } };
        d_insert g relation { { n: 1 } };
    ];
    call debug_value{v: g};
    /* A relation without attributes. */
    var dum: relation {};
    insert dum relation { {} };
    insert dum relation { {} };
    call debug_value{v: dum};
    /* Of relations, of texts made as it runs. */
    var nested: relation { r: relation { n: integer }, t: text };
    var i := 0;
    while i < 50 loop [
        var inner: relation { n: integer };
        insert inner relation { { n: i + 1 } };
        insert nested relation { { r: inner, t: to_text{from: i} } };
        insert inner relation { { n: i } };
        i := i + 1;
    ];
    call debug_value{v: count{r: nested}};
    call debug_value{v: count{r: nested where count{r: r} = 1}};
    call debug_value{v: every{r: nested, of: min{r: r, of: n} = parse_integer{from: t} + 1}};
];
";

#[test]
fn inserts_leave_what_else_holds_a_value_alone() {
    let (code, stderr) = run_source("inserts_and_values", INSERTS_AND_VALUES);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            "relation { n: integer } { { n: 10 }, { n: 20 } }",
            "relation { n: integer } { { n: 5 }, { n: 10 }, { n: 20 } }",
            "relation { n: integer } { { n: 10 }, { n: 20 } }",
            "relation { n: integer } { { n: 10 }, { n: 15 }, { n: 20 } }",
            "tuple { a: 3, b: 2 }",
            "relation { n: integer } { { n: 1 }, { n: 2 }, { n: 3 }, { n: 5 } }",
            "relation { n: integer } { { n: 1 }, { n: 2 }, { n: 4 } }",
            "tuple { c: 2, f: 2 }",
            "tuple { a: 4, c: 5 }",
            "5",
            "relation { n: integer } { { n: 7 }, { n: 8 } }",
            "1",
            "2",
            "10",
            "tuple { d: 2, e: 1 }",
            "relation { n: integer } { { n: 1 } }",
            "1",
            "relation { n: integer } { { n: 2 } }",
            "relation {} { {} }",
            "50",
            "50",
            "true",
        ],
        "{stderr}"
    );
}

/// What `insert` adds is checked against a variable's keys: by its first
/// attributes or others, among the tuples or between what is added, and what
/// is there already doesn't share a key's values with itself. The error writes
/// the tuples as a key checked whole does.
#[test]
fn inserts_check_keys_of_what_they_add() {
    for (key, first, second) in [
        ("a", "{ a: 2, z: 5 }", "{ a: 2, z: 9 }"),
        ("z", "{ a: 2, z: 7 }", "{ a: 4, z: 7 }"),
    ] {
        // Fine, then not, by what's added alone.
        let tuple = if key == "a" {
            "{ a: 2, z: 9 }"
        } else {
            "{ a: 4, z: 7 }"
        };
        let source = format!(
            "var r: relation {{ a: integer, z: integer }} key {{ {key} }};\n\noperator main{{}} [\n    insert r relation {{ {{ a: 1, z: 1 }} }};\n    insert r relation {{ {} }};\n    insert r relation {{ {} }};\n    insert r relation {{ {} }};\n    call debug_value{{v: count{{r}}}};\n    insert r relation {{ {tuple} }};\n    call debug_value{{v: 0}};\n];\n",
            if key == "a" {
                "{ a: 2, z: 5 }"
            } else {
                "{ a: 2, z: 7 }"
            },
            if key == "a" {
                "{ a: 2, z: 5 }"
            } else {
                "{ a: 2, z: 7 }"
            },
            "{ a: 3, z: 3 }",
        );
        let (code, stderr) = run_source(&format!("insert_key_{key}"), &source);
        assert_eq!(code, 1, "{stderr}");
        let lines: Vec<&str> = stderr.lines().collect();
        assert_eq!(lines[0], "3", "{stderr}");
        assert!(lines[1].starts_with("error[R0008]"), "{stderr}");
        assert!(lines[1].contains(first), "{stderr}");
        assert!(lines[1].contains(second), "{stderr}");
    }
    // Between what is added, and undone by a transaction.
    let (code, stderr) = run_source(
        "insert_key_between",
        "var r: relation { a: integer, z: integer } key { z };\n\noperator main{} [\n    insert r relation { { a: 1, z: 1 } };\n    var failed: relation transaction_collection;\n    transaction collecting failed [\n        insert r relation { { a: 2, z: 2 }, { a: 3, z: 2 } };\n    ];\n    call debug_value{v: count{r}};\n    call debug_value{v: count{r: failed}};\n];\n",
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stderr.lines().collect::<Vec<_>>(), ["1", "1"], "{stderr}");
}

/// A key computed by what can fail gives what going through the tuples gives,
/// of a value that isn't computed by an expression a run can use: as the first
/// attribute and as the first two, of integers and texts, from a value block
/// that runs statements, a call, and the tuple further out.
const KEYS_THAT_CAN_FAIL: &str = r#"operator twice{n: integer}: integer [
    n * 2
];

operator stepped{n: integer}: integer [
    var at := 0;
    while at < n loop [
        at := at + 3;
    ];
    at
];

operator main{} [
    var r := series{from: 1, to: 200} extend { b := value mod 7, t := to_text{from: value mod 40} } rename {value as a};
    var wrong := 0;
    var i := 0;
    while i < 300 loop [
        /* `+ 0` of the attribute reads it, so this goes through the tuples. */
        if (r where a = i + 1) <> (r where a + 0 = i + 1) then [
            wrong := wrong + 1;
        ];
        if (r where a = twice{n: i}) <> (r where a + 0 = twice{n: i}) then [
            wrong := wrong + 1;
        ];
        if (r where a = stepped{n: i}) <> (r where a + 0 = stepped{n: i}) then [
            wrong := wrong + 1;
        ];
        if (r where a = [ var k := i; k + 2 ]) <> (r where a + 0 = i + 2) then [
            wrong := wrong + 1;
        ];
        if (r where a = i + 1 and b = i mod 7) <> (r where a + 0 = i + 1 and b + 0 = i mod 7) then [
            wrong := wrong + 1;
        ];
        /* An attribute that goes first, of texts, as `t` sorts after `b`. */
        var s := r remove {a, b};
        if (s where t = to_text{from: i mod 50}) <> (s where t || "" = to_text{from: i mod 50}) then [
            wrong := wrong + 1;
        ];
        i := i + 1;
    ];
    /* The value of the tuple further out, once for each of them. */
    var counts := series{from: 1, to: 5} extend { n := count{r: r where a = value + 1} };
    call debug_value{v: wrong};
    call debug_value{v: counts};
    /* `delete`, by what can fail, and its own value block. */
    var left := series{from: 1, to: 9};
    delete left where value = i - 299;
    delete left where value = [ var k := 2; k + 1 ];
    call debug_value{v: left};
];
"#;

#[test]
fn keys_that_can_fail_find_what_going_through_the_tuples_does() {
    let (code, stderr) = run_source("keys_that_can_fail", KEYS_THAT_CAN_FAIL);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            "0",
            "relation { n: integer, value: integer } { { n: 1, value: 1 }, { n: 1, value: 2 }, { n: 1, value: 3 }, { n: 1, value: 4 }, { n: 1, value: 5 } }",
            "relation { value: integer } { { value: 2 }, { value: 4 }, { value: 5 }, { value: 6 }, { value: 7 }, { value: 8 }, { value: 9 } }",
        ],
        "{stderr}"
    );
}

/// A key that fails fails just when going through the tuples would have: if
/// there is a tuple to compute it for, each of them, whatever another key says,
/// and not behind a guard that holds for none. A conjunct that can fail is still
/// computed for every tuple, so a key doesn't narrow what it is computed for.
#[test]
fn keys_that_can_fail_fail_as_they_did() {
    let run = |name: &str, line: &str| {
        run_source(
            name,
            &format!(
                "operator main{{}} [\n    var zero := 0;\n    var none := series{{from: 1, to: 0}} extend {{ z := value }} rename {{value as a}};\n    var r := series{{from: 1, to: 3}} extend {{ z := value }} rename {{value as a}};\n    {line}\n    call debug_value{{v: 0}};\n];\n"
            ),
        )
    };
    let divides = "error[R0002]: division by zero: 1 div 0";
    for (name, line, fails) in [
        // A tuple to compute it for, or none.
        (
            "key_fails",
            "call debug_value{v: r where a = 1 div zero};",
            true,
        ),
        (
            "key_no_tuple",
            "call debug_value{v: count{r: none where a = 1 div zero}};",
            false,
        ),
        // Each computed, whatever the other finds, or none.
        (
            "key_second_fails",
            "call debug_value{v: r where a = zero + 99 and z = 1 div zero};",
            true,
        ),
        (
            "key_first_fails",
            "call debug_value{v: r where a = 1 div zero and z = zero + 99};",
            true,
        ),
        // Behind a key that can't fail, only among those it finds.
        (
            "key_guarded_found",
            "call debug_value{v: r where a = 2 and z = 1 div zero};",
            true,
        ),
        (
            "key_guarded_none",
            "call debug_value{v: count{r: r where a = 99 and z = 1 div zero}};",
            false,
        ),
        // Behind what can't fail and isn't a key.
        (
            "key_behind_guard",
            "call debug_value{v: count{r: r where a = 1 div zero and z > 99}};",
            false,
        ),
        (
            "key_behind_guard_true",
            "call debug_value{v: count{r: r where a = 1 div zero and z > 0}};",
            true,
        ),
        // Beside what can fail, computed for tuples the key doesn't find.
        (
            "key_beside_failure",
            "call debug_value{v: count{r: r where a = zero + 1 and 1 div (z - 3) < 99}};",
            true,
        ),
        // The same of a `delete`.
        ("delete_key_fails", "delete r where a = 1 div zero;", true),
        (
            "delete_key_no_tuple",
            "delete none where a = 1 div zero;",
            false,
        ),
    ] {
        let (code, stderr) = run(name, line);
        if fails {
            assert_eq!(code, 1, "{line}: {stderr}");
            assert!(stderr.contains(divides), "{line}: {stderr}");
            assert!(
                stderr.contains(&format!("{name}.trendy:5:")),
                "{line}: {stderr}"
            );
        } else {
            assert_eq!(code, 0, "{line}: {stderr}");
        }
    }
}

/// `delete`, `i_delete` and `minus`, which change a variable's own buffer where
/// nothing else holds it, are the same as an operator computing what is left,
/// which never does: by the first attribute, by it and another, by another
/// alone, of tuples that are there and aren't, of integers and of texts, which
/// buffers hold references to.
const DELETES: &str = r#"operator without{r: relation { a: integer, t: text }, a_is: integer}: relation { a: integer, t: text } [
    r where not (a = a_is)
];

operator without_both{r: relation { a: integer, t: text }, a_is: integer, t_is: text}: relation { a: integer, t: text } [
    r where not (a = a_is and t = t_is)
];

operator without_text{r: relation { a: integer, t: text }, t_is: text}: relation { a: integer, t: text } [
    r where not (t = t_is)
];

operator without_those{r: relation { a: integer, t: text }, m: integer, v: integer}: relation { a: integer, t: text } [
    r where not (a mod m = v)
];

operator less{r: relation { a: integer, t: text }, gone: relation { a: integer, t: text }}: relation { a: integer, t: text } [
    r minus gone
];

operator main{} [
    var r: relation { a: integer, t: text };
    var s: relation { a: integer, t: text };
    var x := 1;
    var i := 0;
    var wrong := 0;
    var most := 0;
    while i < 6000 loop [
        x := x * 48271 mod 2147483647;
        var a := x mod 300;
        var t := to_text{from: x mod 5};
        insert r relation { { a, t }, { a: a + 1, t: to_text{from: a} } };
        s := relation { { a, t }, { a: a + 1, t: to_text{from: a} } } union s;
        most := if count{r} > most then count{r} else most;
        x := x * 48271 mod 2147483647;
        var b := x mod 300;
        var kind := x mod 6;
        if kind = 0 then [
            delete r where a = b;
            s := without{r: s, a_is: b};
        ] else if kind = 1 then [
            delete r where a = b and t = "1";
            s := without_both{r: s, a_is: b, t_is: "1"};
        ] else if kind = 2 then [
            delete r where t = to_text{from: b mod 40};
            s := without_text{r: s, t_is: to_text{from: b mod 40}};
        ] else if kind = 3 then [
            delete r where a mod 97 = b mod 97;
            s := without_those{r: s, m: 97, v: b mod 97};
        ] else if kind = 4 then [
            var gone := relation { { a: b, t: "2" }, { a: b + 1, t: "3" } };
            r := r minus gone;
            s := less{r: s, gone};
        ] else [
            var there := r where a < b mod 20;
            i_delete r there;
            s := less{r: s, gone: there};
        ];
        if r <> s then [
            wrong := wrong + 1;
        ];
        i := i + 1;
    ];
    call debug_value{v: wrong};
    call debug_value{v: most > 100};
];
"#;

#[test]
fn deletes_are_what_computing_the_rest_is() {
    let (code, stderr) = run_source("deletes", DELETES);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        ["0", "true"],
        "{stderr}"
    );
}

/// A value something else holds doesn't change when its variable's `delete`
/// does: another variable's, a parameter's, a transaction's it restores, a
/// constant's, and a tuple's, each built as the program runs where it can be,
/// as a constant is always copied. A condition sees the relation whole, and a
/// failure in it changes nothing.
const DELETES_AND_VALUES: &str = r#"var constant := relation { { n: 1 }, { n: 2 } };
var global: relation { n: integer };

operator built{}: relation { n: integer } [
    var r: relation { n: integer };
    var i := 1;
    while i <= 4 loop [
        insert r relation { { n: i * 10 } };
        i := i + 1;
    ];
    r
];

operator less{r: relation { n: integer }, gone: integer}: relation { n: integer } [
    var mine := r;
    delete mine where n = gone;
    mine
];

operator out_of{var r: relation { n: integer }, n: integer} [
    i_delete r relation { { n } };
];

operator given_then_changed{r: relation { n: integer }} [
    delete global where n = 10;
    call debug_value{v: min{r, of: n}};
];

operator main{} [
    var a := built{};
    var b := a;
    delete a where n = 10;
    call debug_value{v: a};
    call debug_value{v: b};
    /* Each again, now that each is the only holder. */
    delete b where n = 20;
    delete a where n = 30;
    call debug_value{v: a};
    call debug_value{v: b};
    /* A constant isn't changed. */
    delete constant where n = 1;
    call debug_value{v: count{r: constant}};
    var other := relation { { n: 1 }, { n: 2 } };
    var another := relation { { n: 1 }, { n: 2 } };
    delete other where n = 2;
    call debug_value{v: tuple { o: count{r: other}, a: count{r: another} }};
    /* An operator keeps its own, or changes the caller's. */
    var c := built{};
    var d := less{r: c, gone: 10};
    call debug_value{v: tuple { c: count{r: c}, d: count{r: d} }};
    call out_of{var r: c, n: 40};
    call debug_value{v: c};
    /* An argument kept while what it came from changes. */
    global := built{};
    call given_then_changed{r: global};
    call debug_value{v: count{r: global}};
    /* What is decided from the relation is decided of it as it was. */
    var e := built{};
    delete e where n = max{r: e, of: n} or n = min{r: e, of: n};
    call debug_value{v: e};
    /* With others at once, each reads it as it was. */
    var f := built{};
    var g: relation { n: integer };
    assign {
        delete f where n = 10;
        g := f;
    };
    call debug_value{v: tuple { f: count{r: f}, g: count{r: g} }};
    /* A failure in the condition changes nothing, and a transaction restores
       what was changed before it. */
    var h := built{};
    var failed: relation transaction_collection;
    var zero := 0;
    transaction collecting failed [
        delete h where n = 10;
        delete h where 1 div (n - 20 + zero) > 5;
    ];
    call debug_value{v: count{r: h}};
    transaction collecting failed [
        i_delete h relation { { n: 20 }, { n: 99 } };
    ];
    call debug_value{v: count{r: h}};
    call debug_value{v: count{r: failed}};
    /* All of them, then more into the room. */
    delete h where n > zero;
    call debug_value{v: h};
    delete h where n > zero;
    insert h relation { { n: 7 } };
    call debug_value{v: h};
    /* A relation without attributes. */
    var dee := relation { {} };
    var yes := zero = 0;
    delete dee where yes;
    call debug_value{v: dee};
    /* A tuple holds its relation and its text, as it did. */
    var inner := built{};
    var nested := relation { { r: inner, t: to_text{from: zero} }, { r: inner, t: "other" } };
    delete inner where n = 10;
    delete nested where t = "other";
    call debug_value{v: count{r: nested where min{r: r, of: n} = 10}};
    call debug_value{v: count{r: inner}};
];
"#;

#[test]
fn deletes_leave_what_else_holds_a_value_alone() {
    let (code, stderr) = run_source("deletes_and_values", DELETES_AND_VALUES);
    assert_eq!(code, 0, "{stderr}");
    let all = "relation { n: integer } { { n: 10 }, { n: 20 }, { n: 30 }, { n: 40 } }";
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            "relation { n: integer } { { n: 20 }, { n: 30 }, { n: 40 } }",
            all,
            "relation { n: integer } { { n: 20 }, { n: 40 } }",
            "relation { n: integer } { { n: 10 }, { n: 30 }, { n: 40 } }",
            "1",
            "tuple { a: 2, o: 1 }",
            "tuple { c: 4, d: 3 }",
            "relation { n: integer } { { n: 10 }, { n: 20 }, { n: 30 } }",
            "10",
            "3",
            "relation { n: integer } { { n: 20 }, { n: 30 } }",
            "tuple { f: 3, g: 4 }",
            "4",
            "4",
            "2",
            "relation { n: integer } {}",
            "relation { n: integer } { { n: 7 } }",
            "relation {} {}",
            "1",
            "3",
        ],
        "{stderr}"
    );
}

/// A failure in a `delete`'s condition, outside any transaction, and an
/// `i_delete` of what isn't there, stop the program as they did; and a key still
/// holds of what's added after a `delete`.
#[test]
fn deletes_fail_as_they_did() {
    let run = |name: &str, body: &str| {
        run_source(
            name,
            &format!(
                "operator main{{}} [\n    var r := series{{from: 1, to: 3}};\n    var zero := 0;\n{body}\n];\n"
            ),
        )
    };
    let (code, stderr) = run(
        "delete_divides",
        "    delete r where 1 div (value - 2 + zero) > 5;",
    );
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.starts_with("error[R0002]: division by zero: 1 div 0"),
        "{stderr}"
    );
    let (code, stderr) = run(
        "delete_missing",
        "    i_delete r relation { { value: 9 } };",
    );
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.starts_with("error[R0007]"), "{stderr}");
    assert!(stderr.contains("value: 9"), "{stderr}");
    let (code, stderr) = run_source(
        "delete_then_key",
        "var r: relation { k: integer, v: integer } key { k };\n\noperator main{} [\n    insert r relation { { k: 1, v: 1 }, { k: 2, v: 2 } };\n    delete r where k = 1;\n    insert r relation { { k: 1, v: 3 } };\n    call debug_value{v: count{r}};\n    insert r relation { { k: 1, v: 4 } };\n];\n",
    );
    assert_eq!(code, 1, "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines[0], "2", "{stderr}");
    assert!(lines[1].starts_with("error[R0008]"), "{stderr}");
}

/// A name alone, in each place one can be, computes what the same thing
/// written out does, when the program runs.
#[test]
fn names_alone_run_as_written_out() {
    let (code, stderr) = run_source(
        "names_alone",
        r#"uses { trendy.files };

operator area{width: integer, height: integer}: integer [ width * height ];

operator person{name: text, born: integer} [
    call debug_value{v: tuple { name, born }};
    call debug_value{v: relation { { name, born } }};
    call debug_value{v: array [{ name, born: born + 1 }]};
];

operator show{width: integer, height: integer} [
    call debug_value{v: area{width, height}};
    call debug_value{v: area{height: width, width: 5}};
    call debug_value{v: area{width, height: 7}};
];

operator bump{var n: integer, out twice: integer} [
    n := n + 1;
    twice := n * 2;
];

operator main{} [
    call person{name: "Ada", born: 1815};
    call show{width: 2, height: 3};
    var n := 1;
    var twice := 0;
    call bump{var n, out twice};
    call debug_value{v: tuple { n, twice }};
    var r := relation { { width: 2, height: 4 } };
    call debug_value{v: r extend { a := area{width, height} }};
    call debug_value{v: sum{r, of: width}};
    var from := 1;
    var to := 2;
    call debug_value{v: series{from, to}};
];
"#,
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            r#"tuple { born: 1815, name: "Ada" }"#,
            r#"relation { born: integer, name: text } { { born: 1815, name: "Ada" } }"#,
            r#"array [tuple { born: 1816, name: "Ada" }]"#,
            "6",
            "10",
            "14",
            "tuple { n: 2, twice: 4 }",
            "relation { a: integer, height: integer, width: integer } { { a: 8, height: 4, width: 2 } }",
            "2",
            "relation { value: integer } { { value: 1 }, { value: 2 } }",
        ]
    );
}

/// Constraints in operators' braces, checked of arguments of each kind: a
/// text before an integer, as offsets matter in what the failure writes, of a
/// generic operator's, a `var` parameter's value as the call starts, of a
/// specification's, whichever version is computed, and for each tuple.
const CALL_CONSTRAINTS: &str = r#"var zero := 0;

operator repeated{s: text, n: integer, constraint n >= 0, constraint s <> ""}: text [
    var r := "";
    var i := 0;
    while i < n loop [
        r := r || s;
        i := i + 1;
    ];
    r
];

operator first_of{a: array E, constraint length{a: a} > 0}: E
    types { E: any type }
[
    a[1]
];

operator last_of{var a: array integer, out last: integer, constraint length{a: a} > 0} [
    last := a[length{a: a}];
];

type figure union;
type round is figure { { r: integer } };
type square is figure { { s: integer } };

operator area{f: figure, constraint f is round}: integer;
operator area{f: round}: integer [ 3 * f.r * f.r ];
operator area{f: square}: integer [ f.s * f.s ];

operator area_of{f: figure}: integer [ area{f: f} ];

operator main{} [
    call debug_value{v: repeated{s: "ab", n: 2}};
    call debug_value{v: first_of{a: array ["x"]}};
    var a := array [1, 2];
    var last := 0;
    call last_of{var a: a, out last: last};
    call debug_value{v: last};
    call debug_value{v: relation { { s: "x", n: 1 }, { s: "y", n: 0 } } extend { t := repeated{s: s, n: n} }};
    call debug_value{v: area_of{f: round{r: 2}}};
    var failed: relation transaction_collection;
    var empty: array integer;
    transaction collecting failed [
        call last_of{var a: empty, out last: last};
    ];
    transaction collecting failed [
        last := length{a: array [first_of{a: empty}]};
    ];
    var figure_given: figure := square{s: 2};
    transaction collecting failed [
        last := area_of{f: figure_given};
    ];
    call debug_value{v: failed{error}};
    var nothing := "";
    call debug_value{v: repeated{s: nothing, n: zero}};
];
"#;

#[test]
fn calls_check_the_constraints_in_their_operators_braces() {
    let (code, stderr) = run_source("call_constraints", CALL_CONSTRAINTS);
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(code, 1, "{stderr}");
    assert_eq!(
        lines[..5],
        [
            r#""abab""#,
            r#""x""#,
            "2",
            r#"relation { n: integer, s: text, t: text } { { n: 0, s: "y", t: "" }, { n: 1, s: "x", t: "x" } }"#,
            "12",
        ],
        "{stderr}"
    );
    // A transaction takes each, as an error it can tell apart.
    assert!(
        lines[5].contains(r#"broken_call_constraint{called: "last_of{a: array integer []}", constraint: "length{a: a} > 0"}"#),
        "{stderr}"
    );
    assert!(
        lines[5].contains(r#"broken_call_constraint{called: "first_of{a: array integer []}", constraint: "length{a: a} > 0"}"#),
        "{stderr}"
    );
    assert!(
        lines[5].contains(
            r#"broken_call_constraint{called: "area{f: square{s: 2}}", constraint: "f is round"}"#
        ),
        "{stderr}"
    );
    // Written where the call is, with what the constraint reads.
    assert_eq!(
        lines[6],
        r#"error[R0024]: repeated{s: ""} can't be called: it breaks the constraint `s <> ""`"#,
        "{stderr}"
    );
    assert!(lines[7].contains("call_constraints.trendy:56:"), "{stderr}");
}

/// `shl` and `shr` computed when running are what they are when compiling,
/// which `trendy_numeric` computes: of the smallest and largest integers,
/// and of counts beyond 63 places, and negative ones, which shift the other
/// way.
#[test]
fn shifts_when_running_are_as_when_compiling() {
    let values = [0, 1, -1, 5, -9, i64::MAX, i64::MIN];
    let counts = [
        0,
        1,
        62,
        63,
        64,
        100,
        -1,
        -62,
        -63,
        -64,
        -100,
        i64::MAX,
        i64::MIN,
    ];
    let mut calls = String::new();
    let mut expected = String::new();
    for a in values {
        for n in counts {
            let shifts = [
                ("left", trendy_numeric::shifted_left(a, n)),
                ("right", trendy_numeric::shifted_right(a, n)),
            ];
            for (operator, shifted) in shifts {
                if let Some(shifted) = shifted {
                    writeln!(calls, "    call {operator}{{a: {a}, n: {n}}};").unwrap();
                    writeln!(expected, "{shifted}").unwrap();
                }
            }
        }
    }
    let source = format!(
        "operator left{{a: integer, n: integer}} [ call debug_value{{v: a shl n}}; ];\n\noperator right{{a: integer, n: integer}} [ call debug_value{{v: a shr n}}; ];\n\noperator main{{}} [\n{calls}];\n"
    );
    let (code, stderr) = run_source("shifts", &source);
    assert_eq!((code, stderr), (0, expected));
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
            // A word's code points, how many, and its last, and whether it
            // is `á`: as one, then as `a` and a combining accent; and the
            // word again.
            "6",
            "code_point{value: 225}",
            "true",
            "true",
            "7",
            "code_point{value: 769}",
            "false",
            "true",
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
            // The file's lines, saved and read back, and a file that can't
            // be written, which gives its error.
            "true",
            r#"relation { error: error } { { error: unwritable_file{path: "target/no/such/directory.txt", reason: "No such file or directory"} } }"#,
            // A CSV file's records, two skipped, whose errors `main` gives
            // as warnings.
            "3",
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
            // What `main` gives, written as the program ends: a value
            // that breaks its type's constraint, and a field that isn't a
            // value of its type.
            r#"warning[R0017]: line 3 of "examples/lakes.csv" gives `depth` metres{m: -281}, which isn't a value of `metres`: it breaks the constraint `m > 0`"#,
            r#"warning[R0014]: "eighty-four", in column 3 on line 5 of "examples/lakes.csv", isn't an integer, the type of `depth.m`"#,
        ]
    );
}

#[test]
fn code_points_of_a_subtype_when_running() {
    // Made as selecting each would make it, of a subtype a constraint
    // defines when it is true of it, and read back, wherever the
    // hierarchy keeps their components.
    let source = "type ascii is code_point { constraint value < 128 };

operator show{word: text} [
    var points := to_code_points{from: word};
    for p in points loop [
        call debug_value{v: p is ascii};
    ];
    call debug_value{v: from_code_points{from: points}};
];

operator main{} [
    call show{word: \"aé\"};
];
";
    assert_eq!(
        run_source("code_points_of_a_subtype", source),
        (0, "true\nfalse\n\"aé\"\n".to_string())
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
        "uses {{ trendy.files }};

operator main{{}} [
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
        "uses {{ trendy.files }};

{types}
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
    let site = site("csv_stops_broken", 12);
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
    let site = self::site("csv_stops_unnamed", 12);
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
                "error[{}]: \"south\", in column 3 on line 2 of \"PATH\", isn't an approximate number, the type of `stop.at.latitude`",
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
fn databases() {
    // A database's definition, its relvars and constraints, which nothing
    // reads yet.
    assert_eq!(run_example("databases"), (0, String::new()));
}

#[test]
fn units() {
    // The deepest lake's name, by the program's own `deepest`, which calls
    // the one it hides, and the lakes shallower than 500 metres, given in
    // centimetres, a representation whose conversions only `lakes.depths`
    // sees, as the program's own `lake`, which hides that unit's.
    let (code, stderr) = run_example("units");
    assert_eq!(
        (code, stderr.as_str()),
        (
            0,
            "3\n\"Baikal\"\nrelation { name: text, shallow: boolean } { { name: \"Titicaca\", shallow: true } }\n"
        )
    );
}

#[test]
fn constraints() {
    // Quechua, added with where it is official at once, which the
    // constraints the initial values satisfy hold of too; and Esperanto,
    // official nowhere, which a transaction refuses, with the error.
    let (code, stderr) = run_example("constraints");
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            "┌──────────┬──────────┬───────────┐",
            "│ country  │ language │ native    │",
            "├──────────┼──────────┼───────────┤",
            "│ Bolivia  │ Quechua  │ Runa Simi │",
            "│ India    │ Hindi    │ हिन्दी     │",
            "│ Kenya    │ Swahili  │ Kiswahili │",
            "│ Peru     │ Quechua  │ Runa Simi │",
            "│ Tanzania │ Swahili  │ Kiswahili │",
            "└──────────┴──────────┴───────────┘",
            "3",
            r#""the constraint `official_somewhere` is broken: `languages{language} <= official{language}` is false of relation { language: text } { { language: \"Esperanto\" } }""#,
        ]
    );
}

#[test]
fn a_statement_that_breaks_a_constraint_fails() {
    // Checked as each statement that assigns a variable it mentions ends,
    // whether it assigns or passes it to a `var` parameter: the first
    // part false, with the tuples that make it false where its form says.
    let source = "var countries := relation { { country: \"Kenya\" } } key { country };\nvar official := relation { { language: \"Swahili\", country: \"Kenya\" } } key { language, country };\nvar stock := 2;\n\nconstraint known_countries := official{country} <= countries;\nconstraint stocked := stock > 0 and 10 div stock > 1;\n\noperator halve{var n: integer} [\n    n := n div 2;\n];\n\noperator main{} [\n    call halve{var n: stock};\n    call debug_value{v: stock};\n    insert official relation { { language: \"Hindi\", country: \"India\" } };\n];\n";
    let (code, stderr) = run_source("constraint_broken", source);
    assert_eq!(code, 1, "{stderr}");
    assert_eq!(
        stderr,
        format!(
            "1\nerror[R0023]: the constraint `known_countries` is broken: `official{{country}} <= countries` is false of relation {{ country: text }} {{ {{ country: \"India\" }} }}\n{}",
            site("constraint_broken", 15)
        )
    );
    let (code, stderr) = run_source(
        "constraint_broken_by_var",
        &source.replace("call debug_value{v: stock};", "call halve{var n: stock};"),
    );
    assert_eq!(code, 1, "{stderr}");
    assert_eq!(
        stderr,
        format!(
            "error[R0023]: the constraint `stocked` is broken: `stock > 0` is false\n{}",
            site("constraint_broken_by_var", 14)
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
                "{moved}{table}{day}{table}warning[{}]: Cusco has 35 sacks left\n --> transactions.trendy:56:9\n{after}relation {{ failed: boolean, message: text }} {{ {{ failed: true, message: \"division by zero: 40 div 0 has no result\" }}, {{ failed: true, message: \"two tuples of `granaries` have the same values for its key {{ city }}: tuple {{ city: \\\"Dhaka\\\", native: \\\"ঢাকা\\\", sacks: 0 }} and tuple {{ city: \\\"Dhaka\\\", native: \\\"ঢাকা\\\", sacks: 30 }}\" }} }}\n",
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

/// Each of the language's other failures is an error a transaction takes
/// too, its message as the failure wrote it.
#[test]
fn every_failure_of_the_programs_values_is_an_error() {
    let source = "type outline {
    { vertices: relation { v_no: integer, x: integer } key { v_no } },
};

type percentage {
    { value: integer },
    constraint value >= 0 and value <= 100,
};

type ellipse { { a: approximate, b: approximate }, constraint a >= b };
type circle is ellipse { constraint a = b };
type small is ellipse { constraint a < 1e0 };

type plane_figure union;
type oval is plane_figure { { width: approximate } };
type polygon is plane_figure { { corners: integer } };

var guests := relation { { room: 1, guest: \"Kenji\" } } key { room };
var grid := array [array [1, 2], array [3, 4]];

operator main{} [
    var found: relation transaction_collection;
    var one := 1;
    var name := \"Ada\";
    transaction collecting found [
        call debug_value{v: tuple { name: name, born: 1815 } join tuple { name: name || \" Lovelace\" }};
    ];
    transaction collecting found [
        call debug_value{v: relation { { n: one } } d_union relation { { n: 1 } }};
    ];
    transaction collecting found [
        call debug_value{v: relation { { n: one } } i_minus relation { { n: 2 } }};
    ];
    transaction collecting found [
        insert guests relation { { room: one, guest: \"Wanjiru\" } };
    ];
    transaction collecting found [
        call debug_value{v: outline{vertices: relation { { v_no: 1, x: 0 }, { v_no: one, x: 4 } }}};
    ];
    transaction collecting found [
        assign {
            update guests { guest := \"Wanjiru\" } where room = one;
            delete guests where guest = \"Kenji\";
        };
    ];
    transaction collecting found [
        assign {
            grid[one + 1][one] := 0;
            grid[2][1] := 9;
        };
    ];
    transaction collecting found [
        call debug_value{v: min{r: relation { { n: one } } where n > 1, of: n}};
    ];
    transaction collecting found [
        call debug_value{v: parse_integer{from: name}};
    ];
    transaction collecting found [
        call debug_value{v: percentage{value: one + 119}};
    ];
    transaction collecting found [
        call debug_value{v: sqrt{of: to_approximate{from: -one}}};
    ];
    transaction collecting found [
        var figure: plane_figure := polygon{corners: one + 3};
        call debug_value{v: treat figure as oval};
    ];
    transaction collecting found [
        call debug_value{v: ellipse{a: to_approximate{from: one} / 2e0, b: 5e-1}};
    ];
    call debug_value{v: (found extend { message := message{error: error} }){message}};
];
";
    let messages = [
        "\\\"Ada\\\" isn't an integer",
        "-1e0 has no square root",
        "d_union or d_insert adds a tuple that is already there: tuple { n: 1 }",
        "ellipse{a: 5e-1, b: 5e-1} is of both `circle` and `small`, but a value has one most specific type",
        "i_minus or i_delete removes a tuple that isn't there: tuple { n: 2 }",
        "percentage{value: 120} isn't a value of `percentage`: it breaks the constraint `value >= 0 and value <= 100`",
        "polygon{corners: 4} isn't a value of `oval`",
        "the relation has no tuples, so it has no min",
        "the tuples disagree on `name`, so they have no join: `name` is \\\"Ada\\\" in one and \\\"Ada Lovelace\\\" in the other",
        "two changes in an assign block remove the same tuple: tuple { guest: \\\"Kenji\\\", room: 1 }",
        "two changes in an assign block set the same element, at [2][1]",
        "two tuples of `guests` have the same values for its key { room }: tuple { guest: \\\"Kenji\\\", room: 1 } and tuple { guest: \\\"Wanjiru\\\", room: 1 }",
        "two tuples of `outline`'s `vertices` have the same values for its key { v_no }: tuple { v_no: 1, x: 0 } and tuple { v_no: 1, x: 4 }",
    ];
    let tuples: Vec<String> = messages
        .iter()
        .map(|message| format!("{{ message: \"{message}\" }}"))
        .collect();
    assert_eq!(
        run_source("every_failure", source),
        (
            0,
            format!("relation {{ message: text }} {{ {} }}\n", tuples.join(", "))
        )
    );
}

/// `main` can give errors, which the program writes as it ends, the
/// warnings, then the failures, with no site, and it exits with 1 if one
/// failed, as a failure nothing takes does, and else with 0.
#[test]
fn main_gives_the_errors_the_program_writes() {
    let program = |collected: &str| {
        format!(
            "type running_low is error {{ {{ city: text, sacks: integer }} }};

operator message{{error: running_low}}: text [
    error.city || \" has \" || to_text{{from: error.sacks}} || \" sacks left\"
];

operator main{{out errors: relation transaction_collection}} [
    var sacks := 0;
    var found: relation transaction_collection;
    transaction collecting found [
        warn running_low{{city: \"Mombasa\", sacks: sacks}};
        {collected}
    ];
    call debug_value{{v: \"done\"}};
    errors := found;
];
"
        )
    };
    let warned = format!(
        "warning[{}]: Mombasa has 0 sacks left\n",
        trendy_diag::Code::Failed.id()
    );
    assert_eq!(
        run_source(
            "main_failed",
            &program("call debug_value{v: 12 div sacks};")
        ),
        (1, format!("\"done\"\n{warned}{}\n", division("12 div 0")))
    );
    assert_eq!(
        run_source("main_warned", &program("")),
        (0, format!("\"done\"\n{warned}"))
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
        "uses {{ trendy.files }};

operator show{{path: text}} [
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
fn a_programs_type_of_an_errors_name_leaves_the_languages_error() {
    // A program's own `unreadable_file`, and `trendy.files`'s, which a unit
    // of it reads with: each called by its unit's name, and a load gives
    // the language's.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("own_error");
    std::fs::create_dir_all(&dir).unwrap();
    let main = dir.join("main.trendy");
    std::fs::write(
        &main,
        "uses { reader };

type unreadable_file { { why: text } };

operator main{} [
    call debug_value{v: unreadable_file{why: \"mine\"}};
    call read{path: \"missing.txt\"};
];
",
    )
    .unwrap();
    std::fs::write(
        dir.join("reader.trendy"),
        "interface

operator read{path: text};

implementation

uses { trendy.files };

operator read{path: text} [
    var lines: array text;
    var errors: relation { error: error };
    call load_text_file{path: path, out lines: lines, out errors: errors};
    call debug_value{v: errors};
];
",
    )
    .unwrap();
    let (code, stderr) = build_and_run(&main, &dir.join("main"), &dir);
    assert_eq!(
        (code, stderr),
        (
            0,
            "main::unreadable_file{why: \"mine\"}
relation { error: error } { { error: trendy.files::unreadable_file{path: \"missing.txt\", reason: \"No such file or directory\"} } }
"
            .to_string()
        )
    );
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
        "uses {{ trendy.files }};

heading city := {{ city: text, people: integer }};

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
        "uses {{ trendy.files }};

type shape {{ {{ w: integer, h: integer }}, constraint w > 0 }};
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
error[{overlapping}]: ellipse{{a: 5e-1, b: 5e-1}} is of both `circle` and `small`, but a value has one most specific type
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
        "uses {{ trendy.files }};

{types}
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
error[{violated}]: two tuples of `seg`'s `ends` have the same values for its key {{ x }}: tuple {{ n: 1, x: 0 }} and tuple {{ n: 2, x: 0 }}
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
                "error[{violated}]: two tuples of `outline`'s `vertices` have the same values for its key {{ v_no }}: tuple {{ v_no: 1, x: 0 }} and tuple {{ v_no: 1, x: 5 }}
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
                site("csv_segments", 16)
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
    let site = site("csv_lakes_broken", 10);
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
fn a_representations_own_constraints_when_running() {
    // Checked of the components given to its selector, before its `to`,
    // and of those an `update` in it gives, as the type's are of a value.
    let source = "type tens {
    { n: integer },
    { big: integer, constraint big mod 10 = 0 }
        to { n := big div 10 }
        from { big := n * 10 },
};

operator show{given: integer} [
    var t := tens{n: 1};
    update t { n := 2 };
    call debug_value{v: t.big};
    update t { big := given };
    call debug_value{v: t};
];

operator main{} [
    call show{given: 30};
    call show{given: 15};
];
";
    let (code, stderr) = run_source("representation_constraints", source);
    assert_eq!(code, 1, "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines[..4], ["20", "tens{n: 3}", "20", lines[3]]);
    assert!(
        lines[3].starts_with(&format!(
            "error[{}]: tens{{big: 15}} isn't a value of `tens`: it breaks the constraint `big mod 10 = 0`",
            trendy_diag::Code::BrokenConstraint.id()
        )),
        "{stderr}"
    );
    // So too as `load_csv` reads one in it, of the record's components,
    // writing its line, or skipping it, with its error, if the call gives
    // errors.
    let types = "type tens {
    { n: integer },
    { big: integer, constraint big mod 10 = 0 }
        to { n := big div 10 }
        from { big := n * 10 },
};
";
    let (code, stderr) = loaded_rows(
        "tens",
        types,
        "{ name: text, t: tens }",
        "name,big\nRome,20\nOslo,15\n",
        "tuple { name: 1, t: tuple { big: 2 } }",
    );
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.starts_with(&format!(
            "error[{}]: line 3 of \"PATH\" gives `t` tens{{big: 15}}, which isn't a value of `tens`: it breaks the constraint `big mod 10 = 0`",
            trendy_diag::Code::BrokenConstraint.id()
        )),
        "{stderr}"
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
    let site = site("csv_points_negative", 19);
    assert_eq!(
        loaded("points_negative", "1,2,0\n2,-1,0\n"),
        (
            1,
            format!(
                "error[{broken}]: line 3 of \"PATH\" gives `at.r` distance{{m: -1e3}}, which isn't a value of `distance`: it breaks the constraint `m >= 0e0`\n{site}"
            )
        )
    );
    let site = self::site("csv_points_far", 19);
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
                "error[{}]: \"far\", in column 2 on line 2 of \"PATH\", isn't an approximate number, the type of `at.r.km`",
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
    let at = site(&format!("csv_{name}"), 6);
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
            r#"" 10883000", in column 5 on line 2 of "PATH", isn't an integer, the type of `people`"#
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
fn what_the_work_on_each_tuple_makes_is_freed_but_what_it_gives() {
    // Each tuple's work, and each call's, in a scope of its own: what they
    // give, the tuples an `extend` builds and the values calls return,
    // outlives them, through calls that make relations of texts. A local
    // variable's value is freed after its last use, but what a later one,
    // or the body's value, still points into.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("tuple_scopes_input");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("lines.txt");
    std::fs::write(&path, "a\nb\n").unwrap();
    let source = format!(
        "uses {{ trendy.files }};\n\noperator shout{{t: text}}: text [\n    var loud := t || \"!\";\n    var same := loud;\n    same\n];\n\noperator echoes{{t: text}}: relation {{ e: text }} [\n    var first := shout{{t: t}} || \"1\";\n    var both := relation {{ {{ e: first }}, {{ e: t || \"2\" }} }};\n    var unused := both union both;\n    both\n];\n\noperator main{{}} [\n    var lines: array text;\n    call load_text_file{{path: {:?}, out lines: lines}};\n    var listed: relation {{ n: integer, line: text }};\n    load listed from lines as line numbered n;\n    call debug_value{{v: listed extend {{ loud := shout{{t: line}} || \"?\" }}}};\n    call debug_value{{v: (listed where shout{{t: line}} = \"b!\"){{n}}}};\n    call debug_value{{v: (listed extend {{ heard := echoes{{t: line}} }}){{n, heard}}}};\n];\n",
        path.to_str().unwrap()
    );
    let (code, stderr) = run_source("tuple_scopes", &source);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr,
        concat!(
            "relation { line: text, loud: text, n: integer } { { line: \"a\", loud: \"a!?\", n: 1 }, { line: \"b\", loud: \"b!?\", n: 2 } }\n",
            "relation { n: integer } { { n: 2 } }\n",
            "relation { heard: relation { e: text }, n: integer } { { heard: relation { e: text } { { e: \"a!1\" }, { e: \"a2\" } }, n: 1 }, { heard: relation { e: text } { { e: \"b!1\" }, { e: \"b2\" } }, n: 2 } }\n",
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
            "uses {{ trendy.files }};\n\noperator main{{}} [\n    var lines: array text;\n    call load_text_file{{path: {}, out lines: lines}};\n    call debug_value{{v: lines}};\n];\n",
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
                site("load_text_file_missing", 5)
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
                site("load_text_file_latin1", 5)
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
            "uses {{ trendy.files }};

operator main{{}} [
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

/// The file a test of `save_text_file` writes, in a directory of those tests',
/// with what it holds to start with, if anything.
fn saved_file(name: &str, holds: Option<&str>) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("saved");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    match holds {
        Some(holds) => std::fs::write(&path, holds).unwrap(),
        None => drop(std::fs::remove_file(&path)),
    }
    path
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
        (
            "shift_left",
            "a shl 1",
            overflow("9223372036854775807 shl 1"),
        ),
        (
            "shift_right_by_negative",
            "a shr (b - 1)",
            overflow("9223372036854775807 shr (-1)"),
        ),
        (
            "shift_far",
            "(b + 1) shl a",
            overflow("1 shl 9223372036854775807"),
        ),
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
            // A duration selected in hours and minutes, which its own
            // constraint checks, stored in seconds, its minutes read back,
            // and changed in that representation.
            "duration{seconds: 27900}",
            "45",
            "duration{seconds: 26100}",
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
            // A name alone is its own value.
            r#"tuple { nation: "Peru" }"#,
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
    // Every path downstream; what reaches the Nile, from three sources;
    // and the stations on the loop, without Nakano, off it.
    "┌───────────────┬───────────────┐",
    "│ from          │ to            │",
    "├───────────────┼───────────────┤",
    "│ Atbara        │ Nile          │",
    "│ Blue Nile     │ Nile          │",
    "│ Kagera        │ Lake Victoria │",
    "│ Kagera        │ Nile          │",
    "│ Kagera        │ White Nile    │",
    "│ Lake Victoria │ Nile          │",
    "│ Lake Victoria │ White Nile    │",
    "│ White Nile    │ Nile          │",
    "└───────────────┴───────────────┘",
    "┌───────────────┬──────────────┐",
    "│ name          │ native       │",
    "├───────────────┼──────────────┤",
    "│ Atbara        │ عطبرة        │",
    "│ Blue Nile     │ ዓባይ          │",
    "│ Kagera        │ Kagera       │",
    "│ Lake Victoria │ Nalubaale    │",
    "│ White Nile    │ النيل الأبيض │",
    "└───────────────┴──────────────┘",
    "┌───────────┬────────┐",
    "│ name      │ native │",
    "├───────────┼────────┤",
    "│ Ikebukuro │ 池袋   │",
    "│ Shibuya   │ 渋谷   │",
    "│ Shinagawa │ 品川   │",
    "│ Shinjuku  │ 新宿   │",
    "│ Tokyo     │ 東京   │",
    "│ Ueno      │ 上野   │",
    "└───────────┴────────┘",
];

#[test]
fn relations() {
    assert_eq!(folded_and_run("relations"), RELATIONS);
}

/// A program whose image relations, of relations known only when it runs,
/// are lookups, each loop's made once (`FunctionBuilder::looked_up`).
const LOOKUPS: &str = "operator show{r: relation { a: integer }, t: relation { a: integer, b: integer }, q: relation { x: integer }, z: integer} [
    /* Two images of `t`, which share a lookup, one of them empty. */
    call debug_table{r: r extend { n := count{r: !!t}, total := sum{r: !!t, of: b} }};
    call debug_table{r: r where count{r: !!t} = 1};
    /* The inner loop's relation reads the outer tuple, so it is looked up in anew for each. */
    call debug_table{r: q extend { per := sum{r: r extend { k := count{r: !!(t where b > x)} }, of: k} }};
    /* One bound in the loop may differ for each tuple: no lookup. */
    call debug_table{r: r extend { n := [ var u := t where b > 15; count{r: !!u} ] }};
    call debug_table{r: relation { { a: 1, k: 0 }, { a: 1, k: 15 } } extend { n := [ var u := t where b > k; count{r: !!u} ] }};
    call debug_table{r: relation { { a: 1, k: 0 }, { a: 1, k: 15 } } extend { n := [ var u := t; if k > 0 then [ u := t where b > k; ]; count{r: !!u} ] }};
    /* Computed only where it was: never, here, so nothing divides by zero. */
    call debug_table{r: (r where a > 100) extend { n := count{r: !!(t where 1 div z = 1)} }};
    call debug_table{r: r where a > 100 and count{r: !!(t where 1 div z = 1)} > 0};
    /* A failure after the lookup is made undoes it, and the loop makes it again. */
    var tries := 0;
    var refused: relation transaction_collection;
    while tries < 3 loop [
        transaction collecting refused [
            call debug_table{r: r extend { n := count{r: !!t}, p := (10 * a + tries) div (a - 2 + tries) }};
        ];
        tries := tries + 1;
    ];
    call debug_value{v: count{r: refused}};
    /* And one the program fails with gives it up too. */
    call debug_table{r: r extend { n := count{r: !!t}, p := a div (a - 2) }};
];

operator main{} [
    call show{
        r: relation { { a: 1 }, { a: 2 }, { a: 3 } },
        t: relation { { a: 1, b: 10 }, { a: 1, b: 20 }, { a: 2, b: 30 } },
        q: relation { { x: 0 }, { x: 15 } },
        z: 0,
    };
];
";

#[test]
fn image_relations_are_looked_up_once_per_loop() {
    let (code, stderr) = run_source("lookups", LOOKUPS);
    let expected = [
        "┌───┬───┬───────┐",
        "│ a │ n │ total │",
        "├───┼───┼───────┤",
        "│ 1 │ 2 │    30 │",
        "│ 2 │ 1 │    30 │",
        "│ 3 │ 0 │     0 │",
        "└───┴───┴───────┘",
        "┌───┐",
        "│ a │",
        "├───┤",
        "│ 2 │",
        "└───┘",
        "┌─────┬────┐",
        "│ per │ x  │",
        "├─────┼────┤",
        "│   2 │ 15 │",
        "│   3 │  0 │",
        "└─────┴────┘",
        "┌───┬───┐",
        "│ a │ n │",
        "├───┼───┤",
        "│ 1 │ 1 │",
        "│ 2 │ 1 │",
        "│ 3 │ 0 │",
        "└───┴───┘",
        "┌───┬────┬───┐",
        "│ a │ k  │ n │",
        "├───┼────┼───┤",
        "│ 1 │  0 │ 2 │",
        "│ 1 │ 15 │ 1 │",
        "└───┴────┴───┘",
        "┌───┬────┬───┐",
        "│ a │ k  │ n │",
        "├───┼────┼───┤",
        "│ 1 │  0 │ 2 │",
        "│ 1 │ 15 │ 1 │",
        "└───┴────┴───┘",
        "┌───┬───┐",
        "│ a │ n │",
        "├───┼───┤",
        "└───┴───┘",
        "┌───┐",
        "│ a │",
        "├───┤",
        "└───┘",
        "┌───┬───┬────┐",
        "│ a │ n │ p  │",
        "├───┼───┼────┤",
        "│ 1 │ 2 │ 12 │",
        "│ 2 │ 1 │ 11 │",
        "│ 3 │ 0 │ 10 │",
        "└───┴───┴────┘",
        "2",
    ];
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        (code, &lines[..expected.len()]),
        (1, &expected[..]),
        "{stderr}"
    );
    assert_eq!(
        lines[expected.len()..],
        [
            &division("2 div 0")[..],
            site("lookups", 25).trim_end(),
            called("lookups", "show", 29).trim_end(),
        ],
        "{stderr}"
    );
}

/// Products when running, made in canonical order where the operands'
/// attributes don't interleave, and sorted where they do (the runtime's
/// `product`), of what's still held once the operands are given up.
#[test]
fn products_are_in_canonical_order() {
    let source = r#"operator show{
    u: relation { a: integer },
    t: relation { b: text },
    pair: relation { a: integer, c: integer },
    z: relation { z: integer },
    x: integer,
} [
    /* All of one operand's attributes before the other's, whichever it is. */
    call debug_value{v: u times t};
    call debug_value{v: t times u};
    /* A chain written against name order. */
    call debug_value{v: times{z, t, u}};
    /* Interleaved: `b` comes between `pair`'s attributes. */
    call debug_value{v: pair times t};
    call debug_value{v: t times pair};
    /* An empty operand, either way. */
    call debug_value{v: u times (t where b = "none")};
    call debug_value{v: (t where b = "none") times u};
    /* An operand without attributes: one tuple, and none. */
    call debug_value{v: u times relation {} { {} }};
    call debug_value{v: relation {} { {} } times u};
    call debug_value{v: u times (relation {} { {} } where x > 100)};
    call debug_value{v: relation {} { {} } times (relation {} { {} } where x < 100)};
    /* What it points to outlasts its operands, which are given up here. */
    var kept := [
        var l := relation { { a: "left " || to_text{from: x} } };
        var r := relation { { b: "right " || to_text{from: x} } };
        l times r
    ];
    var interleaved := [
        var l := relation { { a: "left " || to_text{from: x}, c: "far " || to_text{from: x} } };
        var r := relation { { b: "right " || to_text{from: x} } };
        l times r
    ];
    call debug_value{v: kept};
    call debug_value{v: interleaved};
];

operator main{} [
    call show{
        u: relation { { a: 2 }, { a: 1 } },
        t: relation { { b: "y" }, { b: "x" } },
        pair: relation { { a: 1, c: 9 }, { a: 1, c: 8 }, { a: 0, c: 7 } },
        z: relation { { z: 5 } },
        x: 3,
    };
];
"#;
    let (code, stderr) = run_source("products", source);
    let two_by_two = r#"relation { a: integer, b: text } { { a: 1, b: "x" }, { a: 1, b: "y" }, { a: 2, b: "x" }, { a: 2, b: "y" } }"#;
    let interleaved = r#"relation { a: integer, b: text, c: integer } { { a: 0, b: "x", c: 7 }, { a: 0, b: "y", c: 7 }, { a: 1, b: "x", c: 8 }, { a: 1, b: "x", c: 9 }, { a: 1, b: "y", c: 8 }, { a: 1, b: "y", c: 9 } }"#;
    let expected = [
        two_by_two,
        two_by_two,
        r#"relation { a: integer, b: text, z: integer } { { a: 1, b: "x", z: 5 }, { a: 1, b: "y", z: 5 }, { a: 2, b: "x", z: 5 }, { a: 2, b: "y", z: 5 } }"#,
        interleaved,
        interleaved,
        "relation { a: integer, b: text } {}",
        "relation { a: integer, b: text } {}",
        "relation { a: integer } { { a: 1 }, { a: 2 } }",
        "relation { a: integer } { { a: 1 }, { a: 2 } }",
        "relation { a: integer } {}",
        "relation {} { {} }",
        r#"relation { a: text, b: text } { { a: "left 3", b: "right 3" } }"#,
        r#"relation { a: text, b: text, c: text } { { a: "left 3", b: "right 3", c: "far 3" } }"#,
    ];
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!((code, &lines[..]), (0, &expected[..]), "{stderr}");
}

/// A program whose restrictions by the first attributes in name order look
/// at only the tuples they may keep, when running
/// (`FunctionBuilder::equal_runs`), or at every tuple.
const EQUAL_RUNS: &str = r#"type city { { name: text } };
type figure union;
type round is figure { { r: integer } };
type square is figure { { s: integer } };

operator show{
    r: relation { a: integer, b: text, c: integer },
    q: relation { k: integer },
    named: relation { name: text, people: integer },
    places: relation { at: city, n: integer },
    figures: relation { f: figure, n: integer },
    x: integer,
    below: integer,
    between: integer,
    above: integer,
    t: text,
    z: integer,
} [
    /* The first attribute, written on either side. */
    call debug_table{r: r where a = x};
    call debug_table{r: r where x = a};
    /* Not there: below its values, between them, and above them. */
    call debug_value{v: count{r: r where a = below}};
    call debug_value{v: count{r: r where a = between}};
    call debug_value{v: count{r: r where a = above}};
    /* The first two, in either order. */
    call debug_table{r: r where b = t and a = x};
    /* The first and the third: only the first narrows. */
    call debug_table{r: r where c = x and a = x};
    /* Only the second, for which every tuple is looked at. */
    call debug_table{r: r where b = t};
    /* The others are computed only for the tuples of the run: another
       tuple's `c` of 0 divides nothing. */
    call debug_table{r: r where 10 div c > 1 and a = x};
    /* From a tuple further out. */
    call debug_table{r: q extend { n := count{r: r where a = k} }};
    /* Text, and a type of the program's own, come first too. */
    call debug_table{r: named where name = t};
    call debug_table{r: places where at = city{name: t}};
    /* A subtype's value isn't of the attribute's type: all are looked at. */
    call debug_table{r: figures where f = round{r: x}};
    /* A value that can fail is computed only if a tuple needs it. */
    call debug_value{v: count{r: (r where a = below) where a = 1 div z}};
    call debug_value{v: count{r: r where a = 1 div z}};
];

operator main{} [
    call show{
        r: relation {
            { a: 1, b: "no", c: 0 },
            { a: 3, b: "no", c: 3 },
            { a: 3, b: "yes", c: 5 },
            { a: 5, b: "yes", c: 0 },
        },
        q: relation { { k: 1 }, { k: 2 }, { k: 3 } },
        named: relation { { name: "Oslo", people: 1 }, { name: "yes", people: 2 } },
        places: relation { { at: city{name: "Oslo"}, n: 1 }, { at: city{name: "yes"}, n: 2 } },
        figures: relation { { f: round{r: 3}, n: 1 }, { f: square{s: 3}, n: 2 } },
        x: 3,
        below: 0,
        between: 2,
        above: 9,
        t: "yes",
        z: 0,
    };
];
"#;

#[test]
fn a_restriction_by_its_first_attributes_finds_their_run() {
    let (code, stderr) = run_source("equal_runs", EQUAL_RUNS);
    let run_of_three = [
        "┌───┬─────┬───┐",
        "│ a │ b   │ c │",
        "├───┼─────┼───┤",
        "│ 3 │ no  │ 3 │",
        "│ 3 │ yes │ 5 │",
        "└───┴─────┴───┘",
    ];
    let expected = [
        &run_of_three[..],
        &run_of_three[..],
        &["0", "0", "0"],
        &[
            "┌───┬─────┬───┐",
            "│ a │ b   │ c │",
            "├───┼─────┼───┤",
            "│ 3 │ yes │ 5 │",
            "└───┴─────┴───┘",
            "┌───┬────┬───┐",
            "│ a │ b  │ c │",
            "├───┼────┼───┤",
            "│ 3 │ no │ 3 │",
            "└───┴────┴───┘",
            "┌───┬─────┬───┐",
            "│ a │ b   │ c │",
            "├───┼─────┼───┤",
            "│ 3 │ yes │ 5 │",
            "│ 5 │ yes │ 0 │",
            "└───┴─────┴───┘",
        ],
        &run_of_three[..],
        &[
            "┌───┬───┐",
            "│ k │ n │",
            "├───┼───┤",
            "│ 1 │ 1 │",
            "│ 2 │ 0 │",
            "│ 3 │ 2 │",
            "└───┴───┘",
            "┌──────┬────────┐",
            "│ name │ people │",
            "├──────┼────────┤",
            "│ yes  │      2 │",
            "└──────┴────────┘",
            "┌───────────────────┬───┐",
            "│ at                │ n │",
            "├───────────────────┼───┤",
            "│ city{name: \"yes\"} │ 2 │",
            "└───────────────────┴───┘",
            "┌─────────────┬───┐",
            "│ f           │ n │",
            "├─────────────┼───┤",
            "│ round{r: 3} │ 1 │",
            "└─────────────┴───┘",
            "0",
        ],
    ]
    .concat();
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        (code, &lines[..expected.len()]),
        (1, &expected[..]),
        "{stderr}"
    );
    assert_eq!(
        lines[expected.len()..][0],
        division("1 div 0"),
        "a value that can fail is computed for the first tuple: {stderr}"
    );
}

/// Read-only operators whose statements are run, in every place a call of
/// one can be, of what's given back after the local variables holding it are
/// given up.
const READ_ONLY_STATEMENTS: &str = r#"/* A constraint that runs statements. */
operator is_even{n: integer}: boolean [
    var i := n;
    while i >= 2 loop [
        i := i - 2;
    ];
    i = 0
];

type even { { n: integer }, constraint is_even{n: n} };

/* Generic: the larger of a relation's values, by a loop over an array. */
operator largest{r: relation { x: T }, start: T}: T types { T: any ordered } [
    var values: array T;
    load values value from r order [asc x] keep {x};
    var best := start;
    for v in values loop [
        if v > best then [
            best := v;
        ];
    ];
    best
];

/* Versions of a specification. */
type figure union;
type round is figure { { r: integer } };
type square is figure { { s: integer } };

operator sides{f: figure}: integer;

operator sides{f: round}: integer [ 0 ];

operator sides{f: square}: integer [
    var n := 0;
    while n < 4 loop [
        n := n + 1;
    ];
    n
];

/* A text it builds, handed to its caller. */
operator repeated{t: text, n: integer}: text [
    var out := "";
    var i := 0;
    loop [
        out := out || t;
        i := i + 1;
    ] while i < n;
    out
];

/* A tuple holding text, and a relation holding text, from locals. */
operator named{n: integer}: tuple { name: text, seen: relation { t: text } } [
    var seen: relation { t: text };
    var i := 0;
    while i < n loop [
        insert seen relation { { t: "t" || to_text{from: i} } };
        i := i + 1;
    ];
    tuple { name: "n" || to_text{from: n}, seen: seen }
];

/* A transaction of its own, adding what fails to a local. */
operator divided{a: integer, b: integer}: integer [
    var q := -1;
    var refused: relation transaction_collection;
    transaction collecting refused [
        q := a div b;
    ];
    q
];

/* An `update`, `delete` and a local of a program type. */
operator kept{r: relation { x: integer }}: relation { x: integer, y: integer } [
    var out := r extend { y := x * 2 };
    delete out where x = 2;
    update out { y := if x = 3 then 0 else y };
    out
];

/* A `fail` statement, and its failure caught by the caller. */
operator positive{n: integer}: integer [
    if n <= 0 then [
        fail failure{message: "not positive: " || to_text{from: n}};
    ];
    n
];

/* Called from a straight-line one, which isn't computed when compiling. */
operator twice_even{n: integer}: boolean [
    var a := is_even{n: n};
    a and is_even{n: n * 2}
];

operator main{} [
    var zero := parse_integer{from: "0"};
    /* Known arguments, and arguments known only now. */
    call debug_value{v: is_even{n: 4}};
    call debug_value{v: is_even{n: 3 + zero}};
    call debug_value{v: twice_even{n: 3}};
    /* Selected, checking the constraint when running. */
    call debug_value{v: even{n: 4}};
    call debug_value{v: largest{r: relation { { x: 3 }, { x: 9 }, { x: 5 } }, start: 0}};
    call debug_value{v: largest{r: relation { { x: "b" }, { x: "c" } }, start: "a"}};
    call debug_value{v: sides{f: square{s: 2}}};
    call debug_value{v: sides{f: round{r: 2}}};
    call debug_value{v: repeated{t: "ab", n: 3}};
    var kept_name := named{n: 2};
    call debug_value{v: kept_name};
    call debug_value{v: divided{a: 7, b: 2}};
    call debug_value{v: divided{a: 7, b: zero}};
    call debug_value{v: kept{r: relation { { x: 1 }, { x: 2 }, { x: 3 } }}};
    /* For each tuple. */
    var r := relation { { x: 1 }, { x: 2 }, { x: 3 } };
    call debug_value{v: r extend { s := repeated{t: "z", n: x} }};
    call debug_value{v: r where is_even{n: x}};
    /* Its failure goes to the caller's transaction. */
    var refused: relation transaction_collection;
    var got := 0;
    transaction collecting refused [
        got := positive{n: zero};
    ];
    call debug_value{v: got};
    call debug_value{v: count{r: refused}};
    call debug_value{v: positive{n: 5}};
    /* A selection that breaks it, with no transaction: the program fails. */
    call debug_value{v: even{n: 3 + zero}};
];
"#;

#[test]
fn read_only_operators_run_statements() {
    let (code, stderr) = run_source("read_only_statements", READ_ONLY_STATEMENTS);
    let expected = [
        "true",
        "false",
        "false",
        "even{n: 4}",
        "9",
        r#""c""#,
        "4",
        "0",
        r#""ababab""#,
        r#"tuple { name: "n2", seen: relation { t: text } { { t: "t0" }, { t: "t1" } } }"#,
        "3",
        "-1",
        "relation { x: integer, y: integer } { { x: 1, y: 2 }, { x: 3, y: 0 } }",
        r#"relation { s: text, x: integer } { { s: "z", x: 1 }, { s: "zz", x: 2 }, { s: "zzz", x: 3 } }"#,
        "relation { x: integer } { { x: 2 } }",
        "0",
        "1",
        "5",
    ];
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        (code, &lines[..expected.len()]),
        (1, &expected[..]),
        "{stderr}"
    );
    assert!(
        lines[expected.len()].contains("even{n: 3} isn't a value of `even`"),
        "{stderr}"
    );
}

/// Value blocks whose statements are run, in every place one can be: in a
/// statement's expression, whose own scope is in use, in another's
/// statements, and for each tuple, of what's given back after their local
/// variables are given up.
const VALUE_BLOCK_STATEMENTS: &str = r#"var program_total := 5;
type t { { n: integer }, constraint [ var i := n; while i >= 2 loop [ i := i - 2; ]; i = 0 ] };
var scale := relation { { x: 1 }, { x: 2 } };

/* In a straight-line read-only operator, which still folds where it can. */
operator triple{n: integer}: integer [ n * 3 ];

operator with_block{n: integer}: integer [
    var base := triple{n: n};
    base + [
        var i := 0;
        var s := 0;
        while i < n loop [
            s := s + i;
            i := i + 1;
        ];
        s
    ]
];

operator main{} [
    var zero := parse_integer{from: "0"};
    /* In a statement's expression, whose own scope is in use. */
    var total := 1 + [
        var s := 0;
        for x in array [1, 2, 3] loop [
            s := s + x;
        ];
        s
    ];
    call debug_value{v: total};
    /* A text made in it, kept after. */
    var word := [
        var w := "";
        var i := 0;
        while i < 3 loop [
            w := w || "ab";
            i := i + 1;
        ];
        w
    ];
    call debug_value{v: word};
    call debug_value{v: with_block{n: 4}};
    call debug_value{v: with_block{n: 4 + zero}};
    /* For each tuple, reading the tuple at hand. */
    var r := relation { { x: 1 }, { x: 2 }, { x: 3 } };
    call debug_value{v: r extend { y := [ var p := 1; var i := 0; while i < x loop [ p := p * 2; i := i + 1; ]; p ] }};
    call debug_value{v: r where [ var keep := false; if x > 1 then [ keep := true; ]; keep ]};
    /* An `if` statement choosing, a transaction and a `fail`. */
    call debug_value{v: [ var a := 0; if zero = 0 then [ a := 7; ] else [ a := 8; ]; a ]};
    call debug_value{v: [
        var q := -1;
        var e: relation transaction_collection;
        transaction collecting e [
            q := 1 div zero;
        ];
        tuple { q: q, failures: count{r: e} }
    ]};
    /* Reading a program variable and a parameter-like local around it. */
    call debug_value{v: [ var s := program_total; if s > 1 then [ s := s + total; ]; s ]};
    /* Nested, one inside another's statements. */
    call debug_value{v: [
        var out := 0;
        var i := 0;
        while i < 2 loop [
            out := out + [
                var j := 0;
                var inner := 0;
                while j < 3 loop [
                    inner := inner + 1;
                    j := j + 1;
                ];
                inner
            ];
            i := i + 1;
        ];
        out
    ]};
    /* Failing, inside a transaction of the operator's. */
    var refused: relation transaction_collection;
    var kept := 0;
    transaction collecting refused [
        kept := [
            var i := 0;
            var made := "x";
            while i < 2 loop [
                made := made || "y";
                i := i + 1;
            ];
            1 div (zero * i)
        ];
    ];
    call debug_value{v: kept};
    call debug_value{v: count{r: refused}};
    /* A constraint that has one. */
    call debug_value{v: t{n: 4}};
    /* A failure, nothing to catch it. */
    call debug_value{v: t{n: 3 + zero}};
];
"#;

#[test]
fn value_blocks_run_statements() {
    let (code, stderr) = run_source("value_block_statements", VALUE_BLOCK_STATEMENTS);
    let expected = [
        "7",
        r#""ababab""#,
        "18",
        "18",
        "relation { x: integer, y: integer } { { x: 1, y: 2 }, { x: 2, y: 4 }, { x: 3, y: 8 } }",
        "relation { x: integer } { { x: 2 }, { x: 3 } }",
        "7",
        "tuple { failures: 1, q: -1 }",
        "12",
        "6",
        "0",
        "1",
        "t{n: 4}",
    ];
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        (code, &lines[..expected.len()]),
        (1, &expected[..]),
        "{stderr}"
    );
    assert!(
        lines[expected.len()].contains("t{n: 3} isn't a value of `t`"),
        "{stderr}"
    );
}

/// Relations whose tuples come out in canonical order or don't, and too big
/// for the runtime's scratch memory or not, when running.
#[test]
fn relations_are_sorted_only_if_they_need_it() {
    let source = "operator show{zero: integer} [
    var digits := relation {
        { d: 0 },
        { d: 1 },
        { d: 2 },
        { d: 3 },
        { d: 4 },
        { d: 5 },
        { d: 6 },
        { d: 7 },
        { d: 8 },
        { d: 9 },
    };
    var many := (times{
        digits rename {d as a},
        digits rename {d as b},
        digits rename {d as c},
        digits rename {d as e},
    } extend { n := 1000 * a + 100 * b + 10 * c + e + zero }){n};
    /* In order, by `n`, so nothing is sorted. */
    var same := many extend { p := n + 1 };
    call debug_value{v: value from (same where n = 9999){p}};
    /* Out of order: `a` comes first, and falls as `n` rises. */
    var fallen := many extend { a := 9999 - n };
    call debug_value{v: count{r: fallen}};
    call debug_value{v: value from (fallen where a = 0){n}};
    call debug_value{v: value from (fallen where n = 0){a}};
    /* Removing attributes makes duplicates, of many tuples. */
    call debug_value{v: count{r: (fallen extend { g := n mod 10 }){g}}};
    /* Pairs that outgrow the scratch memory, as they are made. */
    var arcs := (many where n < 300) extend { m := n + 1 };
    call debug_value{v: count{r: arcs tclose}};
    /* In order, holding texts each tuple's scope made, kept after them. */
    var named := (many where n < 3) extend { t := \"n = \" || to_text{from: n} };
    var other := named;
    call debug_table{r: other};
    /* One tuple, and none. */
    call debug_table{r: (many where n = 7) extend { t := to_text{from: n} }};
    call debug_value{v: count{r: (many where n < 0){n}}};
];

operator main{} [
    call show{zero: 0};
];
";
    let (code, stderr) = run_source("sorted_if_need_be", source);
    let expected = [
        "10000",
        "10000",
        "9999",
        "9999",
        "10",
        "45150",
        "┌───┬───────┐",
        "│ n │ t     │",
        "├───┼───────┤",
        "│ 0 │ n = 0 │",
        "│ 1 │ n = 1 │",
        "│ 2 │ n = 2 │",
        "└───┴───────┘",
        "┌───┬───┐",
        "│ n │ t │",
        "├───┼───┤",
        "│ 7 │ 7 │",
        "└───┴───┘",
        "0",
    ];
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!((code, &lines[..]), (0, &expected[..]), "{stderr}");
}

/// Tuples of nothing but integers, nested ones and a type of the program's
/// own's among them, are compared as a run of integers, when running, in
/// the order the compiler has them in when it knows them.
#[test]
fn tuples_of_integers_sort_when_running_as_when_compiling() {
    let source = "type point { { x: integer, y: integer } };

var known := relation {
    { m: tuple { p: 3, q: -2 }, n: point{x: 2, y: 3}, o: -1 },
    { m: tuple { p: -3, q: 2 }, n: point{x: 2, y: 3}, o: 1 },
    { m: tuple { p: 3, q: 2 }, n: point{x: -2, y: 3}, o: 1 },
    { m: tuple { p: 3, q: 2 }, n: point{x: 2, y: -3}, o: 1 },
    { m: tuple { p: 3, q: 2 }, n: point{x: 2, y: 3}, o: -1 },
    { m: tuple { p: 3, q: 2 }, n: point{x: 2, y: 3}, o: 1 },
};

operator show{a: integer, b: integer} [
    var made := relation {
        { m: tuple { p: a, q: b }, n: point{x: b, y: a}, o: 1 },
        { m: tuple { p: a, q: b }, n: point{x: b, y: a}, o: -1 },
        { m: tuple { p: a, q: b }, n: point{x: b, y: -a}, o: 1 },
        { m: tuple { p: a, q: b }, n: point{x: -b, y: a}, o: 1 },
        { m: tuple { p: -a, q: b }, n: point{x: b, y: a}, o: 1 },
        { m: tuple { p: a, q: -b }, n: point{x: b, y: a}, o: -1 },
        { m: tuple { p: a, q: b }, n: point{x: b, y: a}, o: 1 },
    };
    call debug_value{v: made};
    call debug_value{v: known};
    call debug_value{v: made = known};
];

operator main{} [
    call show{a: 3, b: 2};
];
";
    let (code, stderr) = run_source("integers_sorted", source);
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(lines[0], lines[1], "{stderr}");
    assert_eq!(lines[2], "true", "{stderr}");
    assert!(
        lines[0].contains(
            "{ m: tuple { p: -3, q: 2 }, n: point{x: 2, y: 3}, o: 1 }, { m: tuple { p: 3, q: -2 }, n: point{x: 2, y: 3}, o: -1 }, { m: tuple { p: 3, q: 2 }, n: point{x: -2, y: 3}, o: 1 }"
        ),
        "{stderr}"
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
            // One after the other, and ordered, a prefix first.
            "array [1200, 950, 220, 300]",
            "tuple { earlier_element: true, prefix: true }",
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

/// The most memory the executable at `path` held as it ran, in bytes, and that
/// it ran without failing. On macOS, the allocator keeps large blocks a program has
/// freed, for the next to use, and they count as the process's, which here would be
/// much of the figure and vary from run to run: without that, this is what the program
/// holds.
fn peak_memory(path: &Path) -> u64 {
    let (flag, label, unit) = if cfg!(target_os = "macos") {
        ("-l", "maximum resident set size", 1)
    } else {
        ("-v", "Maximum resident set size (kbytes)", 1024)
    };
    let run = Command::new("/usr/bin/time")
        .arg(flag)
        .arg(path)
        .env("MallocLargeCache", "0")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "{stderr}");
    let line = stderr.lines().find(|line| line.contains(label)).unwrap();
    let digits: String = line.chars().filter(char::is_ascii_digit).collect();
    digits.parse::<u64>().unwrap() * unit
}

/// The source of a program that compiles and runs `operators`, with `LEFT` in
/// them being `left`, and the most memory it holds as it runs.
fn peak_memory_of(name: &str, operators: &str, left: u32) -> u64 {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let input = dir.join(format!("{name}-{left}.trendy"));
    std::fs::write(&input, operators.replace("LEFT", &left.to_string())).unwrap();
    let output = dir.join(format!("{name}-{left}"));
    let (code, stderr) = build_and_run(&input, &output, &workspace_root());
    assert_eq!(code, 0, "{stderr}");
    peak_memory(&output)
}

/// Programs that each make a thousand tuples `LEFT` times, kept by what the name says.
const HOLDING_MEMORY: [(&str, &str); 10] = [
    (
        "an_argument",
        "operator churn{r: relation { value: integer }, left: integer}: integer [
            if left = 0 then count{r} else churn{r: series{from: left, to: left + 999}, left: left - 1}
        ];
        operator main{} [
            var left := LEFT;
            call debug_value{v: churn{r: relation {}, left}};
        ];",
    ),
    (
        "a_bound_value",
        "operator churn{left: integer}: integer [
            var r := series{from: left, to: left + 999};
            if left = 0 then count{r} else churn{left: left - count{r} + 999}
        ];
        operator main{} [
            var left := LEFT;
            call debug_value{v: churn{left}};
        ];",
    ),
    (
        "a_value_blocks_variable",
        "operator churn{left: integer}: integer [
            var r: relation { value: integer } := relation {};
            if left > 0 then [
                r := series{from: left, to: left + 999};
            ];
            if left = 0 then count{r} else churn{left: left - count{r} + 999}
        ];
        operator main{} [
            var left := LEFT;
            call debug_value{v: churn{left}};
        ];",
    ),
    (
        "what_its_arguments_are_made_of",
        "operator churn{n: integer, left: integer}: integer [
            if left = 0 then n else churn{n: count{r: series{from: left, to: left + 999}}, left: left - 1}
        ];
        operator main{} [
            var left := LEFT;
            call debug_value{v: churn{n: 0, left}};
        ];",
    ),
    (
        "an_update_operators_variable",
        "operator churn{left: integer, var kept: integer} [
            var r := series{from: left, to: left + 999};
            kept := count{r};
            if left > 0 then [
                call churn{left: left - 1, var kept};
            ];
        ];
        operator main{} [
            var left := LEFT;
            var kept := 0;
            call churn{left, var kept};
            call debug_value{v: kept};
        ];",
    ),
    (
        "an_update_operators_argument",
        "operator churn{r: relation { value: integer }, left: integer, var kept: integer} [
            kept := count{r};
            if left > 0 then [
                call churn{r: series{from: left, to: left + 999}, left: left - 1, var kept};
            ];
        ];
        operator main{} [
            var left := LEFT;
            var kept := 0;
            call churn{r: relation {}, left, var kept};
            call debug_value{v: kept};
        ];",
    ),
    (
        "as_it_returns",
        "operator given{r: relation { value: integer }, n: integer}: integer [
            if n = 0 then count{r} else given{r, n: n - 1}
        ];
        operator main{} [
            var left := LEFT;
            while left > 0 loop [
                left := left - given{r: series{from: left, to: left + 999}, n: 1} + 999;
            ];
        ];",
    ),
    (
        "as_an_update_operator_returns",
        "operator given{r: relation { value: integer }, n: integer} [
            if n > 0 then [
                call given{r, n: n - 1};
            ];
        ];
        operator main{} [
            var left := LEFT;
            while left > 0 loop [
                call given{r: series{from: left, to: left + 999}, n: 1};
                left := left - 1;
            ];
        ];",
    ),
    (
        "as_it_fails",
        "operator given{r: relation { value: integer }, n: integer}: integer [
            if n = 0 then 1 div (count{r} - 1000) else given{r, n: n - 1}
        ];
        operator main{} [
            var left := LEFT;
            var errors: relation transaction_collection := relation {};
            while left > 0 loop [
                var r := series{from: left, to: left + 999};
                transaction collecting errors [
                    left := left - given{r, n: 1};
                ];
                left := left - 1;
            ];
        ];",
    ),
    (
        "through_others",
        "operator churn{r: relation { value: integer }, left: integer}: integer [
            if left = 0 then count{r} else other{r: series{from: left, to: left + 999}, left: left - 1}
        ];
        operator other{r: relation { value: integer }, left: integer}: integer [
            churn{r: series{from: left, to: left + count{r} - 1}, left}
        ];
        operator main{} [
            var left := LEFT;
            call debug_value{v: churn{r: relation {}, left}};
        ];",
    ),
];

/// A tail call gives up what its frame holds, whatever holds it: the most
/// memory these hold doesn't grow with how many calls follow, though each makes
/// a thousand tuples, which all together would come to 160 MB.
#[test]
fn a_tail_call_keeps_no_memory() {
    for (name, source) in HOLDING_MEMORY {
        let few = peak_memory_of(name, source, 200);
        let many = peak_memory_of(name, source, 20_000);
        assert!(many < few + (1 << 20), "{name}: {few} bytes, then {many}");
    }
}

/// Operators of each shape a tail position has, each called twenty million deep.
const TAIL_CALLS: &str = r#"
var ticks := 0;

operator countdown{n: integer}: integer [
    if n = 0 then 0 else countdown{n: n - 1}
];

operator even{n: integer}: boolean [
    if n = 0 then true else odd{n: n - 1}
];

operator odd{n: integer}: boolean [
    if n = 0 then false else even{n: n - 1}
];

/* In a value block, and after a binding, and in the `if`s of each. */
operator blocks{n: integer}: integer [
    var half := n div 2;
    [
        var left := half + half + n mod 2;
        if left = 0 then 7 else if left mod 2 = 0 then blocks{n: left - 1} else blocks{n: left - 1}
    ]
];

/* A statement that decides what runs next: the body is a value block too. */
operator stated{n: integer}: integer [
    var left := n;
    if left > 0 then [
        left := left - 1;
    ];
    if n = 0 then 8 else stated{n: left}
];

/* Arguments it passes on as they are, and that it returns at the end. */
operator kept{r: relation { v: integer }, t: text, n: integer}: text [
    if n = 0 then t || to_text{from: count{r}} else kept{r, t, n: n - 1}
];

operator generic{a: T, n: integer}: T
    types { T: any type }
[
    if n = 0 then a else generic{a, n: n - 1}
];

operator checked{n: integer, constraint n >= 0}: integer [
    if n = 0 then 9 else checked{n: n - 1}
];

/* Update operators: the last statement, of a branch, of a branch. */
operator down{n: integer} [
    if n > 0 then [
        if n > 1 then [
            call down{n: n - 1};
        ] else [
            call down{n: 0};
        ];
    ];
];

/* The call is the last statement, with none around it. */
operator around{} [
    ticks := ticks + 1;
    if ticks < 20_000_000 then [
        call again{};
    ];
];

operator again{} [
    call around{};
];

/* A `var` parameter passed on, a variable's passed by a `var` parameter, and
   an `out` parameter. */
operator summed{n: integer, var total: integer} [
    total := total + n;
    if n > 0 then [
        call summed{n: n - 1, var total};
    ];
];

operator on_global{n: integer} [
    ticks := n;
    if n > 0 then [
        call by_global{n: n - 1, var left: ticks};
    ];
];

operator by_global{n: integer, var left: integer} [
    if n > 0 then [
        left := n;
        call by_global{n: n - 1, var left};
    ];
];

operator up_even{n: integer, out result: boolean} [
    if n = 0 then [
        result := true;
    ] else [
        call up_odd{n: n - 1, out result};
    ];
];

operator up_odd{n: integer, out result: boolean} [
    if n = 0 then [
        result := false;
    ] else [
        call up_even{n: n - 1, out result};
    ];
];

operator main{} [
    var n := 20_000_000;
    call debug_value{v: countdown{n}};
    call debug_value{v: even{n}};
    call debug_value{v: odd{n}};
    call debug_value{v: blocks{n}};
    call debug_value{v: stated{n}};
    var made := relation { { v: n }, { v: n + 1 } };
    call debug_value{v: kept{r: made, t: "tuples: " || to_text{from: 1 + 1}, n}};
    call debug_value{v: generic{a: n, n}};
    call debug_value{v: generic{a: "a" || to_text{from: n}, n}};
    call debug_value{v: count{r: generic{a: made, n}}};
    call debug_value{v: checked{n}};
    call down{n};
    call around{};
    var total := 0;
    call summed{n: 1000, var total};
    call debug_value{v: total};
    call summed{n, var total};
    call on_global{n};
    call debug_value{v: ticks};
    var result := false;
    call up_even{n, out result};
    call debug_value{v: result};
];
"#;

/// None of these can be done with a frame for each call: a million deep is
/// nothing, but twenty million, of frames of a hundred bytes or so, is more than
/// the stack has.
#[test]
fn tail_calls_take_no_stack() {
    let (code, stderr) = run_source("tail_calls", TAIL_CALLS);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            "0",
            "true",
            "false",
            "7",
            "8",
            "\"tuples: 22\"",
            "20000000",
            "\"a20000000\"",
            "2",
            "9",
            "500500",
            "1",
            "true",
        ]
    );
}

/// A specification whose versions call each other, and one whose version ends it.
const TAIL_VERSIONS: &str = r#"
type step union;

type forward is step { { by: integer } };

type back is step { { by: integer, name: text } };

operator walk{at: step, n: integer}: text;

operator walk{at: forward, n: integer}: text [
    if n = 0 then "forward " || to_text{from: at.by} else walk{at: back{by: n, name: "b" || to_text{from: n}}, n: n - 1}
];

operator walk{at: back, n: integer}: text [
    if n = 0 then at.name else walk{at: forward{by: n}, n: n - 1}
];

/* A version that calls nothing is in the cycle its specification is in, as a
   tail call can run it. */
type ending union;

type going is ending { { by: integer } };

type done is ending { { name: text } };

operator finish{at: ending, n: integer}: text;

operator finish{at: going, n: integer}: text [
    if n = 0 then finish{at: done{name: "done " || to_text{from: at.by}}, n} else finish{at: going{by: n}, n: n - 1}
];

operator finish{at: done, n: integer}: text [
    at.name
];

operator main{} [
    var n := 20_000_000;
    call debug_value{v: walk{at: forward{by: 0}, n}};
    n := n + 1;
    call debug_value{v: walk{at: forward{by: 0}, n}};
    call debug_value{v: finish{at: going{by: 0}, n}};
];
"#;

/// Whichever version a call of a specification fits, a tail call runs it in
/// place of its caller, as a call of the operator would be.
#[test]
fn a_tail_call_runs_a_version() {
    let (code, stderr) = run_source("tail_versions", TAIL_VERSIONS);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        ["\"forward 1\"", "\"b1\"", "\"done 1\""]
    );
}

/// What `TAIL_VALUES` writes.
const TAIL_VALUES_WRITTEN: [&str; 41] = [
    "10",
    "┌───────┐",
    "│ value │",
    "├───────┤",
    "│     1 │",
    "│     2 │",
    "└───────┘",
    "\"3\"",
    "3",
    "┌───────┬──────┐",
    "│ level │ wire │",
    "├───────┼──────┤",
    "│     3 │ a    │",
    "│     3 │ b    │",
    "│     3 │ c    │",
    "│     3 │ d    │",
    "└───────┴──────┘",
    "relation { level: integer, wire: text } { { level: 3, wire: \"a\" } }",
    "\"1x0\"",
    "\"n3!!!\"",
    "\"321..1..21..1..\"",
    "\"s3yyy\"",
    "\"1x0\"",
    "┌───────┬──────┐",
    "│ level │ wire │",
    "├───────┼──────┤",
    "│     1 │ w1   │",
    "└───────┴──────┘",
    "\"pzzzz\"",
    "┌──────┬───┐",
    "│ made │ n │",
    "├──────┼───┤",
    "│ 1x0  │ 1 │",
    "│ 1x0  │ 2 │",
    "└──────┴───┘",
    "┌──────┬───┐",
    "│ made │ n │",
    "├──────┼───┤",
    "│ k!   │ 1 │",
    "│ k!!  │ 2 │",
    "└──────┴───┘",
];

/// Values of each kind a call in a cycle takes, holds and gives.
const TAIL_VALUES: &str = r#"
heading signal := { wire: text, level: integer };

/* Returns what it was given, once nothing new comes. */
operator settled{gates: relation { wire: text, from_wire: text }, known: relation signal}: relation signal [
    var lit := (gates rename {wire as out_wire, from_wire as wire} join known){out_wire, level} rename {out_wire as wire};
    var more := known union lit;
    if more = known then known else settled{gates, known: more}
];

/* Returns what a binding holds. */
operator last_made{n: integer, s: text}: text [
    var made := s || "x" || to_text{from: n};
    if n = 0 then made else last_made{n: n - 1, s: to_text{from: count{r: series{from: 1, to: n}}}}
];

/* Returns a parameter's part. */
operator part{t: tuple { name: text, n: integer }}: text [
    if t.n = 0 then t.name else part{t: tuple { name: t.name || "!", n: t.n - 1 }}
];

/* Work left with the result, which is a value made by a tail call. */
operator doubled{n: integer}: text [
    if n = 0 then "." else [
        var below := doubled{n: n - 1};
        to_text{from: n} || below || below
    ]
];

/* In a value block, a local variable as its value. */
operator local_value{n: integer, s: text}: text [
    var kept := s;
    if n > 0 then [
        kept := kept || "y";
    ];
    if n = 0 then kept else local_value{n: n - 1, s: kept}
];

/* An update operator, whose argument its variable held. */
var shared := relation { { wire: "start", level: 1 } };

operator replaced{r: relation signal, n: integer} [
    shared := relation { { wire: "w" || to_text{from: n}, level: n } };
    if n = 0 then [
        call debug_table{r};
    ] else [
        call replaced{r: shared, n: n - 1};
    ];
];

/* A local variable that's an argument, and passed on. */
operator passing{n: integer, s: text, var out_text: text} [
    var mine := s || "z";
    out_text := mine;
    if n > 0 then [
        call passing{n: n - 1, s: mine, var out_text};
    ];
];

/* Returns its argument, the only thing holding it once a tail call has left the
   statement that made it, with nothing of its own to make. */
operator passes_on{r: relation { value: integer }, n: integer}: relation { value: integer } [
    if n = 0 then r else if n = 1 then fresh{n: 0} else passes_on{r, n: n - 1}
];

operator fresh{n: integer}: relation { value: integer } [
    passes_on{r: series{from: 1, to: 2 + n}, n}
];

/* Reads its parameter after a call that ends in a tail call of itself, which
   gives that other arguments. */
operator reread{n: integer}: integer [
    if n <= 0 then 0 else tail_to_reread{n: n - 1} + n
];

operator tail_to_reread{n: integer}: integer [
    reread{n}
];

/* Of two results, of which one points to something and the other doesn't,
   whichever is lowered first: `digits` makes a text for `parsed`, which holds
   it, whatever called it. */
operator digits{n: integer}: text [
    if n = 0 then "0" else to_text{from: parsed{n: n - 1} + 1}
];

operator parsed{n: integer}: integer [
    parse_integer{from: digits{n}}
];

operator main{} [
    var n := 3;
    call debug_value{v: reread{n: n + 1}};
    call debug_table{r: passes_on{r: relation {}, n}};
    call debug_value{v: digits{n}};
    call debug_value{v: parsed{n}};
    var gates := relation {
        { wire: "b", from_wire: "a" },
        { wire: "c", from_wire: "b" },
        { wire: "d", from_wire: "c" },
    };
    var known := relation { { wire: "a", level: n } };
    call debug_table{r: settled{gates, known}};
    call debug_value{v: known};
    call debug_value{v: last_made{n, s: "s"}};
    call debug_value{v: part{t: tuple { name: "n" || to_text{from: n}, n }}};
    call debug_value{v: doubled{n}};
    call debug_value{v: local_value{n, s: "s" || to_text{from: n}}};
    /* Called with known arguments too, when the program runs. */
    call debug_value{v: last_made{n: 1, s: "t"}};
    call replaced{r: shared, n};
    var written := "";
    call passing{n, s: "p", var out_text: written};
    call debug_value{v: written};
    /* For each tuple, of a relation known when compiling and of one that isn't. */
    var given := relation { { n: 2 }, { n: 1 } };
    call debug_table{r: given extend { made := last_made{n, s: "e"} }};
    call debug_table{r: relation { { n: 2 }, { n: 1 } } extend { made := part{t: tuple { name: "k", n }} }};
];
"#;

/// What a call in a cycle takes and gives holds up through tail calls and calls
/// with work left alike, freed memory being scribbled over: what it was given
/// as it returns it, what it made, what a binding held, and parts of each.
#[test]
fn values_outlive_tail_calls() {
    let (code, stderr) = run_source("tail_values", TAIL_VALUES);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stderr.lines().collect::<Vec<_>>(), TAIL_VALUES_WRITTEN);
}

/// Each program's name, its source, and what it writes before it fails.
const TOO_DEEP: [(&str, &str, &str); 9] = [
    (
        "too_deep_operand",
        "operator sum_to{n: integer}: integer [
            if n = 0 then 0 else n + sum_to{n: n - 1}
        ];
        operator main{} [
            var n := 1000;
            call debug_value{v: sum_to{n}};
            n := 100_000_000;
            call debug_value{v: sum_to{n}};
        ];",
        "500500\n",
    ),
    (
        "too_deep_for_each_tuple",
        "heading contains := { parent: text, child: text };
        operator size{part: text, parts: relation contains}: integer [
            1 + sum{r: (parts where parent = part) extend { under := size{part: child, parts} }, of: under}
        ];
        operator main{} [
            var tree: relation contains := relation {
                { parent: \"bike\", child: \"wheel\" },
                { parent: \"bike\", child: \"frame\" },
                { parent: \"wheel\", child: \"spoke\" },
                { parent: \"wheel\", child: \"hub\" },
            };
            call debug_value{v: size{part: \"bike\", parts: tree}};
            var ring: relation contains := relation {
                { parent: \"a\", child: \"b\" },
                { parent: \"b\", child: \"a\" },
            };
            call debug_value{v: size{part: \"a\", parts: ring}};
        ];",
        "5\n",
    ),
    (
        "too_deep_before_the_last_statement",
        "operator deep{n: integer} [
            if n > 0 then [
                call deep{n: n - 1};
                call debug_value{v: n};
            ];
        ];
        operator main{} [
            var n := 100_000_000;
            call deep{n};
        ];",
        "",
    ),
    (
        "too_deep_local_argument",
        "operator deep{n: integer, var total: integer} [
            var mine := total;
            if n > 0 then [
                call deep{n: n - 1, var total: mine};
            ];
        ];
        operator main{} [
            var total := 0;
            var n := 100_000_000;
            call deep{n, var total};
        ];",
        "",
    ),
    (
        "too_deep_in_a_statement",
        "operator deep{n: integer}: integer [
            var below := 0;
            if n > 0 then [
                below := deep{n: n - 1};
            ];
            below
        ];
        operator main{} [
            var n := 100_000_000;
            call debug_value{v: deep{n}};
        ];",
        "",
    ),
    (
        "too_deep_in_a_value_block",
        "operator deep{n: integer}: integer [
            1 + [
                var left := n - 1;
                deep{n: left}
            ]
        ];
        operator main{} [
            var n := 100_000_000;
            call debug_value{v: deep{n}};
        ];",
        "",
    ),
    (
        "too_deep_in_a_loop",
        "operator deep{n: integer} [
            var more := n > 0;
            while more loop [
                call deep{n: n - 1};
                more := false;
            ];
        ];
        operator main{} [
            var n := 100_000_000;
            call deep{n};
        ];",
        "",
    ),
    (
        "too_deep_in_a_transaction",
        "operator deep{n: integer} [
            transaction [
                if n > 0 then [
                    call deep{n: n - 1};
                ];
            ];
        ];
        operator main{} [
            var n := 100_000_000;
            call deep{n};
        ];",
        "",
    ),
    (
        "too_deep_treated",
        "type thing union;
        type small is thing { { n: integer } };
        type large is thing { { m: integer } };
        operator outer{n: integer}: small [
            treat inner{n: n + 1} as small
        ];
        operator inner{n: integer}: thing [
            outer{n}
        ];
        operator main{} [
            var n := 3;
            call debug_value{v: outer{n}};
        ];",
        "",
    ),
];

/// A call with work left after it, whether it is a call of a read-only or an
/// update operator's, fails like anything else once they use up the stack,
/// writing the ends of the calls and exiting with status 1: however they got
/// there, and a tail call among them makes no difference.
#[test]
fn calls_with_work_left_fail_when_they_go_too_deep() {
    for (name, source, before) in TOO_DEEP {
        let (code, stderr) = run_source(name, source);
        assert_eq!(code, 1, "{name}: {stderr}");
        let written = stderr
            .strip_prefix(before)
            .unwrap_or_else(|| panic!("{name}: {stderr}"));
        assert!(
            written.starts_with(
                "error[R0025]: the calls went too deep: those with work left when they return \
                 used up the 1073741824 bytes of the stack\n"
            ),
            "{name}: {stderr}"
        );
        assert!(
            stderr.contains("calls more, the outermost:"),
            "{name}: {stderr}"
        );
        assert!(stderr.lines().count() < 30, "{name}: {stderr}");
    }
}

/// A transaction takes a `too_deep`, as it does any other error, and then the
/// stack is as it was: the calls that fail give up what they hold on the way.
#[test]
fn a_transaction_takes_too_deep() {
    let (code, stderr) = run_source(
        "too_deep_taken",
        r#"
operator sum_to{n: integer, t: text}: integer [
    if n = 0 then length{a: to_code_points{from: t}} else n + sum_to{n: n - 1, t: t || ""}
];

operator main{} [
    var n := 100_000_000;
    var total := 0;
    var errors: relation transaction_collection := relation {};
    transaction collecting errors [
        total := sum_to{n, t: "abc"};
    ];
    call debug_value{v: total};
    n := 1000;
    transaction collecting errors [
        total := sum_to{n, t: "abc"};
    ];
    call debug_value{v: total};
];
"#,
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stderr.lines().collect::<Vec<_>>(), ["0", "500503"]);
}

/// A failure in a call that tail calls led to leaves no trace of what they held,
/// and of them only the last.
#[test]
fn a_tail_call_fails_like_any_other() {
    let (code, stderr) = run_source(
        "tail_failure",
        r"
operator dividing{n: integer, r: relation { v: integer }}: integer [
    var doubled := r extend { w := v * 2 };
    if n = 0 then 1 div count{r: doubled where w < 0} else dividing{n: n - 1, r: r union relation { { v: n } }}
];

operator main{} [
    var n := 100;
    call debug_value{v: dividing{n, r: relation {}}};
];
",
    );
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.starts_with("error[R0002]"), "{stderr}");
    assert_eq!(stderr.matches("in `dividing`").count(), 1, "{stderr}");
}

/// Each way a relation's tuples of integers come to be sorted, whose order is
/// signed and goes by each attribute in turn, and where equal tuples are one.
const INTEGER_TUPLES: &str = r"
type spot { { across: integer, up: integer } };

operator main{} [
    var big := 9223372036854775807;
    var low := -big - 1;
    var one := 1;
    var rows := relation {
        { a: one, b: 2 },
        { a: -one, b: 5 },
        { a: one, b: -2 },
        { a: big, b: low },
        { a: low, b: big },
    };
    /* A projection: the attribute that comes first is `b` once renamed, with
       equal tuples as one. */
    call debug_table{r: rows rename {a as z, b as c}};
    call debug_table{r: (rows extend { p := a * 0 }){p}};
    /* An `extend`, whose new attribute comes first, and equal tuples can't arise. */
    var few := (rows where a = one or a = -one) rename {a as m, b as n};
    call debug_table{r: few extend { a := -m }};
    /* A selector with values known only when the program runs. */
    call debug_table{r: relation { { n: one }, { n: -one }, { n: one }, { n: 0 * one } }};
    /* A join, its common attribute last in name order. */
    var left := relation { { k: one, a: 2 }, { k: 0 * one, a: 3 }, { k: 2 * one, a: 1 } };
    var right := relation { { k: one, c: 7 }, { k: 0 * one, c: 9 }, { k: 2 * one, c: 8 } };
    call debug_table{r: join{left, right}};
    /* A product whose attributes interleave. */
    var odds := relation { { a: 3 * one, c: 1 }, { a: one, c: 2 } };
    var evens := relation { { b: 2 * one, d: 1 }, { b: 0 * one, d: 2 } };
    call debug_table{r: odds times evens};
    /* An ungrouping, whose tuples don't come in order. */
    var grouped := relation {
        { g: 2 * one, r: relation { { n: 1 } } },
        { g: one, r: relation { { n: 3 }, { n: 2 } } },
    };
    call debug_table{r: (grouped ungroup {r}) rename {g as z}};
    /* An array's elements, which can repeat. */
    var numbers := array [3 * one, -one, 3 * one, 0 * one];
    var loaded: relation { n: integer };
    load loaded from numbers as n;
    call debug_table{r: loaded};
    /* A transitive closure. */
    var edges := relation { { from_n: 3 * one, to_n: one }, { from_n: one, to_n: 2 * one } };
    call debug_table{r: edges tclose};
    /* A tuple attribute, and a type of the program's own, are integers too. */
    var nested := relation {
        { t: tuple { y: one, x: 2 }, i: 1 },
        { t: tuple { y: -one, x: 2 }, i: 1 },
        { t: tuple { y: one, x: 1 }, i: 1 },
    };
    call debug_table{r: nested{t}};
    var spots := relation { { s: spot{across: one, up: 2} }, { s: spot{across: -one, up: 3} } };
    call debug_table{r: (spots extend { i := 1 }){s}};
    /* Eight attributes, the last sorted in place, and nine, the first that isn't. */
    var eight := relation {
        { a: one, b: 1, c: 1, d: 1, e: 1, f: 1, g: 1, h: 2 },
        { a: one, b: 1, c: 1, d: 1, e: 1, f: 1, g: 1, h: -2 },
    };
    call debug_table{r: eight rename {h as a0}};
    var nine := relation {
        { a: one, b: 1, c: 1, d: 1, e: 1, f: 1, g: 1, h: 1, i: 2 },
        { a: one, b: 1, c: 1, d: 1, e: 1, f: 1, g: 1, h: 1, i: -2 },
    };
    call debug_table{r: nine rename {i as a0}};
    /* What a projection leaves can grow and shrink where it is held. */
    var kept := (series{from: 1, to: 1000} rename {value as i}) extend { z := i mod 3 };
    var values := kept{z};
    insert values relation { { z: 7 * one }, { z: -one } };
    call debug_table{r: values};
    delete values where z = 7;
    call debug_table{r: values};
];
";

/// What `INTEGER_TUPLES` writes.
const INTEGER_TUPLES_WRITTEN: [&str; 106] = [
    "┌──────────────────────┬──────────────────────┐",
    "│ c                    │ z                    │",
    "├──────────────────────┼──────────────────────┤",
    "│ -9223372036854775808 │  9223372036854775807 │",
    "│                   -2 │                    1 │",
    "│                    2 │                    1 │",
    "│                    5 │                   -1 │",
    "│  9223372036854775807 │ -9223372036854775808 │",
    "└──────────────────────┴──────────────────────┘",
    "┌───┐",
    "│ p │",
    "├───┤",
    "│ 0 │",
    "└───┘",
    "┌────┬────┬────┐",
    "│ a  │ m  │ n  │",
    "├────┼────┼────┤",
    "│ -1 │  1 │ -2 │",
    "│ -1 │  1 │  2 │",
    "│  1 │ -1 │  5 │",
    "└────┴────┴────┘",
    "┌────┐",
    "│ n  │",
    "├────┤",
    "│ -1 │",
    "│  0 │",
    "│  1 │",
    "└────┘",
    "┌───┬───┬───┐",
    "│ a │ c │ k │",
    "├───┼───┼───┤",
    "│ 1 │ 8 │ 2 │",
    "│ 2 │ 7 │ 1 │",
    "│ 3 │ 9 │ 0 │",
    "└───┴───┴───┘",
    "┌───┬───┬───┬───┐",
    "│ a │ b │ c │ d │",
    "├───┼───┼───┼───┤",
    "│ 1 │ 0 │ 2 │ 2 │",
    "│ 1 │ 2 │ 2 │ 1 │",
    "│ 3 │ 0 │ 1 │ 2 │",
    "│ 3 │ 2 │ 1 │ 1 │",
    "└───┴───┴───┴───┘",
    "┌───┬───┐",
    "│ n │ z │",
    "├───┼───┤",
    "│ 1 │ 2 │",
    "│ 2 │ 1 │",
    "│ 3 │ 1 │",
    "└───┴───┘",
    "┌────┐",
    "│ n  │",
    "├────┤",
    "│ -1 │",
    "│  0 │",
    "│  3 │",
    "└────┘",
    "┌────────┬──────┐",
    "│ from_n │ to_n │",
    "├────────┼──────┤",
    "│      1 │    2 │",
    "│      3 │    1 │",
    "│      3 │    2 │",
    "└────────┴──────┘",
    "┌───────────────────────┐",
    "│ t                     │",
    "├───────────────────────┤",
    "│ tuple { x: 1, y: 1 }  │",
    "│ tuple { x: 2, y: -1 } │",
    "│ tuple { x: 2, y: 1 }  │",
    "└───────────────────────┘",
    "┌─────────────────────────┐",
    "│ s                       │",
    "├─────────────────────────┤",
    "│ spot{across: -1, up: 3} │",
    "│ spot{across: 1, up: 2}  │",
    "└─────────────────────────┘",
    "┌───┬────┬───┬───┬───┬───┬───┬───┐",
    "│ a │ a0 │ b │ c │ d │ e │ f │ g │",
    "├───┼────┼───┼───┼───┼───┼───┼───┤",
    "│ 1 │ -2 │ 1 │ 1 │ 1 │ 1 │ 1 │ 1 │",
    "│ 1 │  2 │ 1 │ 1 │ 1 │ 1 │ 1 │ 1 │",
    "└───┴────┴───┴───┴───┴───┴───┴───┘",
    "┌───┬────┬───┬───┬───┬───┬───┬───┬───┐",
    "│ a │ a0 │ b │ c │ d │ e │ f │ g │ h │",
    "├───┼────┼───┼───┼───┼───┼───┼───┼───┤",
    "│ 1 │ -2 │ 1 │ 1 │ 1 │ 1 │ 1 │ 1 │ 1 │",
    "│ 1 │  2 │ 1 │ 1 │ 1 │ 1 │ 1 │ 1 │ 1 │",
    "└───┴────┴───┴───┴───┴───┴───┴───┴───┘",
    "┌────┐",
    "│ z  │",
    "├────┤",
    "│ -1 │",
    "│  0 │",
    "│  1 │",
    "│  2 │",
    "│  7 │",
    "└────┘",
    "┌────┐",
    "│ z  │",
    "├────┤",
    "│ -1 │",
    "│  0 │",
    "│  1 │",
    "│  2 │",
    "└────┘",
];

#[test]
fn tuples_of_integers_sort_and_lose_duplicates_however_they_are_made() {
    let (code, stderr) = run_source("integer_tuples", INTEGER_TUPLES);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stderr.lines().collect::<Vec<_>>(), INTEGER_TUPLES_WRITTEN);
}

/// A relation holds no more memory than its tuples need, or twice as much at
/// most, however many it was projected from: fifty of a thousand values,
/// projected from a million tuples each, take next to nothing, and would come to
/// 400 MB in buffers of a million.
#[test]
fn a_projection_leaves_no_room_for_what_it_dropped() {
    let source = "
operator main{} [
    var n := 1_000_000;
    var made := (series{from: 1, to: n} rename {value as i}) extend { z := i mod 1000 };
    var held: relation { at: integer, values: relation { z: integer } } := relation {};
    var at := 0;
    while at < 50 loop [
        at := at + 1;
        held := held union relation { { at, values: made{z} } };
    ];
    call debug_value{v: count{r: held}};
];
";
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let input = dir.join("projection_room.trendy");
    std::fs::write(&input, source).unwrap();
    let output = dir.join("projection_room");
    let (code, stderr) = build_and_run(&input, &output, &workspace_root());
    assert_eq!((code, stderr.as_str()), (0, "50\n"));
    let peak = peak_memory(&output);
    assert!(peak < 150 << 20, "{peak} bytes");
}

/// Tuples of integers are sorted where they are written, so reshaping them holds
/// nothing but its operand and its result as it sorts: 16 bytes for each of four
/// million tuples, and with temporary memory for them and for pointers to them,
/// 40 bytes.
#[test]
fn sorting_integers_takes_no_memory_but_the_result() {
    let product = "operator main{} [
    var n := 40_000;
    var tuples := times{series{from: 1, to: n} rename {value as a}, series{from: 1, to: 100} rename {value as b}};
";
    let unsorted = peak_memory_of(
        "unsorted_integers",
        &format!("{product}    call debug_value{{v: count{{r: tuples}}}};\n];"),
        0,
    );
    let sorted = peak_memory_of(
        "sorted_integers",
        &format!("{product}    call debug_value{{v: count{{r: tuples rename {{a as c}}}}}};\n];"),
        0,
    );
    assert!(
        sorted < unsorted + 24 * 4_000_000,
        "{unsorted} bytes, then {sorted}"
    );
}

/// What a relational operation reads, as its operand, is given up as it has its
/// result, and whatever the result shares with it lasts, freed memory being
/// scribbled over: what a text attribute points to, and a relation-valued one's,
/// each read after the operand has gone, and what else reads the operand, or
/// shares its buffer, isn't given up with it.
const OPERANDS: &str = r#"
var kept := relation { { name: "a" || "b", n: 1 }, { name: "c" || "d", n: 2 } };

operator same{r: relation { name: text, n: integer }}: relation { name: text, n: integer } [
    r
];

operator named{kept: relation { name: text, n: integer }, n: integer}: relation { name: text, n: integer } [
    /* A binding read once, as an operand, of an operand. */
    var made := kept replace { name := name || to_text{from: n} };
    ((made rename {n as m}) extend { n := m + 1 }){name, n}
];

operator twice{kept: relation { name: text, n: integer }, n: integer}: relation { name: text, n: integer } [
    /* A binding read as an operand and in a condition, and one read after. */
    var made := kept replace { name := name || to_text{from: n} };
    var some := (made where count{r: made} = 2) rename {n as m};
    var again := made rename {n as m};
    (some union again) rename {m as n}
];

operator also{r: relation { name: text, n: integer }}: relation { name: text, n: integer } [
    /* A binding that another holds too. */
    var first := r where n > 0;
    var second := first;
    var dropped := first rename {n as m};
    second union (dropped rename {m as n})
];

operator both{r: relation { name: text, n: integer }}: relation { name: text, n: integer } [
    /* A binding read as two operands, of the last value that reads it. */
    var made := r where n > 0;
    join{made{name}, made}
];

operator each{r: relation { name: text, n: integer }}: relation { c: integer, name: text, n: integer } [
    /* One place reads it, for each tuple, and the rest of the time in a loop. */
    var made := r where n > 0;
    r extend { c := count{r: made{name}} }
];

operator looped{r: relation { name: text, n: integer }}: integer [
    var made := r where n > 0;
    [
        var i := 0;
        while i < 4 loop [
            i := i + count{r: made{name}};
        ];
        i
    ]
];

operator tuples{r: relation { name: text, n: integer }}: relation { name: text, n: integer } [
    /* A tuple's attributes are shared with what is computed from it. */
    var held := tuple { r: r where n > 0 };
    (held rename {r as s}).s
];

operator moved{r: relation { name: text, n: integer }}: relation { name: text, n: integer } [
    /* A binding of a tuple, the last read of it, whose attribute is shared. */
    var held := tuple { r: r where n > 0 };
    var moved := held rename {r as s};
    moved.s
];

operator later{r: relation { name: text, n: integer }}: relation { name: text, n: integer } [
    /* An operand here, and read again after. */
    var made := r where n > 0;
    var names := made{name};
    var numbers := made{name, n};
    join{names, numbers}
];

operator main{} [
    var two := 2;
    call debug_table{r: moved{r: kept}};
    call debug_table{r: later{r: kept}};
    call debug_table{r: both{r: kept}};
    call debug_table{r: each{r: kept}};
    call debug_value{v: looped{r: kept}};
    call debug_table{r: tuples{r: kept}};
    call debug_table{r: (tuple { r: kept where n > 1 } rename {r as s}).s};
    /* Texts, and a chain in which each result is the next's operand. */
    var texts := ((kept replace { name := name || to_text{from: two} }) rename {n as m}){name};
    call debug_table{r: texts};
    /* A relation-valued attribute, whose relation is shared with the operand. */
    var grouped := relation {
        { g: two, r: relation { { name: "x" || "y" } } },
        { g: 1 + two, r: relation { { name: "z" || "w" } } },
    };
    call debug_table{r: ((grouped where g > 0) rename {g as h}){r}};
    call debug_table{r: ((grouped where g > 0) rename {g as h}) ungroup {r}};
    /* Each operand of those with two. */
    var left := kept replace { name := name || "!" };
    call debug_table{r: join{left where n > 0, kept rename {name as other} where n > 0}};
    call debug_table{r: (left where n > 0) union (left where n > 1)};
    call debug_table{r: (left where n > 0) matching (kept where n > 1){n}};
    call debug_table{r: (left where n > 0) not matching (kept where n > 1){n}};
    /* An array's elements, from an operand and into one. */
    var names: array text;
    load names value from (left rename {n as m}) order [asc m] keep {name};
    call debug_value{v: names};
    var again: relation { name: text };
    load again from (array [names[1] || "?", names[2] || "?"]) as name;
    call debug_table{r: again rename {name as called}};
    /* A transitive closure of an operand. */
    var edges := relation { { a: two, b: 1 + two }, { a: 1 + two, b: 2 + two } };
    call debug_table{r: (edges where a > 0) tclose};
    /* What others hold: a variable's, a parameter's, one a call hands back. */
    call debug_table{r: kept rename {n as m}};
    call debug_table{r: kept};
    call debug_table{r: same{r: kept} rename {n as m}};
    call debug_table{r: kept};
    call debug_table{r: same{r: left where n > 1} rename {n as m}};
    call debug_table{r: if two > 1 then kept else left};
    call debug_table{r: (if two > 1 then kept else left) rename {n as m}};
    call debug_table{r: kept};
    /* Bindings. */
    call debug_table{r: named{kept, n: two}};
    call debug_table{r: twice{kept, n: two}};
    call debug_table{r: also{r: left}};
    call debug_table{r: left};
    /* An element of an array, and an attribute of a tuple, as an operand. */
    var rows := array [kept replace { name := name || "%" }];
    call debug_table{r: rows[1] rename {n as m}};
    call debug_table{r: rows[1]};
    var held := tuple { r: kept replace { name := name || "$" } };
    call debug_table{r: held.r rename {n as m}};
    call debug_table{r: held.r};
    call debug_table{r: tuple { r: kept where n > 1 }.r rename {n as m}};
    /* In a loop, assigned its own operation's result. */
    var growing := kept where n > 1;
    var at := 0;
    while at < 3 loop [
        at := at + 1;
        growing := ((growing replace { name := name || "+" }) rename {n as m}) rename {m as n};
    ];
    call debug_table{r: growing};
    /* For each tuple. */
    call debug_table{
        r: kept extend { found := count{r: ((left where n > 0) rename {n as m}){name}} },
    };
];
"#;

/// What `OPERANDS` writes.
const OPERANDS_WRITTEN: [&str; 201] = [
    "┌───┬──────┐",
    "│ n │ name │",
    "├───┼──────┤",
    "│ 1 │ ab   │",
    "│ 2 │ cd   │",
    "└───┴──────┘",
    "┌───┬──────┐",
    "│ n │ name │",
    "├───┼──────┤",
    "│ 1 │ ab   │",
    "│ 2 │ cd   │",
    "└───┴──────┘",
    "┌───┬──────┐",
    "│ n │ name │",
    "├───┼──────┤",
    "│ 1 │ ab   │",
    "│ 2 │ cd   │",
    "└───┴──────┘",
    "┌───┬───┬──────┐",
    "│ c │ n │ name │",
    "├───┼───┼──────┤",
    "│ 2 │ 1 │ ab   │",
    "│ 2 │ 2 │ cd   │",
    "└───┴───┴──────┘",
    "4",
    "┌───┬──────┐",
    "│ n │ name │",
    "├───┼──────┤",
    "│ 1 │ ab   │",
    "│ 2 │ cd   │",
    "└───┴──────┘",
    "┌───┬──────┐",
    "│ n │ name │",
    "├───┼──────┤",
    "│ 2 │ cd   │",
    "└───┴──────┘",
    "┌──────┐",
    "│ name │",
    "├──────┤",
    "│ ab2  │",
    "│ cd2  │",
    "└──────┘",
    "┌────────────────────────────────────────────┐",
    "│ r                                          │",
    "├────────────────────────────────────────────┤",
    "│ relation { name: text } { { name: \"xy\" } } │",
    "│ relation { name: text } { { name: \"zw\" } } │",
    "└────────────────────────────────────────────┘",
    "┌───┬──────┐",
    "│ h │ name │",
    "├───┼──────┤",
    "│ 2 │ xy   │",
    "│ 3 │ zw   │",
    "└───┴──────┘",
    "┌───┬──────┬───────┐",
    "│ n │ name │ other │",
    "├───┼──────┼───────┤",
    "│ 1 │ ab!  │ ab    │",
    "│ 2 │ cd!  │ cd    │",
    "└───┴──────┴───────┘",
    "┌───┬──────┐",
    "│ n │ name │",
    "├───┼──────┤",
    "│ 1 │ ab!  │",
    "│ 2 │ cd!  │",
    "└───┴──────┘",
    "┌───┬──────┐",
    "│ n │ name │",
    "├───┼──────┤",
    "│ 2 │ cd!  │",
    "└───┴──────┘",
    "┌───┬──────┐",
    "│ n │ name │",
    "├───┼──────┤",
    "│ 1 │ ab!  │",
    "└───┴──────┘",
    "array [\"ab!\", \"cd!\"]",
    "┌────────┐",
    "│ called │",
    "├────────┤",
    "│ ab!?   │",
    "│ cd!?   │",
    "└────────┘",
    "┌───┬───┐",
    "│ a │ b │",
    "├───┼───┤",
    "│ 2 │ 3 │",
    "│ 2 │ 4 │",
    "│ 3 │ 4 │",
    "└───┴───┘",
    "┌───┬──────┐",
    "│ m │ name │",
    "├───┼──────┤",
    "│ 1 │ ab   │",
    "│ 2 │ cd   │",
    "└───┴──────┘",
    "┌───┬──────┐",
    "│ n │ name │",
    "├───┼──────┤",
    "│ 1 │ ab   │",
    "│ 2 │ cd   │",
    "└───┴──────┘",
    "┌───┬──────┐",
    "│ m │ name │",
    "├───┼──────┤",
    "│ 1 │ ab   │",
    "│ 2 │ cd   │",
    "└───┴──────┘",
    "┌───┬──────┐",
    "│ n │ name │",
    "├───┼──────┤",
    "│ 1 │ ab   │",
    "│ 2 │ cd   │",
    "└───┴──────┘",
    "┌───┬──────┐",
    "│ m │ name │",
    "├───┼──────┤",
    "│ 2 │ cd!  │",
    "└───┴──────┘",
    "┌───┬──────┐",
    "│ n │ name │",
    "├───┼──────┤",
    "│ 1 │ ab   │",
    "│ 2 │ cd   │",
    "└───┴──────┘",
    "┌───┬──────┐",
    "│ m │ name │",
    "├───┼──────┤",
    "│ 1 │ ab   │",
    "│ 2 │ cd   │",
    "└───┴──────┘",
    "┌───┬──────┐",
    "│ n │ name │",
    "├───┼──────┤",
    "│ 1 │ ab   │",
    "│ 2 │ cd   │",
    "└───┴──────┘",
    "┌───┬──────┐",
    "│ n │ name │",
    "├───┼──────┤",
    "│ 2 │ ab1  │",
    "│ 3 │ cd2  │",
    "└───┴──────┘",
    "┌───┬──────┐",
    "│ n │ name │",
    "├───┼──────┤",
    "│ 1 │ ab1  │",
    "│ 2 │ cd2  │",
    "└───┴──────┘",
    "┌───┬──────┐",
    "│ n │ name │",
    "├───┼──────┤",
    "│ 1 │ ab!  │",
    "│ 2 │ cd!  │",
    "└───┴──────┘",
    "┌───┬──────┐",
    "│ n │ name │",
    "├───┼──────┤",
    "│ 1 │ ab!  │",
    "│ 2 │ cd!  │",
    "└───┴──────┘",
    "┌───┬──────┐",
    "│ m │ name │",
    "├───┼──────┤",
    "│ 1 │ ab%  │",
    "│ 2 │ cd%  │",
    "└───┴──────┘",
    "┌───┬──────┐",
    "│ n │ name │",
    "├───┼──────┤",
    "│ 1 │ ab%  │",
    "│ 2 │ cd%  │",
    "└───┴──────┘",
    "┌───┬──────┐",
    "│ m │ name │",
    "├───┼──────┤",
    "│ 1 │ ab$  │",
    "│ 2 │ cd$  │",
    "└───┴──────┘",
    "┌───┬──────┐",
    "│ n │ name │",
    "├───┼──────┤",
    "│ 1 │ ab$  │",
    "│ 2 │ cd$  │",
    "└───┴──────┘",
    "┌───┬──────┐",
    "│ m │ name │",
    "├───┼──────┤",
    "│ 2 │ cd   │",
    "└───┴──────┘",
    "┌───┬───────┐",
    "│ n │ name  │",
    "├───┼───────┤",
    "│ 2 │ cd+++ │",
    "└───┴───────┘",
    "┌───────┬───┬──────┐",
    "│ found │ n │ name │",
    "├───────┼───┼──────┤",
    "│     2 │ 1 │ ab   │",
    "│     2 │ 2 │ cd   │",
    "└───────┴───┴──────┘",
];

#[test]
fn an_operand_goes_as_soon_as_it_is_read() {
    let (code, stderr) = run_source("operands", OPERANDS);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stderr.lines().collect::<Vec<_>>(), OPERANDS_WRITTEN);
}

/// A chain of operations holds one operand and one result at a time, each of 24
/// bytes for each of four million tuples: with every result lasting until the
/// statement ends, the five in this one would come to 480 MB.
#[test]
fn a_chain_of_operations_holds_two_results_at_a_time() {
    let source = "operator main{} [
    var n := 40_000;
    var tuples := times{series{from: 1, to: n} rename {value as a}, series{from: 1, to: 100} rename {value as b}};
    call debug_value{
        v: count{
            r: ((((tuples extend { c := a }) rename {c as d}) rename {d as e}) rename {e as f}) rename {f as g},
        },
    };
];";
    let peak = peak_memory_of("operand_chain", source, 0);
    assert!(peak < 300 << 20, "{peak} bytes");
}

/// A binding that an operation reads last, as its operand, goes as the operation
/// has its result, rather than as the value it is part of is computed: while the
/// projection and the renaming go on, `tuples` is gone. With it, this holds 64 MB
/// more, of 16 bytes for each tuple.
#[test]
fn a_binding_goes_as_its_last_operation_has_its_result() {
    let program = |value: &str| {
        format!(
            "operator kept{{n: integer}}: integer [
    var tuples := times{{series{{from: 1, to: n}} rename {{value as a}}, series{{from: 1, to: 100}} rename {{value as b}}}};
    var renamed := (({value}) rename {{a as c}}) rename {{b as d}};
    count{{r: renamed}}
];
operator main{{}} [
    call debug_value{{v: kept{{n: 40_000}}}};
];"
        )
    };
    // Read once more after, in the second, so that it can't go.
    let last = peak_memory_of("binding_last", &program("tuples"), 0);
    let held = peak_memory_of(
        "binding_held",
        &program("tuples where a > count{r: tuples} * 0 - 1"),
        0,
    );
    assert!(last + (32 << 20) < held, "{last} bytes, and {held} held");
}

/// The closure of random graphs, each the arcs of `arcs` random pairs of `nodes`
/// values, against one computed here, as how many tuples it has and a sum that
/// depends on which they are: every shape of cycles, loops, values that only
/// start arcs or only end them, and many ways to one value.
#[test]
fn a_transitive_closure_has_every_path_of_any_graph() {
    let mut random = 20_261_104_u64;
    let mut next = |below: u64| {
        random = random
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (random >> 33) % below
    };
    let mut body = String::from("    var one := 1;\n");
    let mut expected = Vec::new();
    for graph in 0..60 {
        let nodes: u64 = [1, 2, 3, 5, 8, 13, 21, 34][usize::try_from(next(8)).unwrap()];
        let count = next(nodes * nodes / 2 + 2);
        let mut arcs = std::collections::BTreeSet::new();
        for _ in 0..count {
            arcs.insert((next(nodes), next(nodes)));
        }
        let mut closure = arcs.clone();
        loop {
            let more: Vec<(u64, u64)> = closure
                .iter()
                .flat_map(|&(a, b)| {
                    arcs.iter()
                        .filter(move |&&(c, _)| c == b)
                        .map(move |&(_, d)| (a, d))
                })
                .filter(|pair| !closure.contains(pair))
                .collect();
            if more.is_empty() {
                break;
            }
            closure.extend(more);
        }
        let tuples: Vec<String> = arcs
            .iter()
            .map(|(a, b)| format!("{{ a: one * {a}, b: one * {b} }}"))
            .collect();
        writeln!(
            body,
            "    var g{graph}: relation {{ a: integer, b: integer }} := relation {{ {} }};",
            tuples.join(", ")
        )
        .unwrap();
        writeln!(
            body,
            "    call debug_value{{v: tuple {{ c: count{{r: g{graph} tclose}}, s: sum{{r: g{graph} tclose, of: a * 100 + b}} }}}};",
        )
        .unwrap();
        let sum: u64 = closure.iter().map(|(a, b)| a * 100 + b).sum();
        expected.push(format!("tuple {{ c: {}, s: {sum} }}", closure.len()));
    }
    let (code, stderr) = run_source("tclose_graphs", &format!("operator main{{}} [\n{body}];\n"));
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stderr.lines().collect::<Vec<_>>(), expected);
}

/// Values of each kind a closure can be of, whose order is the order its tuples
/// are in, each read after the operand has gone, freed memory being scribbled
/// over, and those that take no memory, or that a tuple arcs to itself.
const CLOSURES: &str = r#"
type mark { { n: integer } };

operator main{} [
    var one := 1;
    var names := relation {
        { from: "c" || "c", to: "a" || "a" },
        { from: "a" || "a", to: "b" || "b" },
        { from: "b" || "b", to: "a" || "a" },
        { from: "d" || "d", to: "c" || "c" },
    };
    call debug_table{r: (names where from <> "") tclose};
    var flags := relation { { p: one = 1, q: one = 2 }, { p: one = 2, q: one = 2 } };
    call debug_table{r: flags tclose};
    var pairs := relation {
        { x: tuple { t: "b" || "", n: one }, y: tuple { t: "a" || "", n: one } },
        { x: tuple { t: "a" || "", n: one }, y: tuple { t: "a" || "", n: 2 * one } },
    };
    call debug_table{r: pairs tclose};
    var marks := relation { { a: mark{n: 2 * one}, b: mark{n: one} }, { a: mark{n: one}, b: mark{n: 0 * one} } };
    call debug_table{r: marks tclose};
    var sets := relation {
        { a: relation { { n: 2 * one } }, b: relation { { n: one } } },
        { a: relation { { n: one } }, b: relation { { n: 0 * one } } },
    };
    call debug_table{r: sets tclose};
    var nothing: relation { a: tuple {}, b: tuple {} } := relation { { a: tuple {}, b: tuple {} } } where one = 1;
    call debug_table{r: nothing tclose};
    var none: relation { a: integer, b: integer } := relation { { a: 1, b: 2 } } where one = 2;
    call debug_table{r: none tclose};
    var loops := relation { { a: one, b: one } };
    call debug_table{r: loops tclose};
    /* The same closure after, and an operation on it. */
    var stops := relation { { a: 3 * one, b: 2 * one }, { a: 2 * one, b: one } };
    call debug_table{r: (stops tclose) where a = 3};
];
"#;

/// What `CLOSURES` writes.
const CLOSURES_WRITTEN: [&str; 60] = [
    "┌──────┬────┐",
    "│ from │ to │",
    "├──────┼────┤",
    "│ aa   │ aa │",
    "│ aa   │ bb │",
    "│ bb   │ aa │",
    "│ bb   │ bb │",
    "│ cc   │ aa │",
    "│ cc   │ bb │",
    "│ dd   │ aa │",
    "│ dd   │ bb │",
    "│ dd   │ cc │",
    "└──────┴────┘",
    "┌───────┬───────┐",
    "│ p     │ q     │",
    "├───────┼───────┤",
    "│ false │ false │",
    "│ true  │ false │",
    "└───────┴───────┘",
    "┌────────────────────────┬────────────────────────┐",
    "│ x                      │ y                      │",
    "├────────────────────────┼────────────────────────┤",
    "│ tuple { n: 1, t: \"a\" } │ tuple { n: 2, t: \"a\" } │",
    "│ tuple { n: 1, t: \"b\" } │ tuple { n: 1, t: \"a\" } │",
    "│ tuple { n: 1, t: \"b\" } │ tuple { n: 2, t: \"a\" } │",
    "└────────────────────────┴────────────────────────┘",
    "┌────────────┬────────────┐",
    "│ a          │ b          │",
    "├────────────┼────────────┤",
    "│ mark{n: 1} │ mark{n: 0} │",
    "│ mark{n: 2} │ mark{n: 0} │",
    "│ mark{n: 2} │ mark{n: 1} │",
    "└────────────┴────────────┘",
    "┌──────────────────────────────────────┬──────────────────────────────────────┐",
    "│ a                                    │ b                                    │",
    "├──────────────────────────────────────┼──────────────────────────────────────┤",
    "│ relation { n: integer } { { n: 1 } } │ relation { n: integer } { { n: 0 } } │",
    "│ relation { n: integer } { { n: 2 } } │ relation { n: integer } { { n: 0 } } │",
    "│ relation { n: integer } { { n: 2 } } │ relation { n: integer } { { n: 1 } } │",
    "└──────────────────────────────────────┴──────────────────────────────────────┘",
    "┌──────────┬──────────┐",
    "│ a        │ b        │",
    "├──────────┼──────────┤",
    "│ tuple {} │ tuple {} │",
    "└──────────┴──────────┘",
    "┌───┬───┐",
    "│ a │ b │",
    "├───┼───┤",
    "└───┴───┘",
    "┌───┬───┐",
    "│ a │ b │",
    "├───┼───┤",
    "│ 1 │ 1 │",
    "└───┴───┘",
    "┌───┬───┐",
    "│ a │ b │",
    "├───┼───┤",
    "│ 3 │ 1 │",
    "│ 3 │ 2 │",
    "└───┴───┘",
];

#[test]
fn a_transitive_closure_is_of_values_of_any_kind() {
    let (code, stderr) = run_source("closures", CLOSURES);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stderr.lines().collect::<Vec<_>>(), CLOSURES_WRITTEN);
}

/// A closure takes about what its result does: 16 bytes for each of its two
/// million tuples, however many steps its paths take, and it took 40 times that
/// with each step keeping a merged copy of all so far.
#[test]
fn a_transitive_closure_takes_about_what_its_result_does() {
    let source = "operator main{} [
    var n := 2000;
    var edges := (series{from: 1, to: n} rename {value as a}) extend { b := a + 1 };
    call debug_value{v: count{r: edges tclose}};
];";
    let peak = peak_memory_of("tclose_memory", source, 0);
    assert!(peak < 100 << 20, "{peak} bytes");
}

/// An arc to a value already reached is passed over, as all that it reaches came
/// with it: here, with all 4.5 million arcs that go forward among 3000 values, that
/// takes 40 times less time, which only a wide margin of it can tell.
#[test]
fn a_transitive_closure_goes_each_way_to_a_value_once() {
    let source = "operator main{} [
    var n := 3000;
    var a := series{from: 1, to: n} rename {value as a};
    var b := series{from: 1, to: n} rename {value as b};
    var edges := (a times b) where a < b;
    call debug_value{v: count{r: edges tclose}};
];";
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let input = dir.join("tclose_ways.trendy");
    std::fs::write(&input, source).unwrap();
    let output = dir.join("tclose_ways");
    let (code, stderr) = build_and_run(&input, &output, &workspace_root());
    assert_eq!((code, stderr.as_str()), (0, "4498500\n"));
    let start = std::time::Instant::now();
    Command::new(&output).output().unwrap();
    let taken = start.elapsed();
    assert!(taken.as_secs() < 4, "{taken:?}");
}

/// Restrictions of a closure that follow only one value's paths, of values of
/// each kind, freed memory being scribbled over, whatever else the condition says.
const CLOSURE_OF_A_VALUE: &str = r#"
operator from_here{edges: relation { a: text, b: text }, here: text}: relation { b: text } [
    (edges tclose where a = here){b}
];

operator main{} [
    var one := 1;
    var two := 2;
    var three := 3;
    var four := 4;
    var five := 5;
    var six := 6;
    var nine := 9;
    /* A diamond, then a cycle, then a value only an arc ends at. */
    var edges := relation {
        { a: one * 1, b: one * 2 },
        { a: one * 1, b: one * 3 },
        { a: one * 2, b: one * 4 },
        { a: one * 3, b: one * 4 },
        { a: one * 4, b: one * 5 },
        { a: one * 5, b: one * 4 },
        { a: one * 5, b: one * 6 },
    };
    call debug_table{r: edges tclose where a = one};
    call debug_table{r: edges tclose where a = four};
    call debug_table{r: edges tclose where b = four};
    call debug_table{r: edges tclose where two = b};
    call debug_table{r: edges tclose where a = six};
    call debug_table{r: edges tclose where b = one};
    call debug_table{r: edges tclose where a = nine};
    call debug_table{r: edges tclose where b = nine};
    /* Both, what else there is, and an operation on its result. */
    call debug_table{r: edges tclose where a = one and b = six};
    call debug_table{r: edges tclose where b = six and a = nine};
    call debug_table{r: edges tclose where a = one and b > 4};
    call debug_table{r: edges tclose where b = four and a <> b};
    call debug_table{r: (edges tclose where a = three){b}};
    /* A value that can fail, by itself, or with another. */
    call debug_table{r: edges tclose where a = one * 3};
    call debug_table{r: edges tclose where b = one * 4};
    call debug_table{r: edges tclose where a = one * 1 and b = one * 6};
    /* An operand that's made here, and goes, and a value of each tuple. */
    call debug_table{r: (edges where a > 0) tclose where a = five};
    var nodes := relation { { v: one * 1 }, { v: one * 5 }, { v: one * 6 } };
    call debug_table{
        r: nodes extend {
            down := count{r: edges tclose where a = v},
            up := count{r: edges tclose where b = v},
        },
    };
    /* Values that point to what they're shared with, and a parameter. */
    var names := relation {
        { a: "x" || "1", b: "x" || "2" },
        { a: "x" || "2", b: "x" || "3" },
        { a: "x" || "3", b: "x" || "2" },
    };
    call debug_table{r: names tclose where a = "x" || "1"};
    call debug_table{r: names tclose where b = "x" || "3"};
    call debug_table{r: from_here{edges: names, here: "x" || "2"}};
    var held := names tclose where a = "x" || "3";
    call debug_table{r: held};
    /* Pairs of tuples of text. */
    var places := relation {
        { a: tuple { p: "m" || "n", q: one }, b: tuple { p: "m" || "o", q: one } },
        { a: tuple { p: "m" || "o", q: one }, b: tuple { p: "m" || "p", q: one } },
    };
    call debug_table{r: places tclose where a = tuple { p: "m" || "n", q: one }};
    /* A value that can fail, computed only if a tuple is there to compare it to,
       and not when a guard holds for none of them. */
    var none: relation { a: integer, b: integer } := relation {};
    var zero := one - 1;
    call debug_table{r: none tclose where a = 1 div zero};
    call debug_table{r: edges tclose where a = 1 div zero and b < 0};
    call debug_table{r: edges tclose where a = nine and b = 1 div zero};
    call debug_table{r: edges tclose where a = 1 div zero};
];
"#;

/// What `CLOSURE_OF_A_VALUE` writes.
const CLOSURE_OF_A_VALUE_WRITTEN: [&str; 156] = [
    "┌───┬───┐",
    "│ a │ b │",
    "├───┼───┤",
    "│ 1 │ 2 │",
    "│ 1 │ 3 │",
    "│ 1 │ 4 │",
    "│ 1 │ 5 │",
    "│ 1 │ 6 │",
    "└───┴───┘",
    "┌───┬───┐",
    "│ a │ b │",
    "├───┼───┤",
    "│ 4 │ 4 │",
    "│ 4 │ 5 │",
    "│ 4 │ 6 │",
    "└───┴───┘",
    "┌───┬───┐",
    "│ a │ b │",
    "├───┼───┤",
    "│ 1 │ 4 │",
    "│ 2 │ 4 │",
    "│ 3 │ 4 │",
    "│ 4 │ 4 │",
    "│ 5 │ 4 │",
    "└───┴───┘",
    "┌───┬───┐",
    "│ a │ b │",
    "├───┼───┤",
    "│ 1 │ 2 │",
    "└───┴───┘",
    "┌───┬───┐",
    "│ a │ b │",
    "├───┼───┤",
    "└───┴───┘",
    "┌───┬───┐",
    "│ a │ b │",
    "├───┼───┤",
    "└───┴───┘",
    "┌───┬───┐",
    "│ a │ b │",
    "├───┼───┤",
    "└───┴───┘",
    "┌───┬───┐",
    "│ a │ b │",
    "├───┼───┤",
    "└───┴───┘",
    "┌───┬───┐",
    "│ a │ b │",
    "├───┼───┤",
    "│ 1 │ 6 │",
    "└───┴───┘",
    "┌───┬───┐",
    "│ a │ b │",
    "├───┼───┤",
    "└───┴───┘",
    "┌───┬───┐",
    "│ a │ b │",
    "├───┼───┤",
    "│ 1 │ 5 │",
    "│ 1 │ 6 │",
    "└───┴───┘",
    "┌───┬───┐",
    "│ a │ b │",
    "├───┼───┤",
    "│ 1 │ 4 │",
    "│ 2 │ 4 │",
    "│ 3 │ 4 │",
    "│ 5 │ 4 │",
    "└───┴───┘",
    "┌───┐",
    "│ b │",
    "├───┤",
    "│ 4 │",
    "│ 5 │",
    "│ 6 │",
    "└───┘",
    "┌───┬───┐",
    "│ a │ b │",
    "├───┼───┤",
    "│ 3 │ 4 │",
    "│ 3 │ 5 │",
    "│ 3 │ 6 │",
    "└───┴───┘",
    "┌───┬───┐",
    "│ a │ b │",
    "├───┼───┤",
    "│ 1 │ 4 │",
    "│ 2 │ 4 │",
    "│ 3 │ 4 │",
    "│ 4 │ 4 │",
    "│ 5 │ 4 │",
    "└───┴───┘",
    "┌───┬───┐",
    "│ a │ b │",
    "├───┼───┤",
    "│ 1 │ 6 │",
    "└───┴───┘",
    "┌───┬───┐",
    "│ a │ b │",
    "├───┼───┤",
    "│ 5 │ 4 │",
    "│ 5 │ 5 │",
    "│ 5 │ 6 │",
    "└───┴───┘",
    "┌──────┬────┬───┐",
    "│ down │ up │ v │",
    "├──────┼────┼───┤",
    "│    0 │  5 │ 6 │",
    "│    3 │  5 │ 5 │",
    "│    5 │  0 │ 1 │",
    "└──────┴────┴───┘",
    "┌────┬────┐",
    "│ a  │ b  │",
    "├────┼────┤",
    "│ x1 │ x2 │",
    "│ x1 │ x3 │",
    "└────┴────┘",
    "┌────┬────┐",
    "│ a  │ b  │",
    "├────┼────┤",
    "│ x1 │ x3 │",
    "│ x2 │ x3 │",
    "│ x3 │ x3 │",
    "└────┴────┘",
    "┌────┐",
    "│ b  │",
    "├────┤",
    "│ x2 │",
    "│ x3 │",
    "└────┘",
    "┌────┬────┐",
    "│ a  │ b  │",
    "├────┼────┤",
    "│ x3 │ x2 │",
    "│ x3 │ x3 │",
    "└────┴────┘",
    "┌─────────────────────────┬─────────────────────────┐",
    "│ a                       │ b                       │",
    "├─────────────────────────┼─────────────────────────┤",
    "│ tuple { p: \"mn\", q: 1 } │ tuple { p: \"mo\", q: 1 } │",
    "│ tuple { p: \"mn\", q: 1 } │ tuple { p: \"mp\", q: 1 } │",
    "└─────────────────────────┴─────────────────────────┘",
    "┌───┬───┐",
    "│ a │ b │",
    "├───┼───┤",
    "└───┴───┘",
    "┌───┬───┐",
    "│ a │ b │",
    "├───┼───┤",
    "└───┴───┘",
    "┌───┬───┐",
    "│ a │ b │",
    "├───┼───┤",
    "└───┴───┘",
    "error[R0002]: division by zero: 1 div 0 has no result",
    " --> closure_of_a_value.trendy:75:5",
];

#[test]
fn a_closure_can_be_asked_about_one_value() {
    let (code, stderr) = run_source("closure_of_a_value", CLOSURE_OF_A_VALUE);
    assert_eq!(code, 1, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        CLOSURE_OF_A_VALUE_WRITTEN
    );
}

/// Its paths from or to one value, of a chain of twenty thousand: with all the
/// closure's 200 million tuples made first, that would come to over 3 GB.
#[test]
fn a_closure_of_one_value_makes_no_more_of_it() {
    let source = "operator main{} [
    var n := 20_000;
    var edges := (series{from: 1, to: n} rename {value as a}) extend { b := a + 1 };
    call debug_value{v: count{r: edges tclose where a = n - 5}};
    call debug_value{v: count{r: edges tclose where b = 5}};
];";
    let peak = peak_memory_of("closure_of_one", source, 0);
    assert!(peak < 50 << 20, "{peak} bytes");
}

/// What a closure of one value reads is given up once it has its result, as any
/// operation's operand is: of 64 MB, which with the next operand's would make 192
/// here, rather than 128.
#[test]
fn a_closure_of_one_value_gives_up_its_operand() {
    let source = "operator main{} [
    var n := 40_000;
    var one := 1;
    var t := times{series{from: 1, to: n} rename {value as a}, series{from: 1, to: 100} rename {value as b}};
    call debug_value{
        v: count{
            r: ((t rename {a as c, b as d}) tclose where c = one){d} union (t rename {a as c, b as d}){d},
        },
    };
];";
    let peak = peak_memory_of("closure_operand", source, 0);
    assert!(peak < 195 << 20, "{peak} bytes");
}

/// A diamond, then a cycle, a value only an arc ends at, and an arc to itself, asked
/// about values that reach something and those that don't, by `matching` and `join`,
/// on one attribute or both, and for each tuple of a relation.
#[test]
fn a_closure_can_be_asked_about_many_values() {
    let (code, stderr) = run_source(
        "closure_of_values",
        r#"
operator main{} [
    var one := 1;
    var edges := relation {
        { a: one * 1, b: one * 2 },
        { a: one * 1, b: one * 3 },
        { a: one * 2, b: one * 4 },
        { a: one * 3, b: one * 4 },
        { a: one * 4, b: one * 5 },
        { a: one * 5, b: one * 4 },
        { a: one * 5, b: one * 6 },
        { a: one * 7, b: one * 7 },
    };
    var from := relation { { a: one * 2 }, { a: one * 6 }, { a: one * 7 }, { a: one * 9 } };
    var to := relation { { b: one * 4 }, { b: one * 1 }, { b: one * 9 } };
    call debug_value{v: (edges tclose) matching from};
    call debug_value{v: (edges tclose) matching to};
    call debug_value{v: join{edges tclose, from}};
    call debug_value{v: join{to, edges tclose}};
    var named := relation { { a: one * 2, name: "two" }, { a: one * 7, name: "seven" } };
    call debug_value{v: (edges tclose) matching named};
    call debug_value{v: join{edges tclose, named}};
    var pairs := relation { { a: one * 1, b: one * 6 }, { a: one * 6, b: one * 1 } };
    call debug_value{v: (edges tclose) matching pairs};
    call debug_value{v: join{pairs, edges tclose}};
    call debug_value{v: (edges tclose) matching relation { { a: one * 1 } }};
    call debug_value{v: (edges tclose) matching (from where a > 100)};
    call debug_value{v: count{r: ((edges where a > 100) tclose) matching from}};
    call debug_value{v: from extend { n := count{r: (edges tclose) matching relation { tuple { a } }} }};
    call debug_value{v: count{r: (edges tclose) not matching from}};
    var nothing := (relation { { x: one } } extend { a := tuple {}, b := tuple {} }) remove {x};
    call debug_value{v: (nothing tclose) matching nothing{a}};
    /* Values that come twice, and that aren't in order in their relation's tuples. */
    var twice := relation {
        { a: one * 2, name: "two" },
        { a: one * 2, name: "zwei" },
        { a: one * 7, name: "seven" },
    };
    call debug_value{v: (edges tclose) matching twice};
    var unordered := relation { { aa: one, b: one * 6 }, { aa: one * 2, b: one * 2 } };
    call debug_value{v: (edges tclose) matching unordered};
    /* Values no arc has, before those that one does, and nothing in common. */
    var absent := relation { { a: one * 0 }, { a: one * 7 } };
    call debug_value{v: (edges tclose) matching absent};
    var gap := relation { { a: one * 1, b: one * 5 }, { a: one * 5, b: one * 6 } };
    call debug_value{v: (gap tclose) matching relation { { a: one * 1 }, { a: one * 3 } }};
    var other := relation { { x: one } };
    call debug_value{v: count{r: times{edges tclose, other}}};
];
"#,
    );
    assert_eq!(code, 0, "{stderr}");
    let integers = "relation { a: integer, b: integer }";
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            format!("{integers} {{ {{ a: 2, b: 4 }}, {{ a: 2, b: 5 }}, {{ a: 2, b: 6 }}, {{ a: 7, b: 7 }} }}"),
            format!("{integers} {{ {{ a: 1, b: 4 }}, {{ a: 2, b: 4 }}, {{ a: 3, b: 4 }}, {{ a: 4, b: 4 }}, {{ a: 5, b: 4 }} }}"),
            format!("{integers} {{ {{ a: 2, b: 4 }}, {{ a: 2, b: 5 }}, {{ a: 2, b: 6 }}, {{ a: 7, b: 7 }} }}"),
            format!("{integers} {{ {{ a: 1, b: 4 }}, {{ a: 2, b: 4 }}, {{ a: 3, b: 4 }}, {{ a: 4, b: 4 }}, {{ a: 5, b: 4 }} }}"),
            format!("{integers} {{ {{ a: 2, b: 4 }}, {{ a: 2, b: 5 }}, {{ a: 2, b: 6 }}, {{ a: 7, b: 7 }} }}"),
            "relation { a: integer, b: integer, name: text } { { a: 2, b: 4, name: \"two\" }, { a: 2, b: 5, name: \"two\" }, { a: 2, b: 6, name: \"two\" }, { a: 7, b: 7, name: \"seven\" } }".to_string(),
            format!("{integers} {{ {{ a: 1, b: 6 }} }}"),
            format!("{integers} {{ {{ a: 1, b: 6 }} }}"),
            format!("{integers} {{ {{ a: 1, b: 2 }}, {{ a: 1, b: 3 }}, {{ a: 1, b: 4 }}, {{ a: 1, b: 5 }}, {{ a: 1, b: 6 }} }}"),
            format!("{integers} {{}}"),
            "0".to_string(),
            "relation { a: integer, n: integer } { { a: 2, n: 3 }, { a: 6, n: 0 }, { a: 7, n: 1 }, { a: 9, n: 0 } }".to_string(),
            "14".to_string(),
            "relation { a: tuple {}, b: tuple {} } { { a: tuple {}, b: tuple {} } }".to_string(),
            format!("{integers} {{ {{ a: 2, b: 4 }}, {{ a: 2, b: 5 }}, {{ a: 2, b: 6 }}, {{ a: 7, b: 7 }} }}"),
            format!("{integers} {{ {{ a: 1, b: 2 }}, {{ a: 1, b: 6 }}, {{ a: 2, b: 6 }}, {{ a: 3, b: 6 }}, {{ a: 4, b: 6 }}, {{ a: 5, b: 6 }} }}"),
            format!("{integers} {{ {{ a: 7, b: 7 }} }}"),
            format!("{integers} {{ {{ a: 1, b: 5 }}, {{ a: 1, b: 6 }} }}"),
            "18".to_string(),
        ]
    );
}

/// More values than a batch of them, of each kind, freed memory being scribbled over,
/// each set of them with what the whole closure has for it, which `not matching` makes.
#[test]
fn a_closure_of_many_values_has_what_the_whole_closure_does() {
    let (code, stderr) = run_source(
        "closure_of_values_kinds",
        r"
operator same_integers{
    edges: relation { a: integer, b: integer },
    from: relation { a: integer },
    to: relation { b: integer },
}: boolean [
    (edges tclose) matching from = (edges tclose) minus ((edges tclose) not matching from)
        and (edges tclose) matching to = (edges tclose) minus ((edges tclose) not matching to)
];

operator same_texts{
    edges: relation { a: text, b: text },
    from: relation { a: text },
    to: relation { b: text },
}: boolean [
    (edges tclose) matching from = (edges tclose) minus ((edges tclose) not matching from)
        and (edges tclose) matching to = (edges tclose) minus ((edges tclose) not matching to)
];

operator main{} [
    var n := 200;
    /* A chain, with a way back near its end, and arcs that skip some of it. */
    var chain := (series{from: 1, to: n - 1} rename {value as a}) extend { b := a + 1 };
    var edges := chain union relation { { a: n, b: n - 20 } } union (((series{from: 1, to: n - 7} rename {value as a}) where a mod 5 = 0) extend { b := a + 7 });
    for size in array [2, 63, 64, 65, 128, 129, 190] loop [
        var from := (series{from: 1, to: size} rename {value as a}) where a mod 1 = 0;
        var to := from rename {a as b};
        call debug_value{v: same_integers{edges, from, to}};
        var named := (edges extend { c := to_text{from: a}, d := to_text{from: b} }){c, d} rename {c as a, d as b};
        var text_from := (from extend { c := to_text{from: a} }){c} rename {c as a};
        call debug_value{v: same_texts{edges: named, from: text_from, to: text_from rename {a as b}}};
    ];
];
",
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stderr.lines().collect::<Vec<_>>(), ["true"; 14]);
}

/// What a closure is asked about can fail, as any operand can.
#[test]
fn a_closure_of_many_values_fails_as_they_do() {
    let (code, stderr) = run_source(
        "closure_of_values_fails",
        r"
operator main{} [
    var one := 1;
    var edges := relation { { a: one, b: one + 1 } };
    var from := relation { { a: one } };
    call debug_value{v: (edges tclose) matching (from where 1 div (a - 1) = 0)};
];
",
    );
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.starts_with("error[R0002]: division by zero"),
        "{stderr}"
    );
}

/// Their paths from or to ten values, of a chain of twenty thousand: with all the
/// closure's 200 million tuples made first, that would come to over 3 GB.
#[test]
fn a_closure_of_many_values_makes_no_more_of_it() {
    let source = "operator main{} [
    var n := 20_000;
    var edges := (series{from: 1, to: n} rename {value as a}) extend { b := a + 1 };
    var values := (series{from: 1, to: 10} rename {value as x}) extend { a := n - x, b := x };
    call debug_value{v: count{r: (edges tclose) matching values{a}}};
    call debug_value{v: count{r: (edges tclose) matching values{b}}};
    call debug_value{v: count{r: join{edges tclose, values{a}}}};
    call debug_value{v: count{r: join{values{b}, edges tclose}}};
];";
    let peak = peak_memory_of("closure_of_many", source, 0);
    assert!(peak < 50 << 20, "{peak} bytes");
}

/// Every value of a graph of a million arcs, each reaching about as many: following
/// the paths from each in turn would take over a minute.
#[test]
fn a_closure_of_every_value_takes_about_what_the_whole_does() {
    let source = "operator main{} [
    var n := 1500;
    var p := series{from: 1, to: n} rename {value as a};
    var q := series{from: 1, to: n} rename {value as b};
    var edges := (p times q) where a < b;
    call debug_value{v: count{r: (edges tclose) matching p}};
    call debug_value{v: count{r: (edges tclose) matching q}};
];";
    let start = std::time::Instant::now();
    let (code, stderr) = run_source("closure_of_every", source);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stderr.lines().collect::<Vec<_>>(), ["1124250", "1124250"]);
    let taken = start.elapsed();
    assert!(taken.as_secs() < 10, "{taken:?}");
}

/// What a closure of many values reads is given up once it has its result, as any
/// operation's operand is: of 64 MB, which with the next operand's would make 192
/// here, rather than 128.
#[test]
fn a_closure_of_many_values_gives_up_its_operand() {
    let source = "operator main{} [
    var n := 40_000;
    var one := 1;
    var t := times{series{from: 1, to: n} rename {value as a}, series{from: 1, to: 100} rename {value as b}};
    var c := relation { { c: one }, { c: one + 1 } };
    call debug_value{
        v: count{
            r: ((t rename {a as c, b as d}) tclose matching c){d} union (t rename {a as c, b as d}){d},
        },
    };
];";
    let peak = peak_memory_of("closure_values_operand", source, 0);
    assert!(peak < 195 << 20, "{peak} bytes");
}

/// What it is asked about too, of 64 MB, which is given up before the next operand.
#[test]
fn a_closure_of_many_values_gives_up_what_it_is_asked_about() {
    let source = "operator main{} [
    var n := 40_000;
    var one := 1;
    var edges := relation { { c: one, d: one + 1 } };
    var t := times{series{from: 1, to: n} rename {value as a}, series{from: 1, to: 100} rename {value as b}};
    call debug_value{
        v: count{
            r: ((edges tclose) matching (t rename {a as c, b as e})){d} union (t rename {a as d, b as e}){d},
        },
    };
];";
    let peak = peak_memory_of("closure_values_other", source, 0);
    assert!(peak < 195 << 20, "{peak} bytes");
}

/// Every kind of reader of a relation, given one nothing need have sorted, or through a
/// rename: its tuples come in another order than canonical, as the key comes first in
/// neither operand, and the others go the other way.
const READERS_OF_A_RELATION: &str = r#"
/* Tuples a join gives in another order than canonical: the key is neither relation's first
   attribute, and the values of the others go the other way. */
type tied { { a: integer, b: integer }, order [desc a, desc b] };

operator counted{r: relation { a: integer, k: integer, z: integer }}: integer [ count{r} ];

operator main{} [
    var one := 1;
    var p := series{from: 1, to: 6} extend { k := (value * one) mod 4, a := 60 - value } remove {value};
    var q := series{from: 1, to: 4} extend { k := (value * one) mod 4, z := 40 - value } remove {value};
    var far := series{from: 1, to: 200} extend { k := (value * one) mod 40, a := 900 - value } remove {value};
    var near := series{from: 1, to: 4} extend { k := value * one } remove {value};

    /* What doesn't depend on the order. */
    call debug_value{v: count{r: p join q}};
    call debug_value{v: is_empty{r: p join q}};
    call debug_value{v: is_empty{r: (p join q) where a < 0}};
    call debug_value{v: sum{r: p join q, of: a}};
    call debug_value{v: product{r: (p join q) where a < 56, of: a}};
    call debug_value{v: min{r: p join q, of: z}};
    call debug_value{v: max{r: p join q, of: z}};
    call debug_value{v: (tuple from (p join q) where a = 59 and z = 39).k};
    call debug_value{v: (p join q){k}};
    call debug_value{v: (p join q){z, k}};
    call debug_value{v: (p join q) where a > 57};
    call debug_value{v: (p join q) rename {a as y}};
    call debug_value{v: (p join q) rename {a as zz}};
    call debug_value{v: (p join q) extend { c := z + a }};
    call debug_value{v: (p join q) replace { a := a div 10, z := z div 10 }};
    call debug_value{v: ((p join q) replace { a := a div 10 }) remove {z}};
    /* Replacing can make two tuples one, and what counts them has to know. */
    call debug_value{v: count{r: (p join q) replace { a := a div 10, z := z div 10 }}};
    call debug_value{v: sum{r: (p join q) replace { a := a div 10, z := z div 10 }, of: k}};
    call debug_value{v: is_empty{r: (p join q) replace { a := 0, z := 0 }}};
    const replaced := (p join q) replace { a := a div 10, z := z div 10 };
    call debug_value{v: count{r: replaced}};

    /* What does: an operand in the order it comes, then what it gives. */
    call debug_value{v: (p join q) join near};
    call debug_value{v: near join (p join q)};
    call debug_value{v: (p join q) matching near};
    call debug_value{v: (p join q) not matching near};
    call debug_value{v: (p join q) union (q join p)};
    call debug_value{v: (p join q) minus ((p join q) where a > 56)};
    call debug_value{v: (p join q) = (q join p)};
    call debug_value{v: (p join q) <= (q join p)};
    call debug_value{v: counted{r: p join q}};
    call debug_value{v: ((p join q){a, k}) tclose};
    call debug_value{v: (p join q) group { g := {z} }};
    call debug_value{v: (((p join q) group { g := {z} }) ungroup {g})};
    call debug_value{v: (p join q) times relation { { t: "x" }, { t: "y" } } where a = 59};
    call debug_table{r: p join q};

    /* Their first attribute the key, as would be in order if they were canonical: they
       come in the order of another. */
    var by_a := relation { { a: one * 59 }, { a: one * 56 } };
    call debug_value{v: (p join q) join by_a};
    call debug_value{v: by_a join (p join q)};
    call debug_value{v: (p join q) matching by_a};
    call debug_value{v: (p join q) not matching by_a};
    call debug_value{v: by_a matching (p join q)};
    call debug_value{v: (p join q) join (p join q){a, k}};
    /* Values a merge would pass, believing they are in order. */
    var passed := relation { { a: one * 55 }, { a: one * 58 } };
    call debug_value{v: (p join q) join passed};
    call debug_value{v: passed join (p join q)};
    call debug_value{v: (p join q) matching passed};
    call debug_value{v: (p join q) not matching passed};
    /* What it makes in order if its operands are, with nothing to say they are. */
    call debug_value{v: (p join q) times relation { { zz: "x" }, { zz: "y" } }};
    call debug_value{v: relation { { aa: "x" }, { aa: "y" } } times (p join q)};
    /* Read by a join while a loop goes through it, which mustn't sort it there. */
    const looping := p join q;
    call debug_value{v: looping extend { c := count{r: looping join by_a} }};

    /* The same, by name, with what reads it two ways, and renames of them. */
    const joined := p join q;
    const by_any := joined{k};
    const renamed := joined rename {a as y};
    const again := renamed join near;
    call debug_value{v: count{r: joined}};
    call debug_value{v: by_any};
    call debug_value{v: again};
    call debug_value{v: joined = (q join p)};
    const only_loose := far join near;
    const chain := only_loose rename {a as y, k as x};
    const further := chain rename {x as k};
    call debug_value{v: count{r: further}};
    call debug_value{v: further{k}};
    call debug_value{v: further join near};

    /* A condition on what is named, which isn't pushed into the join: it is on tuples in
       no order, and those that agree aren't together. A value block's constants are
       bound for what comes after them, as a read-only operator's are. */
    call debug_value{v: [ const named := p join q; named where a > 57 ]};
    call debug_value{v: [ const named := p join q; count{r: named where a > 57} ]};
    call debug_value{v: [ const named := p join q; named where a = 55 ]};
    call debug_value{v: [ const named := p join q; named where a = 55 and z = 38 ]};
    call debug_value{v: [ const named := p join q; named matching by_a where a = 56 ]};
    call debug_value{v: [ const named := p join q; count{r: (named extend { c := a }) where c > 57} ]};
    call debug_value{v: [ const named := p join q; const vs := named where a > 55; vs join by_a ]};
    /* Read one way and another, which sorts it, and by what only counts it. */
    call debug_value{v: [ const named := p join q; count{r: named} + count{r: named where a > 57} ]};
    call debug_value{v: [ const named := p join q; named union named ]};
    call debug_value{v: [ const named := p join q; named = (q join p) ]};
    call debug_value{v: [ const named := p join q; counted{r: named} ]};
    call debug_value{v: [ const named := p join q; is_empty{r: named} ]};
    /* Renames by name, read through and otherwise. */
    call debug_value{v: [ const vl := p rename {a as y}; const vr := q rename {z as b}; vl join vr ]};
    call debug_value{v: [ const vl := p rename {k as x}; const vr := q rename {k as x}; vr matching vl ]};
    call debug_value{v: [ const vl := p rename {a as y}; vl where y > 57 ]};
    call debug_value{v: [ const vl := p rename {a as y}; count{r: vl} + count{r: vl where y > 57} ]};
    call debug_value{v: [ const vl := p rename {a as y}; const va := vl rename {y as yy}; va{yy} ]};
    call debug_value{v: [ const vj := p join q; const vr := vj rename {a as y}; vr join near ]};
    /* A semijoin and what it reads, by name, keep what they were given. */
    call debug_value{v: [ const named := p join q; const vs := named matching by_a; vs rename {a as b} ]};
    call debug_value{v: [ const named := p join q; const vs := named not matching by_a; vs{k} ]};

    /* Through a rename on either side, and both, in each kind of operation. */
    const left := p rename {a as y};
    const right := q rename {z as b};
    call debug_value{v: left join right};
    call debug_value{v: left matching right};
    call debug_value{v: left not matching right};
    call debug_value{v: right matching left};
    call debug_value{v: (p rename {k as x}) join (q rename {k as x})};
    call debug_value{v: (p rename {k as x}) matching (q rename {k as x})};
    call debug_value{v: (p rename {a as k, k as a}) join (q rename {k as a})};
    call debug_value{v: count{r: left}};
    call debug_value{v: is_empty{r: left}};
    call debug_value{v: sum{r: left, of: y}};
    call debug_value{v: left{y}};
    call debug_value{v: (p rename {a as b}){b}};
    /* A name that keeps its place, and one that doesn't: the same tuples. */
    call debug_value{v: (p rename {a as c}) = (p rename {a as c})};
    call debug_value{v: p rename {a as c}};
    call debug_value{v: p rename {a as zz}};

    /* In a type's own order, not as it is stored. */
    var values := series{from: 1, to: 4} extend { c := tied{a: value mod 2, b: 10 - value}, k := value mod 3 } remove {value};
    var keys := relation { { k: one * 0 }, { k: one * 1 }, { k: one * 2 } };
    call debug_value{v: min{r: values join keys, of: c}};
    call debug_value{v: max{r: values join keys, of: c}};
    /* Exact, in any order. */
    var numbers := series{from: 1, to: 6} extend { k := value mod 2, n := to_approximate{from: value} / 7e0 } remove {value};
    var both := relation { { k: one * 0 }, { k: one * 1 } };
    call debug_value{v: sum{r: numbers join both, of: n}};
    call debug_value{v: product{r: numbers join both, of: n}};

    /* Named, as statements are, for what doesn't depend on the order, and what does. */
    const counted_join := p join q;
    call debug_value{v: count{r: counted_join}};
    const printed_join := p join q;
    call debug_value{v: printed_join};
    const restricted_join := (p join q) where a > 56;
    call debug_value{v: sum{r: restricted_join, of: a}};
    const held_semijoin := p matching q;
    call debug_value{v: count{r: held_semijoin join q}};
    const named_rename := p rename {a as w};
    call debug_value{v: count{r: named_rename join q}};
    const read_as_renamed := p rename {a as w};
    call debug_value{v: read_as_renamed};
    const read_twice := p join q;
    call debug_value{v: count{r: read_twice}};
    call debug_value{v: read_twice};
    const failing_join := p join q;
    call debug_value{v: count{r: failing_join where a + one > 0}};
    var shrunk := p join q;
    shrunk := shrunk where a > 56;
    call debug_value{v: count{r: shrunk}};
    var joined_again := p join q;
    joined_again := joined_again join near;
    call debug_value{v: count{r: joined_again}};
    var assigned_anew := p join q;
    assigned_anew := far join q;
    call debug_value{v: count{r: assigned_anew}};
    /* Nothing above wants them in order, however deep they are: joined by an attribute that is
       first in what isn't in order by it, which has to find that out. `p join q` is in order of
       `k`, and comes first by `a`. */
    var by_first := series{from: 1, to: 3} extend { a := 53 + value * one, y := 90 + value } remove {value};
    var few := series{from: 1, to: 2} extend { a := 55 + value * one } remove {value};
    call debug_value{v: count{r: (p join q) join by_first}};
    call debug_value{v: sum{r: (p join q) join by_first, of: y}};
    call debug_value{v: count{r: by_first join (p join q)}};
    call debug_value{v: count{r: ((p join q) join by_first){a, y}}};
    call debug_value{v: is_empty{r: (p join q) join by_first}};
    call debug_value{v: count{r: ((p join q) where a > 55) join by_first}};
    call debug_value{v: count{r: ((p join q) where a = 57) join by_first}};
    call debug_value{v: count{r: (p join q) where a = 57}};
    call debug_value{v: sum{r: (p join q) where a = 57, of: z}};
    call debug_value{v: count{r: ((p join q) where a = 57 and z > 36)}};
    call debug_value{v: count{r: ((p join q) matching few) join by_first}};
    call debug_value{v: count{r: ((p join q) not matching few) join by_first}};
    call debug_value{v: count{r: (p join q) matching by_first}};
    call debug_value{v: count{r: by_first matching (p join q)}};
    call debug_value{v: count{r: by_first not matching (p join q)}};
    call debug_value{v: count{r: ((p join q) rename {z as zed}) join by_first}};
    call debug_value{v: count{r: ((p join q) rename {a as aa, z as zz}) join by_first rename {a as aa}}};
    call debug_value{v: count{r: ((p join q) extend { w := a * 2 }) join by_first}};
    call debug_value{v: sum{r: ((p join q) extend { w := a + z }) join by_first, of: w}};
    call debug_value{v: count{r: ((p join q) times few rename {a as other}) join by_first}};
    call debug_value{v: count{r: (p join q){a, z} join by_first}};
    const held_join := p join q;
    call debug_value{v: count{r: held_join join by_first}};
    const held_semi := (p join q) matching few;
    call debug_value{v: count{r: held_semi join by_first}};
    const held_where := (p join q) where a > 55;
    call debug_value{v: count{r: held_where join by_first}};
    const held_renamed := (p join q) rename {z as zed};
    call debug_value{v: count{r: held_renamed join by_first}};
    const held_for_equal := p join q;
    call debug_value{v: count{r: held_for_equal where a = 57}};
    const held_for_equal_and := p join q;
    call debug_value{v: sum{r: held_for_equal_and where a = 57 and z > 36, of: z}};
    const held_moved := (p join q) rename {a as zz, z as aa};
    call debug_value{v: count{r: held_moved join by_first rename {a as zz}}};
    const held_moved_read := (p join q) rename {a as zz, z as aa};
    call debug_value{v: sum{r: held_moved_read, of: aa}};
    const held_extended := (p join q) extend { w := a * 2 };
    call debug_value{v: count{r: held_extended join by_first}};
    const held_in_a_value := [ const inner := p join q; const narrowed := inner where a > 55; count{r: narrowed join by_first} ];
    call debug_value{v: held_in_a_value};
    /* Of what can't be sorted in place, for pointing to something. */
    var ps := p extend { t := to_text{from: a} };
    var bys := by_first extend { u := to_text{from: y} };
    call debug_value{v: count{r: (ps join q) join bys}};
    call debug_value{v: sum{r: (ps join q) join bys, of: y}};
    call debug_value{v: count{r: ((ps join q) where a > 55) join bys}};
    call debug_value{v: count{r: (ps join q) matching bys}};
    const held_text := ps join q;
    call debug_value{v: count{r: held_text join bys}};
    /* A rename of something else, read once, in any order. */
    const of_far := far rename {a as w};
    call debug_value{v: count{r: of_far join near}};
    call debug_value{v: count{r: (far rename {a as w}) matching near}};
    /* Tuples, renamed and joined, have no order. */
    var one_tuple := tuple { a: one, b: one + 1 };
    call debug_value{v: (one_tuple rename {a as c}).c};
    /* Who wants order, and gets it, through what passes it down. */
    call debug_value{v: (p join q) join by_first};
    call debug_value{v: ((p join q) where a > 55) join by_first};
    call debug_value{v: (p join q) matching by_first};
    call debug_value{v: ((p join q) rename {z as zed}) join by_first};
    var total := 0;
    var i := 0;
    while i < 3 loop [
        const each_round := p join q;
        const renamed_each_round := each_round rename {a as w};
        total := total * 10 + count{r: renamed_each_round where w > 56 + i};
        i := i + 1;
    ];
    call debug_value{v: total};
    transaction [
        const in_a_transaction := p join q;
        call debug_value{v: count{r: in_a_transaction}};
    ];
    if count{r: p} > one then [
        const in_a_branch := p join q;
        call debug_value{v: count{r: in_a_branch}};
    ];
];
"#;

/// What the compiler before relations could be read in any order wrote for
/// [`READERS_OF_A_RELATION`]: it depends on no such thing.
const WRITTEN_BY_READERS: &[&str] = &[
    "6",
    "false",
    "true",
    "339",
    "2970",
    "36",
    "39",
    "1",
    "relation { k: integer } { { k: 0 }, { k: 1 }, { k: 2 }, { k: 3 } }",
    "relation { k: integer, z: integer } { { k: 0, z: 36 }, { k: 1, z: 39 }, { k: 2, z: 38 }, { k: 3, z: 37 } }",
    "relation { a: integer, k: integer, z: integer } { { a: 58, k: 2, z: 38 }, { a: 59, k: 1, z: 39 } }",
    "relation { k: integer, y: integer, z: integer } { { k: 0, y: 56, z: 36 }, { k: 1, y: 55, z: 39 }, { k: 1, y: 59, z: 39 }, { k: 2, y: 54, z: 38 }, { k: 2, y: 58, z: 38 }, { k: 3, y: 57, z: 37 } }",
    "relation { k: integer, z: integer, zz: integer } { { k: 0, z: 36, zz: 56 }, { k: 1, z: 39, zz: 55 }, { k: 1, z: 39, zz: 59 }, { k: 2, z: 38, zz: 54 }, { k: 2, z: 38, zz: 58 }, { k: 3, z: 37, zz: 57 } }",
    "relation { a: integer, c: integer, k: integer, z: integer } { { a: 54, c: 92, k: 2, z: 38 }, { a: 55, c: 94, k: 1, z: 39 }, { a: 56, c: 92, k: 0, z: 36 }, { a: 57, c: 94, k: 3, z: 37 }, { a: 58, c: 96, k: 2, z: 38 }, { a: 59, c: 98, k: 1, z: 39 } }",
    "relation { a: integer, k: integer, z: integer } { { a: 5, k: 0, z: 3 }, { a: 5, k: 1, z: 3 }, { a: 5, k: 2, z: 3 }, { a: 5, k: 3, z: 3 } }",
    "relation { a: integer, k: integer } { { a: 5, k: 0 }, { a: 5, k: 1 }, { a: 5, k: 2 }, { a: 5, k: 3 } }",
    "4",
    "6",
    "false",
    "4",
    "relation { a: integer, k: integer, z: integer } { { a: 54, k: 2, z: 38 }, { a: 55, k: 1, z: 39 }, { a: 57, k: 3, z: 37 }, { a: 58, k: 2, z: 38 }, { a: 59, k: 1, z: 39 } }",
    "relation { a: integer, k: integer, z: integer } { { a: 54, k: 2, z: 38 }, { a: 55, k: 1, z: 39 }, { a: 57, k: 3, z: 37 }, { a: 58, k: 2, z: 38 }, { a: 59, k: 1, z: 39 } }",
    "relation { a: integer, k: integer, z: integer } { { a: 54, k: 2, z: 38 }, { a: 55, k: 1, z: 39 }, { a: 57, k: 3, z: 37 }, { a: 58, k: 2, z: 38 }, { a: 59, k: 1, z: 39 } }",
    "relation { a: integer, k: integer, z: integer } { { a: 56, k: 0, z: 36 } }",
    "relation { a: integer, k: integer, z: integer } { { a: 54, k: 2, z: 38 }, { a: 55, k: 1, z: 39 }, { a: 56, k: 0, z: 36 }, { a: 57, k: 3, z: 37 }, { a: 58, k: 2, z: 38 }, { a: 59, k: 1, z: 39 } }",
    "relation { a: integer, k: integer, z: integer } { { a: 54, k: 2, z: 38 }, { a: 55, k: 1, z: 39 }, { a: 56, k: 0, z: 36 } }",
    "true",
    "true",
    "6",
    "relation { a: integer, k: integer } { { a: 54, k: 2 }, { a: 55, k: 1 }, { a: 56, k: 0 }, { a: 57, k: 3 }, { a: 58, k: 2 }, { a: 59, k: 1 } }",
    "relation { a: integer, g: relation { z: integer }, k: integer } { { a: 54, g: relation { z: integer } { { z: 38 } }, k: 2 }, { a: 55, g: relation { z: integer } { { z: 39 } }, k: 1 }, { a: 56, g: relation { z: integer } { { z: 36 } }, k: 0 }, { a: 57, g: relation { z: integer } { { z: 37 } }, k: 3 }, { a: 58, g: relation { z: integer } { { z: 38 } }, k: 2 }, { a: 59, g: relation { z: integer } { { z: 39 } }, k: 1 } }",
    "relation { a: integer, k: integer, z: integer } { { a: 54, k: 2, z: 38 }, { a: 55, k: 1, z: 39 }, { a: 56, k: 0, z: 36 }, { a: 57, k: 3, z: 37 }, { a: 58, k: 2, z: 38 }, { a: 59, k: 1, z: 39 } }",
    r#"relation { a: integer, k: integer, t: text, z: integer } { { a: 59, k: 1, t: "x", z: 39 }, { a: 59, k: 1, t: "y", z: 39 } }"#,
    "┌────┬───┬────┐",
    "│ a  │ k │ z  │",
    "├────┼───┼────┤",
    "│ 54 │ 2 │ 38 │",
    "│ 55 │ 1 │ 39 │",
    "│ 56 │ 0 │ 36 │",
    "│ 57 │ 3 │ 37 │",
    "│ 58 │ 2 │ 38 │",
    "│ 59 │ 1 │ 39 │",
    "└────┴───┴────┘",
    "relation { a: integer, k: integer, z: integer } { { a: 56, k: 0, z: 36 }, { a: 59, k: 1, z: 39 } }",
    "relation { a: integer, k: integer, z: integer } { { a: 56, k: 0, z: 36 }, { a: 59, k: 1, z: 39 } }",
    "relation { a: integer, k: integer, z: integer } { { a: 56, k: 0, z: 36 }, { a: 59, k: 1, z: 39 } }",
    "relation { a: integer, k: integer, z: integer } { { a: 54, k: 2, z: 38 }, { a: 55, k: 1, z: 39 }, { a: 57, k: 3, z: 37 }, { a: 58, k: 2, z: 38 } }",
    "relation { a: integer } { { a: 56 }, { a: 59 } }",
    "relation { a: integer, k: integer, z: integer } { { a: 54, k: 2, z: 38 }, { a: 55, k: 1, z: 39 }, { a: 56, k: 0, z: 36 }, { a: 57, k: 3, z: 37 }, { a: 58, k: 2, z: 38 }, { a: 59, k: 1, z: 39 } }",
    "relation { a: integer, k: integer, z: integer } { { a: 55, k: 1, z: 39 }, { a: 58, k: 2, z: 38 } }",
    "relation { a: integer, k: integer, z: integer } { { a: 55, k: 1, z: 39 }, { a: 58, k: 2, z: 38 } }",
    "relation { a: integer, k: integer, z: integer } { { a: 55, k: 1, z: 39 }, { a: 58, k: 2, z: 38 } }",
    "relation { a: integer, k: integer, z: integer } { { a: 54, k: 2, z: 38 }, { a: 56, k: 0, z: 36 }, { a: 57, k: 3, z: 37 }, { a: 59, k: 1, z: 39 } }",
    r#"relation { a: integer, k: integer, z: integer, zz: text } { { a: 54, k: 2, z: 38, zz: "x" }, { a: 54, k: 2, z: 38, zz: "y" }, { a: 55, k: 1, z: 39, zz: "x" }, { a: 55, k: 1, z: 39, zz: "y" }, { a: 56, k: 0, z: 36, zz: "x" }, { a: 56, k: 0, z: 36, zz: "y" }, { a: 57, k: 3, z: 37, zz: "x" }, { a: 57, k: 3, z: 37, zz: "y" }, { a: 58, k: 2, z: 38, zz: "x" }, { a: 58, k: 2, z: 38, zz: "y" }, { a: 59, k: 1, z: 39, zz: "x" }, { a: 59, k: 1, z: 39, zz: "y" } }"#,
    r#"relation { a: integer, aa: text, k: integer, z: integer } { { a: 54, aa: "x", k: 2, z: 38 }, { a: 54, aa: "y", k: 2, z: 38 }, { a: 55, aa: "x", k: 1, z: 39 }, { a: 55, aa: "y", k: 1, z: 39 }, { a: 56, aa: "x", k: 0, z: 36 }, { a: 56, aa: "y", k: 0, z: 36 }, { a: 57, aa: "x", k: 3, z: 37 }, { a: 57, aa: "y", k: 3, z: 37 }, { a: 58, aa: "x", k: 2, z: 38 }, { a: 58, aa: "y", k: 2, z: 38 }, { a: 59, aa: "x", k: 1, z: 39 }, { a: 59, aa: "y", k: 1, z: 39 } }"#,
    "relation { a: integer, c: integer, k: integer, z: integer } { { a: 54, c: 2, k: 2, z: 38 }, { a: 55, c: 2, k: 1, z: 39 }, { a: 56, c: 2, k: 0, z: 36 }, { a: 57, c: 2, k: 3, z: 37 }, { a: 58, c: 2, k: 2, z: 38 }, { a: 59, c: 2, k: 1, z: 39 } }",
    "6",
    "relation { k: integer } { { k: 0 }, { k: 1 }, { k: 2 }, { k: 3 } }",
    "relation { k: integer, y: integer, z: integer } { { k: 1, y: 55, z: 39 }, { k: 1, y: 59, z: 39 }, { k: 2, y: 54, z: 38 }, { k: 2, y: 58, z: 38 }, { k: 3, y: 57, z: 37 } }",
    "true",
    "20",
    "relation { k: integer } { { k: 1 }, { k: 2 }, { k: 3 }, { k: 4 } }",
    "relation { k: integer, y: integer } { { k: 1, y: 739 }, { k: 1, y: 779 }, { k: 1, y: 819 }, { k: 1, y: 859 }, { k: 1, y: 899 }, { k: 2, y: 738 }, { k: 2, y: 778 }, { k: 2, y: 818 }, { k: 2, y: 858 }, { k: 2, y: 898 }, { k: 3, y: 737 }, { k: 3, y: 777 }, { k: 3, y: 817 }, { k: 3, y: 857 }, { k: 3, y: 897 }, { k: 4, y: 736 }, { k: 4, y: 776 }, { k: 4, y: 816 }, { k: 4, y: 856 }, { k: 4, y: 896 } }",
    "relation { a: integer, k: integer, z: integer } { { a: 58, k: 2, z: 38 }, { a: 59, k: 1, z: 39 } }",
    "2",
    "relation { a: integer, k: integer, z: integer } { { a: 55, k: 1, z: 39 } }",
    "relation { a: integer, k: integer, z: integer } {}",
    "relation { a: integer, k: integer, z: integer } { { a: 56, k: 0, z: 36 } }",
    "2",
    "relation { a: integer, k: integer, z: integer } { { a: 56, k: 0, z: 36 }, { a: 59, k: 1, z: 39 } }",
    "8",
    "relation { a: integer, k: integer, z: integer } { { a: 54, k: 2, z: 38 }, { a: 55, k: 1, z: 39 }, { a: 56, k: 0, z: 36 }, { a: 57, k: 3, z: 37 }, { a: 58, k: 2, z: 38 }, { a: 59, k: 1, z: 39 } }",
    "true",
    "6",
    "false",
    "relation { b: integer, k: integer, y: integer } { { b: 36, k: 0, y: 56 }, { b: 37, k: 3, y: 57 }, { b: 38, k: 2, y: 54 }, { b: 38, k: 2, y: 58 }, { b: 39, k: 1, y: 55 }, { b: 39, k: 1, y: 59 } }",
    "relation { x: integer, z: integer } { { x: 0, z: 36 }, { x: 1, z: 39 }, { x: 2, z: 38 }, { x: 3, z: 37 } }",
    "relation { k: integer, y: integer } { { k: 1, y: 59 }, { k: 2, y: 58 } }",
    "8",
    "relation { yy: integer } { { yy: 54 }, { yy: 55 }, { yy: 56 }, { yy: 57 }, { yy: 58 }, { yy: 59 } }",
    "relation { k: integer, y: integer, z: integer } { { k: 1, y: 55, z: 39 }, { k: 1, y: 59, z: 39 }, { k: 2, y: 54, z: 38 }, { k: 2, y: 58, z: 38 }, { k: 3, y: 57, z: 37 } }",
    "relation { b: integer, k: integer, z: integer } { { b: 56, k: 0, z: 36 }, { b: 59, k: 1, z: 39 } }",
    "relation { k: integer } { { k: 1 }, { k: 2 }, { k: 3 } }",
    "relation { b: integer, k: integer, y: integer } { { b: 36, k: 0, y: 56 }, { b: 37, k: 3, y: 57 }, { b: 38, k: 2, y: 54 }, { b: 38, k: 2, y: 58 }, { b: 39, k: 1, y: 55 }, { b: 39, k: 1, y: 59 } }",
    "relation { k: integer, y: integer } { { k: 0, y: 56 }, { k: 1, y: 55 }, { k: 1, y: 59 }, { k: 2, y: 54 }, { k: 2, y: 58 }, { k: 3, y: 57 } }",
    "relation { k: integer, y: integer } {}",
    "relation { b: integer, k: integer } { { b: 36, k: 0 }, { b: 37, k: 3 }, { b: 38, k: 2 }, { b: 39, k: 1 } }",
    "relation { a: integer, x: integer, z: integer } { { a: 54, x: 2, z: 38 }, { a: 55, x: 1, z: 39 }, { a: 56, x: 0, z: 36 }, { a: 57, x: 3, z: 37 }, { a: 58, x: 2, z: 38 }, { a: 59, x: 1, z: 39 } }",
    "relation { a: integer, x: integer } { { a: 54, x: 2 }, { a: 55, x: 1 }, { a: 56, x: 0 }, { a: 57, x: 3 }, { a: 58, x: 2 }, { a: 59, x: 1 } }",
    "relation { a: integer, k: integer, z: integer } { { a: 0, k: 56, z: 36 }, { a: 1, k: 55, z: 39 }, { a: 1, k: 59, z: 39 }, { a: 2, k: 54, z: 38 }, { a: 2, k: 58, z: 38 }, { a: 3, k: 57, z: 37 } }",
    "6",
    "false",
    "339",
    "relation { y: integer } { { y: 54 }, { y: 55 }, { y: 56 }, { y: 57 }, { y: 58 }, { y: 59 } }",
    "relation { b: integer } { { b: 54 }, { b: 55 }, { b: 56 }, { b: 57 }, { b: 58 }, { b: 59 } }",
    "true",
    "relation { c: integer, k: integer } { { c: 54, k: 2 }, { c: 55, k: 1 }, { c: 56, k: 0 }, { c: 57, k: 3 }, { c: 58, k: 2 }, { c: 59, k: 1 } }",
    "relation { k: integer, zz: integer } { { k: 0, zz: 56 }, { k: 1, zz: 55 }, { k: 1, zz: 59 }, { k: 2, zz: 54 }, { k: 2, zz: 58 }, { k: 3, zz: 57 } }",
    "tied{a: 1, b: 9}",
    "tied{a: 0, b: 6}",
    "3e0",
    "6.119899021666141e-3",
    "6",
    "relation { a: integer, k: integer, z: integer } { { a: 54, k: 2, z: 38 }, { a: 55, k: 1, z: 39 }, { a: 56, k: 0, z: 36 }, { a: 57, k: 3, z: 37 }, { a: 58, k: 2, z: 38 }, { a: 59, k: 1, z: 39 } }",
    "174",
    "6",
    "6",
    "relation { k: integer, w: integer } { { k: 0, w: 56 }, { k: 1, w: 55 }, { k: 1, w: 59 }, { k: 2, w: 54 }, { k: 2, w: 58 }, { k: 3, w: 57 } }",
    "6",
    "relation { a: integer, k: integer, z: integer } { { a: 54, k: 2, z: 38 }, { a: 55, k: 1, z: 39 }, { a: 56, k: 0, z: 36 }, { a: 57, k: 3, z: 37 }, { a: 58, k: 2, z: 38 }, { a: 59, k: 1, z: 39 } }",
    "6",
    "3",
    "5",
    "20",
    "3",
    "276",
    "3",
    "3",
    "false",
    "1",
    "0",
    "1",
    "37",
    "1",
    "1",
    "2",
    "3",
    "3",
    "0",
    "3",
    "3",
    "3",
    "278",
    "6",
    "3",
    "3",
    "1",
    "1",
    "3",
    "1",
    "37",
    "3",
    "227",
    "3",
    "1",
    "3",
    "276",
    "1",
    "3",
    "3",
    "20",
    "20",
    "1",
    "relation { a: integer, k: integer, y: integer, z: integer } { { a: 54, k: 2, y: 91, z: 38 }, { a: 55, k: 1, y: 92, z: 39 }, { a: 56, k: 0, y: 93, z: 36 } }",
    "relation { a: integer, k: integer, y: integer, z: integer } { { a: 56, k: 0, y: 93, z: 36 } }",
    "relation { a: integer, k: integer, z: integer } { { a: 54, k: 2, z: 38 }, { a: 55, k: 1, z: 39 }, { a: 56, k: 0, z: 36 } }",
    "relation { a: integer, k: integer, y: integer, zed: integer } { { a: 54, k: 2, y: 91, zed: 38 }, { a: 55, k: 1, y: 92, zed: 39 }, { a: 56, k: 0, y: 93, zed: 36 } }",
    "321",
    "6",
    "6",
];

#[test]
fn a_relation_read_in_any_order_gives_what_it_did_in_canonical_order() {
    let (code, stderr) = run_source("readers_of_a_relation", READERS_OF_A_RELATION);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stderr.lines().collect::<Vec<_>>(), WRITTEN_BY_READERS);
}

/// What can fail for a tuple says which, so it has to come to the same one first: the
/// first in canonical order, whose `a` is 51, of a join whose key has it in its second run,
/// so that it would give others first, as anything but the failure needn't sort it.
#[test]
fn what_fails_for_a_tuple_fails_for_the_same_one_in_any_order() {
    for (name, value) in [
        (
            "where",
            "count{r: (p join q) where 9223372036854775800 + a > 0}",
        ),
        (
            "extend",
            "count{r: (p join q) extend { c := 9223372036854775800 + a }}",
        ),
        (
            "replace",
            "count{r: (p join q) replace { a := 9223372036854775800 + a }}",
        ),
        ("constant", "count{r: j where 9223372036854775800 + a > 0}"),
        (
            "semijoin",
            "count{r: (p matching q) where 9223372036854775800 + a > 0}",
        ),
    ] {
        let source = format!(
            "operator main{{}} [
    var one := 1;
    var p := series{{from: 1, to: 9}} extend {{ k := (value * one + 1) mod 3, a := 60 - value }} remove {{value}};
    var q := series{{from: 1, to: 4}} extend {{ k := (value * one) mod 3, z := 40 - value }} remove {{value}};
    const j := p join q;
    call debug_value{{v: count{{r: j}}}};
    call debug_value{{v: {value}}};
];
"
        );
        let (code, stderr) = run_source(&format!("fails_in_order_{name}"), &source);
        assert_eq!(code, 1, "{name}: {stderr}");
        let failure = stderr.lines().nth(1).unwrap();
        assert_eq!(
            failure, "error[R0001]: overflow: 9223372036854775800 + 51 is out of its type's range",
            "{name}"
        );
    }
}

/// A closure is found from what it is matched with, in canonical order, whatever that is
/// named: its binding is read once, gives up its reference once, and is right.
#[test]
fn a_closure_matched_with_a_named_join_gives_it_up_once() {
    let source = "operator closed{arcs: relation { x: integer, z: integer }, n: integer}: integer [
    const left := series{from: 1, to: 5 * n} extend { z := value mod 3 } remove {value};
    const right := series{from: 1, to: n} extend { z := value mod 997, k := value mod 11 } remove {value};
    const side := left join right;
    const found := (arcs tclose) join side;
    n * count{r: found}
];

operator main{} [
    const arcs := series{from: 1, to: 20} extend { x := (value * 3) mod 5, z := (value * 7) mod 3 } remove {value};
    var total := 0;
    var n := 1;
    while n <= 40 loop [
        total := total + closed{arcs, n};
        n := n + 1;
    ];
    call debug_value{v: total};
];
";
    let (code, stderr) = run_source("closure_named_join", source);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stderr.trim(), "8195");
}

/// A local variable of a body with statements lasts no longer than the last statement that
/// mentions it, as a binding does: of six, each read only by the next, at most two are held,
/// of 16 and 96 MB at the end, where all of them come to 336 MB.
#[test]
fn a_local_variable_lasts_no_longer_than_its_last_use() {
    let chain =
        "const a := series{from: 1, to: 2000000} extend { k := value * one } remove {value};
    const b := a extend { w1 := k + 1 };
    const c := b extend { w2 := k + 2 };
    const d := c extend { w3 := k + 3 };
    const e := d extend { w4 := k + 4 };
    const f := e extend { w5 := k + 5 };";
    for (name, source) in [
        (
            "in_main",
            format!(
                "operator main{{}} [
    var one := 1;
    {chain}
    call debug_value{{v: count{{r: f}}}};
];"
            ),
        ),
        (
            "in_a_body_with_statements",
            format!(
                "operator g{{one: integer}}: integer [
    var ran := 0;
    while ran < 1 loop [
        ran := ran + 1;
    ];
    {chain}
    count{{r: f}}
];

operator main{{}} [
    var one := 1;
    call debug_value{{v: g{{one}}}};
];"
            ),
        ),
        (
            "in_an_update_operator",
            format!(
                "operator g{{one: integer, out n: integer}} [
    {chain}
    n := count{{r: f}};
    call debug_value{{v: n}};
];

operator main{{}} [
    var one := 1;
    var n := 0;
    call g{{one, out n}};
];"
            ),
        ),
    ] {
        let peak = peak_memory_of(&format!("local_last_use_{name}"), &source, 0);
        assert!(peak < 230 << 20, "{name}: {peak} bytes");
    }
}

/// What a local variable holds lasts as long as anything can read it: a later statement, a
/// loop that goes round again, a branch, what a body ends with, or a transaction that can
/// give it back. Each value here points to something, so that one given up early would show.
#[test]
fn a_local_variable_lasts_as_long_as_anything_reads_it() {
    let source = r#"operator ends_with_it{p: relation { a: integer }}: text [
    var ran := 0;
    while ran < 1 loop [
        ran := ran + 1;
    ];
    const held := to_text{from: count{r: p}} || " held";
    const unread := to_text{from: count{r: p}} || " unread";
    var seen := unread;
    seen := held;
    seen
];

operator of_a_tuple{}: text [
    var ran := 0;
    while ran < 1 loop [
        ran := ran + 1;
    ];
    const t := tuple { a: to_text{from: ran} || " in a tuple", b: 2 };
    const a := t.a;
    const later := a || " later";
    later
];

operator gives{out said: text} [
    const made := "made " || to_text{from: 1 + 1};
    said := made;
];

operator changes{var v: relation { a: integer }} [
    const was := v;
    v := v where a > 2;
    call debug_value{v: count{r: was}};
];

operator main{} [
    const p := series{from: 1, to: 5} rename {value as a};
    const in_loop := to_text{from: count{r: p}} || " in a loop";
    const in_branch := to_text{from: count{r: p}} || " in a branch";
    const for_array := array [to_text{from: 1 + 0} || " of", to_text{from: 1 + 1} || " of"];
    const after := to_text{from: 7 * 6} || " after";
    var i := 0;
    while i < 3 loop [
        const each := in_loop || " " || to_text{from: i};
        call debug_value{v: each};
        i := i + 1;
    ];
    if count{r: p} > 1 then [
        call debug_value{v: in_branch};
    ] else [
        call debug_value{v: "not " || in_branch};
    ];
    for element in for_array loop [
        call debug_value{v: element};
    ];
    call debug_value{v: ends_with_it{p}};
    call debug_value{v: of_a_tuple{}};
    var said := "";
    call gives{out said};
    call debug_value{v: said};
    var v := p;
    call changes{var v};
    call debug_value{v: count{r: v}};
    var kept := to_text{from: 1 + 1} || " kept";
    var found: relation transaction_collection;
    transaction collecting found [
        kept := to_text{from: 1 + 2} || " changed";
        const inside := kept || " inside";
        call debug_value{v: inside};
        fail failure{message: "undone"};
    ];
    call debug_value{v: kept};
    call debug_value{v: count{r: found}};
    call debug_value{v: after};
];
"#;
    let (code, stderr) = run_source("local_last_use_reads", source);
    assert_eq!(code, 0, "{stderr}");
    let written: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        written,
        [
            r#""5 in a loop 0""#,
            r#""5 in a loop 1""#,
            r#""5 in a loop 2""#,
            r#""5 in a branch""#,
            r#""1 of""#,
            r#""2 of""#,
            r#""5 held""#,
            r#""1 in a tuple later""#,
            r#""made 2""#,
            "5",
            "3",
            r#""3 changed inside""#,
            r#""2 kept""#,
            "1",
            r#""42 after""#,
        ],
        "{stderr}"
    );
}

/// What `save_text_file` writes is what `load_text_file` reads back, line for line, each
/// ending in a line feed, however few there are, and whatever a line holds that isn't a line
/// end, and what the file held before is gone.
#[test]
fn saved_lines_read_back_as_they_were() {
    for (name, lines, bytes) in [
        ("none", "array text []", ""),
        ("one_empty", r#"array [""]"#, "\n"),
        ("empty_last", r#"array ["Nile", ""]"#, "Nile\n\n"),
        ("empty_first", r#"array ["", "Nile"]"#, "\nNile\n"),
        (
            "of_the_world",
            r#"array ["Paraná", "长江", "नर्मदा", "Волга", "ᎤᏔᎾ"]"#,
            "Paraná\n长江\nनर्मदा\nВолга\nᎤᏔᎾ\n",
        ),
        (
            "not_line_ends",
            r#"array ["a\rb", "\ra", "\"quoted\"", " spaced "]"#,
            "a\rb\n\ra\n\"quoted\"\n spaced \n",
        ),
    ] {
        let path = saved_file(
            &format!("{name}.txt"),
            Some("what the file held, longer than anything written over it, which goes"),
        );
        let source = format!(
            r#"uses {{ trendy.files }};

operator main{{}} [
    const lines := {lines};
    call save_text_file{{path: "{path}", lines}};
    var read: array text;
    call load_text_file{{path: "{path}", out lines: read}};
    call debug_value{{v: read = lines}};
    call debug_value{{v: length{{a: read}}}};
];
"#,
            path = path.display()
        );
        let (code, stderr) = run_source(&format!("saved_{name}"), &source);
        assert_eq!(code, 0, "{name}: {stderr}");
        let written: Vec<&str> = stderr.lines().collect();
        assert_eq!(written[0], "true", "{name}: {stderr}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes, "{name}");
    }
}

/// More than the buffer a file is written through holds, in lines that fit it, and one that
/// doesn't, comes out whole.
#[test]
fn a_large_file_is_saved_whole() {
    let path = saved_file("large.txt", None);
    let source = format!(
        r#"uses {{ trendy.files }};

operator main{{}} [
    const numbers := series{{from: 1, to: 50000}};
    var lines: array text;
    load lines value from (numbers extend {{ line := to_text{{from: value}} }}) order [asc value] remove {{value}};
    var long := "";
    var i := 0;
    while i < 18 loop [
        long := long || long || "river ";
        i := i + 1;
    ];
    call save_text_file{{path: "{path}", lines: array [long] || lines || array [long]}};
    var read: array text;
    call load_text_file{{path: "{path}", out lines: read}};
    call debug_value{{v: length{{a: read}}}};
    call debug_value{{v: read[2] || read[50001]}};
    call debug_value{{v: read[1] = long and read[50002] = long}};
];
"#,
        path = path.display()
    );
    let (code, stderr) = run_source("saved_large", &source);
    assert_eq!(code, 0, "{stderr}");
    let written: Vec<&str> = stderr.lines().collect();
    assert_eq!(written, ["50002", r#""150000""#, "true"], "{stderr}");
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.len() > 3_000_000, "{}", saved.len());
    assert_eq!(saved.lines().count(), 50002);
    assert!(saved.ends_with('\n'));
}

/// A file that can't be written stops the program, saying why, or gives its error if the call
/// names `out errors`, as reading does.
#[test]
fn a_file_that_cant_be_written_says_why() {
    use trendy_diag::Code;
    let dir = saved_file("a_file", Some("in the way"));
    let dir = dir.parent().unwrap().display().to_string();
    for (name, path, reason) in [
        (
            "no_directory",
            format!("{dir}/no/such/directory.txt"),
            "No such file or directory",
        ),
        ("a_directory", dir.clone(), "Is a directory"),
        (
            "under_a_file",
            format!("{dir}/a_file/below.txt"),
            "Not a directory",
        ),
        (
            "with_a_nul",
            format!(r"{dir}/a\u{{0}}b.txt"),
            "a path can't contain the character U+0000",
        ),
    ] {
        let stops = format!(
            r#"uses {{ trendy.files }};

operator main{{}} [
    call save_text_file{{path: "{path}", lines: array ["Nile"]}};
    call debug_value{{v: "after"}};
];
"#
        );
        let (code, stderr) = run_source(&format!("unwritable_{name}"), &stops);
        assert_eq!(code, 1, "{name}: {stderr}");
        let id = Code::UnwritableFile.id();
        assert!(
            stderr.contains(&format!("error[{id}]: can't write ")),
            "{name}: {stderr}"
        );
        assert!(stderr.contains(reason), "{name}: {stderr}");
        assert!(!stderr.contains("after"), "{name}: {stderr}");
        let gives = format!(
            r#"uses {{ trendy.files }};

operator main{{}} [
    var errors: relation {{ error: error }};
    call save_text_file{{path: "{path}", lines: array ["Nile"], out errors}};
    call debug_value{{v: count{{r: errors}}}};
    call debug_value{{v: errors}};
    call debug_value{{v: "after"}};
];
"#
        );
        let (code, stderr) = run_source(&format!("unwritable_gives_{name}"), &gives);
        assert_eq!(code, 0, "{name}: {stderr}");
        let written: Vec<&str> = stderr.lines().collect();
        assert_eq!(written[0], "1", "{name}: {stderr}");
        assert!(
            written[1].contains("unwritable_file{path: "),
            "{name}: {stderr}"
        );
        assert!(written[1].contains(reason), "{name}: {stderr}");
        assert_eq!(written[2], r#""after""#, "{name}: {stderr}");
    }
    assert_eq!(
        std::fs::read_to_string(format!("{dir}/a_file")).unwrap(),
        "in the way"
    );
}

/// With nothing wrong, `out errors` is empty, and the file written.
#[test]
fn a_file_written_gives_no_errors() {
    let path = saved_file("no_errors.txt", None);
    let source = format!(
        r#"uses {{ trendy.files }};

operator main{{}} [
    var errors: relation {{ error: error }};
    call save_text_file{{path: "{path}", lines: array ["Nile"], out errors}};
    call debug_value{{v: count{{r: errors}}}};
];
"#,
        path = path.display()
    );
    let (code, stderr) = run_source("saved_no_errors", &source);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stderr.trim(), "0");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "Nile\n");
}

/// A line that wouldn't read back as one stops the program before anything is written, so a
/// file already there is as it was, whether or not the call names `out errors`, as it is a
/// mistake in the call. The check can tell: each of these reads back differently.
#[test]
fn a_line_that_wouldnt_read_back_as_one_writes_nothing() {
    use trendy_diag::Code;
    for (name, lines, at) in [
        (
            "line_feed",
            r#"array ["Nile", "Amazon\nYangtze", "Volga"]"#,
            2,
        ),
        ("carriage_return_at_its_end", r#"array ["Nile\r"]"#, 1),
        (
            "line_feed_at_its_end",
            r#"array ["Nile", "Volga", "Mekong\n"]"#,
            3,
        ),
        ("both", r#"array ["\r\n"]"#, 1),
    ] {
        for (form, errors) in [("stops", ""), ("with_errors", ", out errors")] {
            let path = saved_file(&format!("{name}_{form}.txt"), Some("as it was"));
            let source = format!(
                r#"uses {{ trendy.files }};

operator main{{}} [
    var errors: relation {{ error: error }};
    call save_text_file{{path: "{path}", lines: {lines}{errors}}};
    call debug_value{{v: "after"}};
];
"#,
                path = path.display()
            );
            let (code, stderr) = run_source(&format!("not_one_{name}_{form}"), &source);
            assert_eq!(code, 1, "{name} {form}: {stderr}");
            let id = Code::NotOneLine.id();
            let expected = format!("error[{id}]: line {at} to write to ");
            assert!(stderr.contains(&expected), "{name} {form}: {stderr}");
            assert!(!stderr.contains("after"), "{name} {form}: {stderr}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "as it was");
        }
    }
}

/// Whether lowering `source` left a relation in no particular order and then sorted it, which
/// gives the right answer, for a sort that keeping it in order would have saved, each time.
fn sorts_what_it_left_unsorted(source: &str) -> bool {
    trendy_lower::lower(&trendy_opt::optimized(analyze(source))).sorts_what_it_left_unsorted()
}

/// A relation is left in no particular order only if nothing goes on to want it in order, however
/// far above it that is: what can fail for a tuple, or a reader that depends on order, reached
/// through a restriction, a join or a rename, once for each tuple of something. Each of these
/// cost twice what it had when that wasn't followed: 2015 day 3, and 2017 day 11, read a rename
/// for each tuple, and 2025 day 1, and 2023 day 24, read something that can fail.
#[test]
fn nothing_left_in_no_particular_order_is_sorted_later() {
    assert!(!sorts_what_it_left_unsorted(READERS_OF_A_RELATION));
    let heading = "heading step := { n: integer, a: integer, b: integer };
";
    for body in [
        "const taken := s rename {n as m, a as east, b as north};
    const reached := s extend { x := sum{r: taken where m <= n, of: east} };
    count{r: reached{x}}",
        "const taken := s rename {n as m, a as east, b as north};
    const reached := s extend { x := sum{r: (taken where m <= n) extend { far := east * north }, of: far} };
    count{r: reached{x}}",
        "const pairs := (s join s rename {a as c}) where n > 0;
    const speeds := series{from: 1, to: 9} rename {value as speed};
    count{r: (pairs times speeds) where b mod speed = 0}",
        "const joined := s join s rename {a as c};
    const narrowed := joined where a > b;
    const read := narrowed where c > 0;
    count{r: read extend { d := a + b }}",
        "const joined := s join s rename {a as c};
    max{r: joined, of: a + c} + sum{r: joined, of: b}",
    ] {
        for (form, statements) in [
            ("bindings", ""),
            (
                "statements",
                "var ran := 0;
    while ran < 1 loop [
        ran := ran + 1;
    ];
    ",
            ),
        ] {
            let source = format!(
                "{heading}
operator f{{s: relation step}}: integer [
    {statements}{body}
];

operator main{{}} [
    call debug_value{{v: f{{s: series{{from: 1, to: 9}} extend {{ n := value, a := value mod 3, b := value mod 2 }} remove {{value}}}}}};
];
"
            );
            assert!(!sorts_what_it_left_unsorted(&source), "{form}: {body}");
        }
    }
    let mut lowered = 0;
    for entry in std::fs::read_dir(workspace_root().join("examples")).unwrap() {
        let path = entry.unwrap().path();
        if path
            .extension()
            .is_some_and(|extension| extension == "trendy")
        {
            let source = std::fs::read_to_string(&path).unwrap();
            if source.lines().any(|line| line.starts_with("uses ")) {
                continue;
            }
            assert!(!sorts_what_it_left_unsorted(&source), "{}", path.display());
            lowered += 1;
        }
    }
    assert!(lowered > 10, "only {lowered} examples");
}

/// A rename named by a `const` reads what it renames in place, but only if that is already
/// computed: a rename of what could fail is computed where it was written, before what comes
/// after it, as what it fails with says.
#[test]
fn a_rename_of_what_is_computed_fails_where_it_was_named() {
    let source = "operator f{p: relation { a: integer }, one: integer}: integer [
    const renamed := (p extend { b := 9223372036854775800 + a }) rename {b as c};
    const later := 9223372036854775807 + one;
    later + count{r: renamed rename {c as d}}
];

operator main{} [
    call debug_value{v: f{p: series{from: 10, to: 12} rename {value as a}, one: 1}};
];
";
    let (code, stderr) = run_source("rename_fails_where_named", source);
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.contains("9223372036854775800 + 10 is out of"),
        "{stderr}"
    );
}

/// A rename named by a `const` is the value of what it renamed as it was then, whatever is
/// assigned after: it can only be read where it was written if that can't have changed.
#[test]
fn a_named_rename_has_the_value_it_had_when_it_was_named() {
    let source = "operator in_a_loop{p: relation { a: integer }}: integer [
    var x := p;
    var seen := 0;
    var i := 0;
    while i < 3 loop [
        const r := x rename {a as b};
        x := x where a > i + 1;
        seen := seen * 100 + count{r: r rename {b as c}} * 10 + count{r: x};
        i := i + 1;
    ];
    seen
];

operator after_it{p: relation { a: integer }}: integer [
    var x := p;
    var i := 0;
    while i < 1 loop [
        i := i + 1;
    ];
    const r := x rename {a as b};
    x := x where a > 2;
    count{r: r rename {b as c}} * 10 + count{r: x}
];

operator assigned_again{p: relation { a: integer }}: integer [
    var i := 0;
    while i < 1 loop [
        i := i + 1;
    ];
    var r := p rename {a as b};
    r := (p where a > 3) rename {a as b};
    count{r: r rename {b as c}}
];

operator of_a_var_parameter{var v: relation { a: integer }, out seen: integer} [
    const r := v rename {a as b};
    v := v where a > 3;
    seen := count{r: r rename {b as c}} * 10 + count{r: v};
];

operator main{} [
    const p := series{from: 1, to: 5} rename {value as a};
    call debug_value{v: in_a_loop{p}};
    call debug_value{v: after_it{p}};
    call debug_value{v: assigned_again{p}};
    var v := p;
    var seen := 0;
    call of_a_var_parameter{var v, out seen};
    call debug_value{v: seen};
    const named := v rename {a as b};
    v := v where a > 4;
    call debug_value{v: count{r: named rename {b as c}} * 10 + count{r: v}};
    var again := p rename {a as b};
    again := (p where a > 3) rename {a as b};
    call debug_value{v: count{r: again rename {b as c}}};
];
";
    let (code, stderr) = run_source("named_rename_when_named", source);
    assert_eq!(code, 0, "{stderr}");
    let written: Vec<&str> = stderr.lines().collect();
    assert_eq!(written, ["544332", "53", "2", "52", "21", "2"], "{stderr}");
}

/// A rename that is only an operand moves no tuples, by name or not, on either side, so
/// that what it renames is the same each time, and earns an index: reshaping and sorting
/// for each of these would take over ten minutes.
#[test]
fn a_rename_that_is_only_an_operand_costs_nothing() {
    for (name, setup, query) in [
        (
            "keeping_order",
            "",
            "small_one join big_one rename {v as w}",
        ),
        (
            "moving_the_key",
            "",
            "small_one rename {k as x} join big_one rename {k as x, v as a}",
        ),
        (
            "by_name",
            "const small_named := small_one rename {k as x};
    const big_named := big_one rename {k as x, v as a};
    ",
            "small_named join big_named",
        ),
        (
            "semijoin",
            "",
            "big_one rename {k as x, v as a} matching small_one rename {k as x}",
        ),
        (
            "by_name_as_statements",
            "var ran := 0;
    while ran < 1 loop [
        ran := ran + 1;
    ];
    const small_named := small_one rename {k as x};
    const big_named := big_one rename {k as x, v as a};
    ",
            "small_named join big_named",
        ),
    ] {
        let source = format!(
            "heading big := {{ k: integer, v: integer }};

operator round{{big_one: relation big, small_one: relation {{ k: integer }}}}: integer [
    {setup}count{{r: {query}}}
];

operator rounds{{big_one: relation big, small_one: relation {{ k: integer }}, left: integer, seen: integer}}: integer [
    if left = 0
        then seen
        else rounds{{big_one, small_one, left: left - 1, seen: seen + round{{big_one, small_one}}}}
];

operator main{{}} [
    const numbers := series{{from: 1, to: 250000}};
    const big_one := numbers extend {{ k := value, v := (value * 7919) mod 250007 }} remove {{value}};
    const small_one := series{{from: 1, to: 10}} extend {{ k := value * 1000 }} remove {{value}};
    call debug_value{{v: rounds{{big_one, small_one, left: 20000, seen: 0}}}};
];
"
        );
        let start = std::time::Instant::now();
        let (code, stderr) = run_source(&format!("rename_operand_{name}"), &source);
        assert_eq!(code, 0, "{name}: {stderr}");
        assert_eq!(stderr.trim(), "200000", "{name}");
        let taken = start.elapsed();
        assert!(taken.as_secs() < 30, "{name}: {taken:?}");
    }
}

/// A join of 10 tuples with 250,000, a thousand times, on an attribute that comes first
/// in name order or doesn't, or the same as a semijoin: which took 170 billion
/// instructions, and 0.7 at most now, as it reads all the larger's tuples only until it
/// has earned an index.
#[test]
fn a_join_costs_about_the_same_whatever_its_attributes_are_called() {
    for (name, key, other, query) in [
        ("first", "a", "z", "small_one join big_one"),
        ("second", "z", "a", "small_one join big_one"),
        ("first_semijoin", "a", "z", "big_one matching small_one"),
        ("second_semijoin", "z", "a", "big_one matching small_one"),
    ] {
        let source = format!(
            "heading big := {{ {key}: integer, {other}: integer }};
heading small := {{ {key}: integer }};

operator rounds{{big_one: relation big, small_one: relation small, left: integer, seen: integer}}: integer [
    if left = 0
        then seen
        else rounds{{big_one, small_one, left: left - 1, seen: seen + count{{r: {query}}}}}
];

operator main{{}} [
    const numbers := series{{from: 1, to: 250000}};
    const big_one := numbers extend {{ {key} := value, {other} := (value * 7919) mod 250007 }} remove {{value}};
    const small_one := series{{from: 1, to: 10}} extend {{ {key} := value * 1000 }} remove {{value}};
    call debug_value{{v: rounds{{big_one, small_one, left: 1000, seen: 0}}}};
];
"
        );
        let start = std::time::Instant::now();
        let (code, stderr) = run_source(&format!("join_names_{name}"), &source);
        assert_eq!(code, 0, "{stderr}");
        assert_eq!(stderr.trim(), "10000", "{name}");
        // Sorting the larger each time takes ten seconds.
        let taken = start.elapsed();
        assert!(taken.as_secs() < 4, "{name}: {taken:?}");
    }
}

/// Each way of finding what agrees gives the same tuples, in canonical order: the
/// attributes in common first in each operand or not, in operands of about one size,
/// which are sorted, or of very different sizes, the larger read as it is or galloped
/// through, either way round, with what `where` says they should be to check by.
#[test]
fn a_join_finds_the_same_tuples_whichever_way_it_looks() {
    let mut programs = 0;
    for (key, big_other, small_other) in [
        ("a", "m", "n"),
        ("z", "b", "c"),
        ("k", "b", "n"),
        ("k", "m", "c"),
    ] {
        for few in [1, 3, 6] {
            for many in [8, 200] {
                programs += 1;
                // The greatest value of the attribute `big` has.
                let top = many.min(10);
                let source = format!(
                    "operator main{{}} [
    var big := series{{from: 1, to: {many}}} extend {{ {key} := value mod 11, {big_other} := value }} remove {{value}};
    var small := series{{from: 1, to: {few}}} extend {{ {key} := value * 2, {small_other} := value }} remove {{value}};
    var keys := small{{{key}}};
    var none := keys where {key} < 0;
    var wanted := big where {key} = 2 or ({few} >= 3 and ({key} = 4 or {key} = 6)) or ({few} >= 6 and ({key} = 8 or {key} = 10 or {key} = 12));
    call debug_value{{v: (big matching keys) = wanted}};
    call debug_value{{v: (big matching small) = wanted}};
    call debug_value{{v: (big not matching keys) = (big minus wanted)}};
    call debug_value{{v: (big not matching none) = big}};
    call debug_value{{v: is_empty{{r: big matching none}}}};
    call debug_value{{v: join{{big, keys}} = wanted}};
    call debug_value{{v: join{{keys, big}} = wanted}};
    call debug_value{{v: is_empty{{r: join{{big, none}}}}}};
    call debug_value{{v: (keys matching big) = (keys where {key} <= {top})}};
    call debug_value{{v: join{{big, small}}{{{key}, {big_other}}} = wanted}};
    call debug_value{{v: join{{small, big}}{{{key}, {small_other}}} = (small where {key} <= {top})}};
    call debug_value{{v: count{{r: join{{big, small}}}} = count{{r: wanted}}}};
];
"
                );
                let name = format!("join_ways_{key}_{big_other}_{few}_{many}");
                let (code, stderr) = run_source(&name, &source);
                assert_eq!(code, 0, "{name}: {stderr}");
                let wrong: Vec<_> = stderr
                    .lines()
                    .enumerate()
                    .filter(|(_, line)| *line != "true")
                    .collect();
                assert!(wrong.is_empty(), "{name}: {wrong:?}\n{source}");
                assert_eq!(stderr.lines().count(), 12, "{name}");
            }
        }
    }
    assert_eq!(programs, 24);
}

/// A join of a few tuples with many, again and again, on attributes that don't come
/// first, reads them all only until they have earned an index: without one, these would
/// take over a minute.
#[test]
fn an_index_makes_each_join_follow_the_smaller() {
    let source = "heading big := { z: integer, a: integer };
heading small := { z: integer };

operator rounds{big_one: relation big, small_one: relation small, left: integer, seen: integer}: integer [
    if left = 0
        then seen
        else rounds{big_one, small_one, left: left - 1, seen: seen + count{r: small_one join big_one}}
];

operator main{} [
    const big_one := series{from: 1, to: 250000} extend { z := value, a := (value * 7919) mod 250007 } remove {value};
    const small_one := series{from: 1, to: 10} extend { z := value * 1000 } remove {value};
    call debug_value{v: rounds{big_one, small_one, left: 100000, seen: 0}};
];
";
    let start = std::time::Instant::now();
    let (code, stderr) = run_source("join_earns_index", source);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stderr.trim(), "1000000");
    let taken = start.elapsed();
    assert!(taken.as_secs() < 20, "{taken:?}");
}

/// What agrees on two attributes with another between them, which aren't in order
/// though the first is, and values the smaller has several tuples of, read from the
/// larger as it is.
#[test]
fn a_join_finds_what_agrees_on_several_attributes_and_many_tuples() {
    let (code, stderr) = run_source(
        "join_several",
        r"
operator main{} [
    var big := series{from: 1, to: 200} extend { a := value mod 3, m := value, z := value mod 5 } remove {value};
    var wanted := relation { { a: 1, z: 1 }, { a: 2, z: 4 } };
    call debug_value{v: (big matching wanted) = (big where (a = 1 and z = 1) or (a = 2 and z = 4))};
    call debug_value{v: join{big, wanted} = (big where (a = 1 and z = 1) or (a = 2 and z = 4))};
    call debug_value{v: join{wanted, big} = (big where (a = 1 and z = 1) or (a = 2 and z = 4))};
    call debug_value{v: count{r: big not matching wanted}};

    var many := series{from: 1, to: 800} extend { z := value mod 11, b := value } remove {value};
    var several := series{from: 1, to: 30} extend { z := value mod 3, c := value } remove {value};
    call debug_value{v: count{r: join{many, several}}};
    call debug_value{v: count{r: join{several, many}}};
    call debug_value{v: count{r: join{many where b <= 12, several}}};
    call debug_value{v: count{r: many matching several}};
];
",
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        ["true", "true", "true", "173", "2180", "2180", "40", "218"]
    );
}

/// An index has to go with the tuples it is of: made again in memory that has been
/// freed, of as many tuples, in another order, or moved to make room for more, and
/// something else of as many tuples made where they were.
#[test]
fn an_index_is_not_of_what_comes_after_its_tuples() {
    let (code, stderr) = run_source(
        "kept_index_reused",
        r"
operator joined{big: relation { z: integer, b: integer }, few: relation { z: integer }}: integer [
    count{r: big join few}
];

operator again{big: relation { z: integer, b: integer }, few: relation { z: integer }}: integer [
    var i := 0;
    var found := 0;
    while i < 20 loop [
        found := joined{big, few};
        i := i + 1;
    ];
    found
];

operator main{} [
    const few := series{from: 1, to: 40} extend { z := value * 100 } remove {value};
    var big := series{from: 1, to: 5000} extend { z := value, b := value } remove {value};
    var round := 0;
    while round < 8 loop [
        call debug_value{v: again{big, few}};
        big := big replace { z := (z * 7919) mod 10007 };
        round := round + 1;
    ];

    const shape := series{from: 1, to: 5000} extend { z := value, b := value } remove {value};
    var moved := shape replace { z := (z * 7919) mod 10007 };
    call debug_value{v: again{big: moved, few}};
    insert moved series{from: 6000, to: 20000} extend { z := (value * 7919) mod 10007, b := value } remove {value};
    call debug_value{v: again{big: moved, few}};
    var multiplier := 3;
    while multiplier < 20 loop [
        const other := shape replace { z := (z * multiplier) mod 10007 };
        call debug_value{v: again{big: other, few}};
        multiplier := 2 * multiplier - 1;
    ];
];
",
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            "40", "21", "21", "19", "20", "22", "19", "20", "21", "77", "27", "40", "24", "20",
        ]
    );
}

/// A relation joined again and again gets an index, after sixteen joins, which must go
/// when the tuples it is of do: changed where they are, or freed, and something else
/// made in their memory.
#[test]
fn an_index_a_relation_has_earned_goes_when_its_tuples_do() {
    let (code, stderr) = run_source(
        "kept_index",
        r"
operator joined{big: relation { z: integer, b: integer }, few: relation { z: integer }}: integer [
    count{r: big join few}
];

/* Enough joins to have earned an index, then what the last of them finds. */
operator again{big: relation { z: integer, b: integer }, few: relation { z: integer }}: integer [
    var i := 0;
    var found := 0;
    while i < 20 loop [
        found := joined{big, few};
        i := i + 1;
    ];
    found
];

operator main{} [
    var big := series{from: 1, to: 5000} extend { z := value * 2, b := value } remove {value};
    var few := series{from: 1, to: 3} extend { z := value * 2 } remove {value};
    call debug_value{v: again{big, few}};
    /* Added to where it is: there is room, as it has just been made smaller. */
    delete big where b = 1;
    call debug_value{v: big join few};
    call debug_value{v: again{big, few}};
    insert big relation { { z: 4, b: 9001 } };
    call debug_value{v: big join few};
    call debug_value{v: big matching few where z = 4};
    call debug_value{v: count{r: big not matching few}};
    call debug_value{v: again{big, few}};
    delete big where z = 4;
    call debug_value{v: big join few};
    call debug_value{v: again{big, few}};
    /* A copy keeps what it was, and the index, while the other changes. */
    const held := big;
    insert big relation { { z: 2, b: 9002 } };
    call debug_value{v: again{big, few}};
    call debug_value{v: again{big: held, few}};
    delete big where b = 9002;
    /* Made again and again, each likely in memory one before it has gone from. */
    var round := 0;
    while round < 6 loop [
        big := big replace { z := z + 2 };
        call debug_value{v: again{big, few}};
        round := round + 1;
    ];
    /* More relations than it keeps indexes of, each still right, then a first again. */
    var other := 1;
    while other <= 10 loop [
        const each := series{from: 1, to: 4096 + other} extend { z := value * other, b := value } remove {value};
        call debug_value{v: again{big: each, few}};
        other := other + 1;
    ];
    call debug_value{v: again{big, few}};
    /* On other attributes of one relation, two indexes of it at once. */
    const pairs := series{from: 1, to: 5000} extend { x := value mod 70, y := value mod 71, b := value } remove {value};
    const on_x := relation { { x: 3 } };
    const on_y := relation { { y: 3 } };
    var i := 0;
    var seen := 0;
    while i < 20 loop [
        seen := count{r: pairs join on_x} * 100000 + count{r: pairs join on_y};
        i := i + 1;
    ];
    call debug_value{v: seen};
];
",
    );
    assert_eq!(code, 0, "{stderr}");
    let relation = "relation { b: integer, z: integer }";
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            "3".to_string(),
            format!("{relation} {{ {{ b: 2, z: 4 }}, {{ b: 3, z: 6 }} }}"),
            "2".to_string(),
            format!("{relation} {{ {{ b: 2, z: 4 }}, {{ b: 3, z: 6 }}, {{ b: 9001, z: 4 }} }}"),
            format!("{relation} {{ {{ b: 2, z: 4 }}, {{ b: 9001, z: 4 }} }}"),
            "4997".to_string(),
            "3".to_string(),
            format!("{relation} {{ {{ b: 3, z: 6 }} }}"),
            "1".to_string(),
            "2".to_string(),
            "1".to_string(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
            "3".to_string(),
            "3".to_string(),
            "1".to_string(),
            "1".to_string(),
            "0".to_string(),
            "1".to_string(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
            "7200071".to_string(),
        ]
    );
}

/// A number read from a tuple that holds something else, which printed 1000: the tuple
/// was given up after the statement that last read it, making all of it zero, and the
/// constant was only another name for part of it. Whatever else the tuple holds.
#[test]
fn a_number_read_from_a_tuple_lasts_past_the_tuple() {
    for (i, (ty, value)) in [
        ("relation { x: integer }", "relation { { x: n } }"),
        ("text", "to_text{from: n}"),
        ("array integer", "array [n]"),
    ]
    .into_iter()
    .enumerate()
    {
        let source = format!(
            "heading made := {{ cells: {ty}, at: integer }};

operator make{{n: integer}}: tuple made [
    tuple {{ cells: {value}, at: n * 2 }}
];

operator late{{n: integer}}: integer [
    const landed := make{{n}};
    const at_value := landed.at;
    const other := series{{from: 1, to: 1000}};
    at_value + count{{r: other}}
];

operator main{{}} [
    call debug_value{{v: late{{n: 21}}}};
];
"
        );
        let (code, stderr) = run_source(&format!("number_from_tuple_{i}"), &source);
        assert_eq!(code, 0, "{stderr}");
        assert_eq!(stderr.trim(), "1042", "{ty}");
    }
}

/// Programs that read from what is given up, as
/// [`a_value_read_from_what_is_given_up_stays_what_it_was`] runs them.
const READ_FROM_WHAT_IS_GIVEN_UP: &str = r#"
heading made := {
    cells: relation { x: integer },
    at: integer,
    pos: tuple { i: integer, j: integer },
    name: text,
};

operator make{n: integer}: tuple made [
    tuple { cells: relation { { x: n } }, at: n * 2, pos: tuple { i: n, j: n + 5 }, name: "abc" }
];

operator late{n: integer}: integer [
    const landed := make{n};
    const at_value := landed.at;
    const other := series{from: 1, to: 1000};
    at_value + count{r: other}
];

operator late_var{n: integer}: integer [
    const landed := make{n};
    var at_value := landed.at;
    const other := series{from: 1, to: 1000};
    at_value + count{r: other}
];

operator folded{n: integer}: integer [
    const landed := make{n};
    const at_value := landed.at + 0;
    const other := series{from: 1, to: 1000};
    at_value + count{r: other}
];

/* By way of another name for it, and of a part of a part. */
operator again{n: integer}: integer [
    const landed := make{n};
    const at_value := landed.at;
    const named := at_value;
    const other := series{from: 1, to: 1000};
    named + count{r: other}
];

operator deep{n: integer}: integer [
    const landed := make{n};
    const j := landed.pos.j;
    const other := series{from: 1, to: 1000};
    j + count{r: other}
];

operator whole_part{n: integer}: integer [
    const landed := make{n};
    const pos := landed.pos;
    const other := series{from: 1, to: 1000};
    pos.j + count{r: other}
];

/* In a value block, as an argument, and for each tuple. */
operator in_block{n: integer}: integer [
    const value := [ const landed := make{n}; const at_value := landed.at; at_value ];
    const other := series{from: 1, to: 1000};
    value + count{r: other}
];

operator same{x: integer}: integer [ x ];

operator as_argument{n: integer}: integer [
    const landed := make{n};
    const at_value := landed.at;
    same{x: at_value} + count{r: series{from: 1, to: 1000}}
];

operator each{n: integer}: integer [
    const values := (series{from: 1, to: n} rename {value as x}) extend {
        y := [
            const landed := make{n: x};
            const at_value := landed.at;
            const other := series{from: 1, to: 10};
            at_value + count{r: other}
        ],
    };
    sum{r: values, of: y}
];

/* With the next call in the function's place, which gives everything up first. */
operator countdown{k: integer, total: integer}: integer [
    if k = 0 then total else [
        const landed := make{n: k};
        const at_value := landed.at;
        const other := series{from: 1, to: 10};
        countdown{k: k - 1, total: total + at_value + count{r: other}}
    ]
];

/* What points to something holds it itself, of the same kinds of tuple. */
operator kept{n: integer}: integer [
    const landed := make{n};
    const cells := landed.cells;
    const other := series{from: 1, to: 1000};
    count{r: cells} + count{r: other}
];

operator text_kept{n: integer}: integer [
    const landed := make{n};
    const name := landed.name;
    const other := series{from: 1, to: 1000};
    length{a: to_characters{from: name}} + count{r: other}
];

/* Out of a buffer, which giving it up frees. */
operator element{n: integer}: integer [
    var numbers: array integer;
    load numbers value from series{from: 1, to: n} order [asc value];
    const last_of := [ const held := numbers || numbers; const at := held[n]; at ];
    const other := series{from: 1, to: 1000};
    last_of + count{r: other}
];

operator only_tuple{n: integer}: integer [
    const one := series{from: 1, to: n} where value = 3;
    const it := tuple from one;
    const other := series{from: 1, to: 1000};
    it.value + count{r: other}
];

/* Read last as an operand, where the operation gives it up as it ends. */
operator counted_first{n: integer}: integer [
    const numbers := series{from: 1, to: n};
    const how_many := count{r: numbers};
    const some := numbers where value > 3;
    how_many + count{r: some}
];

operator main{} [
    call debug_value{v: late{n: 21}};
    call debug_value{v: late_var{n: 21}};
    call debug_value{v: folded{n: 21}};
    call debug_value{v: again{n: 21}};
    call debug_value{v: deep{n: 21}};
    call debug_value{v: whole_part{n: 21}};
    call debug_value{v: in_block{n: 21}};
    call debug_value{v: as_argument{n: 21}};
    call debug_value{v: each{n: 3}};
    call debug_value{v: countdown{k: 3, total: 0}};
    call debug_value{v: kept{n: 21}};
    call debug_value{v: text_kept{n: 21}};
    call debug_value{v: counted_first{n: 7}};
    call debug_value{v: element{n: 7}};
    call debug_value{v: only_tuple{n: 7}};
];
"#;

/// What is read from a tuple, a relation or an array lasts past what it was read from,
/// which is given up once nothing reads it, making all of it zero, numbers too.
#[test]
fn a_value_read_from_what_is_given_up_stays_what_it_was() {
    let (code, stderr) = run_source("read_from_what_is_given_up", READ_FROM_WHAT_IS_GIVEN_UP);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            "1042", "1042", "1042", "1042", "1026", "1026", "1042", "1042", "42", "42", "1001",
            "1003", "11", "1007", "1003",
        ]
    );
}

/// Arrays the program computes: `||` holds what its elements point to, and `<` is
/// element by element, one that runs out first coming first.
#[test]
fn arrays_concatenate_and_compare_when_the_program_runs() {
    let (code, stderr) = run_source(
        "arrays_running",
        r#"
type n { { v: integer }, order [asc v] };

var kept: array text;

operator joined{a: array text, b: array text}: array text [
    a || b
];

operator main{} [
    var one := "a" || "b";
    kept := joined{a: array [one], b: array [one || "c", "d"]};
    one := "gone";
    call debug_value{v: kept};
    call debug_value{v: kept || array text []};
    var i := array [1, 2];
    call debug_value{v: i || array [3]};
    call debug_value{v: i < array [1, 3]};
    call debug_value{v: i < array [1]};
    call debug_value{v: i >= array [1, 2]};
    call debug_value{v: array [one] < array ["h"]};
    call debug_value{v: array [n{v: i[1]}] < array [n{v: i[2]}]};
    call debug_value{v: array [i[1] = 1] > array [i[1] = 2]};
    call debug_value{v: array [array [i[1]]] < array [array [i[2]]]};
    var rows := relation { { a: i }, { a: i || array [0] }, { a: array [0] } };
    call debug_value{v: max{r: rows, of: a}};
    call debug_value{v: min{r: rows, of: a}};
    var ordered: array array integer;
    load ordered value from rows order [desc a] keep {a};
    call debug_value{v: ordered};
];
"#,
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            "array [\"ab\", \"abc\", \"d\"]",
            "array [\"ab\", \"abc\", \"d\"]",
            "array [1, 2, 3]",
            "true",
            "false",
            "true",
            "true",
            "true",
            "true",
            "true",
            "array [1, 2, 0]",
            "array [0]",
            "array [array [1, 2, 0], array [1, 2], array [0]]",
        ]
    );
}

#[test]
fn characters() {
    let (code, stderr) = run_example("characters");
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            "┌─────────┬──────┐",
            "│ country │ flag │",
            "├─────────┼──────┤",
            "│ Japan   │ 🇯🇵   │",
            "│ Kenya   │ 🇰🇪   │",
            "│ Nepal   │ 🇳🇵   │",
            "│ Sweden  │ 🇸🇪   │",
            "└─────────┴──────┘",
            "┌────────────┬─────────────┬─────────┐",
            "│ characters │ code_points │ country │",
            "├────────────┼─────────────┼─────────┤",
            "│          2 │           2 │ Japan   │",
            "│          3 │           5 │ Nepal   │",
            "│          5 │           5 │ Kenya   │",
            "│          7 │           7 │ Sweden  │",
            "└────────────┴─────────────┴─────────┘",
            "\"🇫🇮🇸🇪\"",
            "'👨\u{200D}👩\u{200D}👧\u{200D}👦'",
            "array ['\\n', '\\'', '\"', '👍']",
            "false",
            "'🇯🇵'",
            "\"ab\"",
            "\"'a'\"",
            "'🇸🇪'",
            "\"🇸🇪\"",
            "40",
            "'á'",
        ]
    );
}

/// Characters the program computes: in arrays, relations and tuples, compared and
/// sorted as text is, by their code points, and held by what outlives the statement
/// that made them.
#[test]
fn characters_when_the_program_runs() {
    let (code, stderr) = run_source(
        "characters_running",
        r#"
type grade {
    { letter: character },
    constraint letter >= 'A' and letter <= 'F',
    order [asc letter],
};

const vowels := (series{from: 1, to: 2} extend { c := if value = 1 then 'a' else 'é' }){c};

var seen: array character;

operator first_of{t: text}: character [
    to_characters{from: t}[1]
];

operator remember{t: text} [
    seen := to_characters{from: t};
];

operator give{out c: character} [
    c := first_of{t: "🇸🇪!"};
];

operator main{} [
    var word := "e" || "\u{301}b";
    var letters := to_characters{from: word};
    call debug_value{v: letters};
    call debug_value{v: length{a: letters}};
    call debug_value{v: letters[1] = 'e\u{301}'};
    call debug_value{v: letters[1] = 'é'};
    call debug_value{v: letters[2] > letters[1]};
    call debug_value{v: from_characters{from: letters} = word};
    call debug_value{v: letters = array ['e\u{301}', 'b']};
    call debug_value{v: letters = array ['e\u{301}', 'c']};
    call debug_value{v: tuple { c: letters[2] } = tuple { c: 'b' }};
    call debug_value{v: first_of{t: word}};

    /* What a statement made, held by a variable after it. */
    call remember{t: "🇳🇵" || "x"};
    call debug_value{v: seen};

    var given: character := 'a';
    call give{out c: given};
    call debug_value{v: given};
    call debug_value{v: from_character{from: given} || "!"};
    call debug_value{v: parse_character{from: word || ""} = 'x'};
];
"#,
    );
    assert_eq!(code, 1, "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        lines[..13],
        [
            "array ['e\u{301}', 'b']",
            "2",
            "true",
            "false",
            "false",
            "true",
            "true",
            "false",
            "true",
            "'e\u{301}'",
            "array ['🇳🇵', 'x']",
            "'🇸🇪'",
            "\"🇸🇪!\"",
        ],
        "{stderr}"
    );
    // Of what isn't one character, it says so.
    assert!(
        lines[13].starts_with("error[R0014]: ") && lines[13].ends_with("isn't one character"),
        "{stderr}"
    );
}

/// Characters in relations and arrays, sorted and compared as text is, by their code
/// points, and in keys.
#[test]
fn characters_in_relations() {
    let (code, stderr) = run_source(
        "characters_in_relations",
        r#"
const vowels := (series{from: 1, to: 2} extend { c := if value = 1 then 'é' else 'a' }){c};

operator main{} [
    call debug_table{r: vowels};
    call debug_value{v: vowels where c = 'a'};
    call debug_value{v: min{r: vowels, of: c}};
    call debug_value{v: max{r: vowels, of: c}};
    var ordered: array character;
    load ordered value from vowels order [desc c] keep {c};
    call debug_value{v: ordered};
    var points: relation { c: character, n: integer };
    load points from to_characters{from: "aéa"} as c numbered n;
    call debug_value{v: count{r: points join vowels}};
    call debug_value{v: tuple { c: 'x' }};
    var keyed := relation { { c: 'x', n: 1 } } key { c };
    insert keyed relation { { c: 'y', n: 1 } };
    call debug_value{v: keyed};
    insert keyed relation { { c: 'x', n: 2 } };
];
"#,
    );
    assert_eq!(code, 1, "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        lines[..13],
        [
            "┌───┐",
            "│ c │",
            "├───┤",
            "│ a │",
            "│ é │",
            "└───┘",
            "relation { c: character } { { c: 'a' } }",
            "'a'",
            "'é'",
            "array ['é', 'a']",
            "3",
            "tuple { c: 'x' }",
            "relation { c: character, n: integer } { { c: 'x', n: 1 }, { c: 'y', n: 1 } }",
        ],
        "{stderr}"
    );
    assert!(lines[13].starts_with("error[R0008]"), "{stderr}");
}

/// A field of one character is read as one, and any other isn't.
#[test]
fn load_csv_reads_characters() {
    let good = "country,flag\nSweden,🇸🇪\nNepal,🇳🇵\n";
    let columns = "tuple { country: 1, flag: 2 }";
    let heading = "{ country: text, flag: character }";
    let (code, stderr) = loaded_rows("characters", "", heading, good, columns);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("flag: '🇸🇪'"), "{stderr}");
    assert!(stderr.contains("flag: '🇳🇵'"), "{stderr}");
    let bad = "country,flag\nSweden,SE\n";
    let (code, stderr) = loaded_rows("not_characters", "", heading, bad, columns);
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.starts_with("error[R0014]"), "{stderr}");
    assert!(stderr.contains("isn't one character"), "{stderr}");
}

#[test]
fn constants() {
    let (code, stderr) = run_example("constants");
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            "┌───────────┬────┬────┐",
            "│ direction │ dx │ dy │",
            "├───────────┼────┼────┤",
            "│ east      │  1 │  0 │",
            "└───────────┴────┴────┘",
            "tuple { x: 0, y: -8 }",
            "8",
            "5",
            "stone_number{n: 3}",
            "4",
            "3",
            "-1e0",
        ]
    );
}

/// A constraint the compiler can't compute of the initial values, as it reads a
/// constant it can't compute or runs statements, is checked as the program starts, at
/// its declaration, before anything `main` does, and at each assignment as any is.
#[test]
fn a_constraint_the_compiler_cant_check_is_checked_as_the_program_starts() {
    let doubling = "operator doubling{n: integer}: integer [
    var i := n;
    while i < 8 loop [
        i := i * 2;
    ];
    i
];
";
    let three = "const three := max{r: series{from: 1, to: 3}, of: value};\n";
    // Broken from the start, reading a constant, and running statements.
    for (name, source, line) in [
        (
            "start_constant",
            format!(
                "{three}var v := relation {{ {{ x: 1 }}, {{ x: 2 }}, {{ x: 3 }}, {{ x: 4 }} }};\nconstraint few := count{{r: v}} <= three;\n\noperator main{{}} [\n    call debug_value{{v: \"main\"}};\n];\n"
            ),
            3,
        ),
        (
            "start_statements",
            format!(
                "{doubling}var v := 9;\nconstraint few := v < doubling{{n: 1}};\n\noperator main{{}} [\n    call debug_value{{v: \"main\"}};\n];\n"
            ),
            9,
        ),
    ] {
        let (code, stderr) = run_source(name, &source);
        assert_eq!(code, 1, "{name}: {stderr}");
        let lines: Vec<&str> = stderr.lines().collect();
        assert!(
            lines[0].starts_with("error[R0023]: the constraint `few` is broken: `"),
            "{stderr}"
        );
        assert!(lines[1].ends_with(&format!(".trendy:{line}:1")), "{stderr}");
        assert_eq!(lines.len(), 2, "{stderr}");
    }
}

/// What a constraint the program checks as it starts reads, and what comes after
/// that check.
#[test]
fn after_a_constraint_is_checked_as_the_program_starts() {
    // Holding then, and broken by a statement, of the tuples that break it.
    let (code, stderr) = run_source(
        "start_holds",
        r#"const a := series{from: 1, to: 3} rename {value as x};
var v := relation { { x: 1 } };
constraint few := count{r: v} <= max{r: a, of: x};
constraint known := v <= a;

operator main{} [
    call debug_value{v: "main"};
    insert v relation { { x: 9 } };
    call debug_value{v: "not here"};
];
"#,
    );
    assert_eq!(code, 1, "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines[0], "\"main\"", "{stderr}");
    assert_eq!(
        lines[1],
        "error[R0023]: the constraint `known` is broken: `v <= a` is false of relation { x: integer } { { x: 9 } }"
    );
    assert!(lines[2].ends_with("start_holds.trendy:8:5"), "{stderr}");
    // Broken from the start, of the tuples that break it.
    let (code, stderr) = run_source(
        "start_tuples",
        r#"const a := series{from: 1, to: 3} rename {value as x};
var v := relation { { x: 1 }, { x: 9 } };
constraint known := v <= a;

operator main{} [
    call debug_value{v: "main"};
];
"#,
    );
    assert_eq!(code, 1, "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        lines[0],
        "error[R0023]: the constraint `known` is broken: `v <= a` is false of relation { x: integer } { { x: 9 } }"
    );
    assert!(lines[1].ends_with("start_tuples.trendy:3:1"), "{stderr}");
    // A constant it reads that fails, needed as the program starts.
    let (code, stderr) = run_source(
        "start_fails",
        r#"const broken := 1 div (max{r: series{from: 0, to: 0}, of: value});
var v := relation { { x: 1 } };
constraint c := count{r: v} <= broken;

operator main{} [
    call debug_value{v: "main"};
];
"#,
    );
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.starts_with("error[R0002]"), "{stderr}");
    assert!(!stderr.contains("\"main\""), "{stderr}");
    // Holding throughout, and then `main` runs at no site of the constraint's: a
    // failure says only what `main` called.
    let three = "const three := max{r: series{from: 1, to: 3}, of: value};\n";
    let (code, stderr) = run_source(
        "start_then_main",
        &format!(
            "{three}var v := relation {{ {{ x: 1 }} }};\nconstraint few := count{{r: v}} <= three;\n\noperator deep{{n: integer}}: integer [\n    1 div n\n];\n\noperator main{{}} [\n    call debug_value{{v: three}};\n    call debug_value{{v: deep{{n: three - 3}}}};\n];\n"
        ),
    );
    assert_eq!(code, 1, "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines[0], "3", "{stderr}");
    assert!(lines[2].ends_with("start_then_main.trendy:6:5"), "{stderr}");
    assert!(
        lines[3].contains("in `deep`, called at") && lines[3].ends_with(":11:5"),
        "{stderr}"
    );
    assert_eq!(lines.len(), 4, "{stderr}");
}

/// `<=` and `>=` compare other values than relations, whose constraints say only
/// that they are false.
#[test]
fn a_constraint_can_compare_any_values() {
    let (code, stderr) = run_source(
        "scalar_constraint",
        r#"var v := relation { { x: 1 } };
constraint up_to_three := count{r: v} <= 3;
constraint at_least_one := 1 <= count{r: v};
constraint named := "a" <= "b" or count{r: v} >= 0;

operator main{} [
    insert v relation { { x: 2 }, { x: 3 } };
    call debug_value{v: count{r: v}};
    insert v relation { { x: 4 } };
    call debug_value{v: "not here"};
];
"#,
    );
    assert_eq!(code, 1, "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines[0], "3", "{stderr}");
    assert_eq!(
        lines[1],
        "error[R0023]: the constraint `up_to_three` is broken: `count{r: v} <= 3` is false"
    );
}

/// A constant's only read needs what is around it needs: this closure would be 450
/// million tuples, and only one value's paths are followed.
#[test]
fn a_constants_only_read_needs_no_more_of_it_than_that_read() {
    let (code, stderr) = run_source(
        "only_read",
        r"
const arcs := (series{from: 1, to: 30000} rename {value as a}) extend { b := a + 1 };
const reach := arcs tclose;

operator main{} [
    call debug_value{v: count{r: reach where a = 1}};
];
",
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stderr.lines().collect::<Vec<_>>(), ["30000"]);
}

/// A constant the compiler can't compute is computed when a read first needs it,
/// after those it reads, however it is reached: the failure of one stops the
/// program, where it is declared, after what the program did before that, and one
/// that nothing needs isn't computed, whether it has one read or several.
#[test]
fn a_constant_is_computed_when_a_read_first_needs_it() {
    let (code, stderr) = run_source(
        "computed_constants",
        r#"
const read_last := read_first + 1;
const read_first := max{r: series{from: 1, to: 3}, of: value};
const texts := (series{from: 1, to: 2} extend { t := "n" || to_text{from: value} }){t};
const unreached := 1 div (max{r: series{from: 0, to: 0}, of: value});
const not_needed := 1 div (max{r: series{from: 0, to: 0}, of: value});
const not_needed_twice := 1 div (max{r: series{from: 0, to: 0}, of: value});

operator also{}: integer [
    read_last * 2
];

operator with_it{}: relation { t: text } [
    texts
];

operator fails{e: error}: integer [
    fail e
];

operator main{} [
    call debug_value{v: read_last};
    call debug_value{v: also{}};
    call debug_table{r: with_it{}};
    /* It is read as it is, as many times, with nothing to free. */
    var n := 0;
    while n < 3 loop [
        n := n + count{r: texts union with_it{}};
    ];
    call debug_value{v: n};
    if n = 0 then [
        call debug_value{v: not_needed};
        call debug_value{v: not_needed_twice};
        call debug_value{v: not_needed_twice};
    ];
];
"#,
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            "4",
            "8",
            "┌────┐",
            "│ t  │",
            "├────┤",
            "│ n1 │",
            "│ n2 │",
            "└────┘",
            "4"
        ]
    );
}

/// A constant that can't be computed stops the program when a read first needs it,
/// where it is declared, after what the program did before that.
#[test]
fn a_constant_fails_when_a_read_first_needs_it() {
    // Its only read.
    let broken = "const broken := 1 div (max{r: series{from: 0, to: 0}, of: value});\n";
    let (code, stderr) = run_source(
        "constant_fails",
        &format!(
            r#"{broken}
operator main{{}} [
    call debug_value{{v: "before it"}};
    call debug_value{{v: broken}};
];
"#
        ),
    );
    assert_eq!(code, 1, "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines[0], "\"before it\"", "{stderr}");
    assert_eq!(
        lines[1],
        "error[R0002]: division by zero: 1 div 0 has no result"
    );
    assert!(lines[2].ends_with("constant_fails.trendy:1:1"), "{stderr}");
    assert!(!stderr.contains("called at"), "{stderr}");
    // Read by what an operator computes, the first read that needs it, in the
    // trace, as a call's is.
    let (code, stderr) = run_source(
        "shared_constant_fails",
        &format!(
            r#"{broken}
operator shown{{}}: integer [
    broken + 1
];

operator main{{}} [
    call debug_value{{v: "before it"}};
    call debug_value{{v: shown{{}}}};
    call debug_value{{v: broken}};
];
"#
        ),
    );
    assert_eq!(code, 1, "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines[0], "\"before it\"", "{stderr}");
    assert!(
        lines[2].ends_with("shared_constant_fails.trendy:1:1"),
        "{stderr}"
    );
    assert!(
        lines[3].contains("in `constant broken`, called at") && lines[3].ends_with(":4:5"),
        "{stderr}"
    );
    assert!(lines[4].contains("in `shown`, called at"), "{stderr}");
    // The program is at its own place again after a read computes one.
    let (code, stderr) = run_source(
        "after_a_constant",
        r"
const many := max{r: series{from: 1, to: 3}, of: value};

operator main{} [
    call debug_value{v: 1 div (many - many)};
];
",
    );
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("after_a_constant.trendy:5:"), "{stderr}");
    assert!(!stderr.contains("called at"), "{stderr}");
    // Failing doesn't count as having been computed, and what a transaction
    // that is undone computed stays computed, as nothing assigned it.
    let (code, stderr) = run_source(
        "constants_in_transactions",
        &format!(
            r"{broken}const many := max{{r: series{{from: 1, to: 3}}, of: value}};

operator main{{}} [
    var errors: relation transaction_collection;
    var i := 0;
    while i < 2 loop [
        transaction collecting errors [
            call debug_value{{v: many}};
            call debug_value{{v: broken}};
        ];
        i := i + 1;
    ];
    call debug_value{{v: broken_count{{errors: errors}}}};
    call debug_value{{v: many}};
];

operator broken_count{{errors: relation transaction_collection}}: integer [
    count{{r: errors}}
];
"
        ),
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stderr.lines().collect::<Vec<_>>(), ["3", "3", "1", "3"]);
}

/// A constant's keys are checked as it is computed.
#[test]
fn a_constants_keys_are_checked_as_it_is_computed() {
    let (code, stderr) = run_source(
        "constant_keys",
        r"
const keyed := (series{from: 1, to: 2} extend { same := 1 }) key { same };

operator main{} [
    call debug_table{r: keyed};
];
",
    );
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.starts_with("error[R0008]"), "{stderr}");
}

/// An interface can leave a constant's value to its implementation, and other
/// units read it as they would one with its value there.
#[test]
fn an_interface_can_leave_a_constants_value_to_its_implementation() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("constant_units");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("sizes.trendy"),
        "interface

const small: integer;
const shown := 2;
const computed: integer;

implementation

const small := shown * 3;
const computed := max{r: series{from: 1, to: small}, of: value};
",
    )
    .unwrap();
    let main = dir.join("constant_units.trendy");
    std::fs::write(
        &main,
        "uses { sizes };

operator main{} [
    call debug_value{v: small};
    call debug_value{v: sizes::shown};
    call debug_value{v: computed};
];
",
    )
    .unwrap();
    let (code, stderr) = build_and_run(&main, &dir.join("constant_units"), &workspace_root());
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stderr.lines().collect::<Vec<_>>(), ["6", "2", "6"]);
}

/// A type's constraints keep each integer in a range that takes fewer bytes,
/// which nothing a program can write can tell.
#[test]
fn values_stored_in_fewer_bytes_behave_as_any_other() {
    let (code, stderr) = run_source(
        "narrowed",
        r#"type level { { value: integer, constraint value >= 0 and value <= 0xFF }, order [asc value] };
type word { { value: integer, constraint value >= 0 and value < 0x10000 }, order [asc value] };
type pair { { hi: level, lo: level } };
type small { { v: integer, name: text, constraint v >= 0, constraint v < 10 }, order [asc v, asc name] };
type negative { { v: integer, constraint v >= -1 and v <= 1 } };
type parent { { v: integer, constraint v >= 0 and v <= 9 } };
type child is parent { constraint v = 1 };

var table := relation { { l: level{value: 3} }, { l: level{value: 200} } };

operator twice{w: word}: word [
    word{value: w.value * 2}
];

operator main{} [
    var l := level{value: 255};
    call debug_value{v: l};
    call debug_value{v: l.value};
    var n := 7;
    var w := word{value: n * 1000};
    call debug_value{v: twice{w: w}};
    update w { value := value + 1 };
    call debug_value{v: w.value};
    call debug_value{v: array [level{value: n}, l]};
    call debug_value{v: array [level{value: n}, l][2].value};
    call debug_value{v: level{value: n} < l};
    call debug_value{v: level{value: n} = level{value: 7}};
    call debug_value{v: array [level{value: n}] < array [l]};
    call debug_value{v: pair{hi: l, lo: level{value: n}}};
    call debug_value{v: pair{hi: l, lo: level{value: n}}.lo.value};
    var s := small{v: n, name: "x"};
    call debug_value{v: s};
    call debug_value{v: s.v + 1};
    call debug_value{v: s < small{v: 8, name: "a"}};
    call debug_value{v: negative{v: n - 8}};
    call debug_value{v: parent{v: n - 6} is child};
    call debug_value{v: parent{v: n - 6}.v};
    call debug_value{v: table};
    call debug_table{r: table};
    call debug_value{v: max{r: table, of: l}};
    call debug_value{v: count{r: table where l.value > n}};
    var rows := (series{from: 1, to: 3} extend { l := level{value: value * 50} }){l};
    call debug_value{v: rows};
    var sorted: array level;
    load sorted value from rows order [desc l] keep {l};
    call debug_value{v: sorted};
    var keyed := relation { { l: level{value: 1}, n: 1 } } key { l };
    insert keyed relation { { l: l, n: 2 } };
    call debug_value{v: keyed};
    call debug_value{v: to_code_points{from: "aé"}};
    call debug_value{v: from_code_points{from: to_code_points{from: "aé🌍"}}};
    call debug_value{v: code_point{value: 0x1F30D}.character};
    call debug_value{v: to_code_points{from: "z"}[1] > code_point{value: 1}};
    var big := level{value: n * 100};
    call debug_value{v: "not here"};
];
"#,
    );
    let broken = trendy_diag::Code::BrokenConstraint.id();
    let printed = r#"level{value: 255}
255
word{value: 14000}
7001
array [level{value: 7}, level{value: 255}]
255
true
true
true
pair{hi: level{value: 255}, lo: level{value: 7}}
7
small{name: "x", v: 7}
8
true
negative{v: -1}
true
1
relation { l: level } { { l: level{value: 3} }, { l: level{value: 200} } }
┌───────────────────┐
│ l                 │
├───────────────────┤
│ level{value: 3}   │
│ level{value: 200} │
└───────────────────┘
level{value: 200}
1
relation { l: level } { { l: level{value: 50} }, { l: level{value: 100} }, { l: level{value: 150} } }
array [level{value: 150}, level{value: 100}, level{value: 50}]
relation { l: level, n: integer } { { l: level{value: 1}, n: 1 }, { l: level{value: 255}, n: 2 } }
array [code_point{value: 97}, code_point{value: 233}]
"aé🌍"
'🌍'
true
"#;
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.starts_with(&format!(
            "{printed}error[{broken}]: level{{value: 700}} isn't a value of `level`: it breaks the constraint `value >= 0 and value <= 0xFF`"
        )),
        "{stderr}"
    );
}

/// A field of a CSV file is checked as a value of its type, whose stored
/// components are narrower than what it is read into.
#[test]
fn csv_files_load_values_stored_in_fewer_bytes() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let path = dir.join("narrowed.csv");
    std::fs::write(&path, "id,level\n1,200\n2,300\n").unwrap();
    let (code, stderr) = run_source(
        "narrowed_csv",
        &format!(
            "
uses {{ trendy.files }};

type level {{ {{ value: integer, constraint value >= 0 and value <= 0xFF }} }};

operator main{{}} [
    var rows: array tuple {{ id: integer, level: level }};
    var columns := tuple {{ id: 1, level: tuple {{ value: 2 }} }};
    call load_csv{{path: {path:?}, header: true, columns: columns, out rows: rows}};
];
"
        ),
    );
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.contains("line 3 of ") && stderr.contains("level{value: 300}"),
        "{stderr}"
    );
    std::fs::write(&path, "id,level\n1,200\n2,30\n").unwrap();
    let (code, stderr) = run_source(
        "narrowed_csv_loaded",
        &format!(
            "
uses {{ trendy.files }};

type level {{ {{ value: integer, constraint value >= 0 and value <= 0xFF }} }};

operator main{{}} [
    var rows: array tuple {{ id: integer, level: level }};
    var columns := tuple {{ id: 1, level: tuple {{ value: 2 }} }};
    call load_csv{{path: {path:?}, header: true, columns: columns, out rows: rows}};
    call debug_value{{v: rows}};
    call debug_value{{v: rows[2].level.value + 1}};
];
"
        ),
    );
    assert_eq!(
        (code, stderr.as_str()),
        (
            0,
            "array [tuple { id: 1, level: level{value: 200} }, tuple { id: 2, level: level{value: 30} }]\n31\n"
        )
    );
    // Side by side, with no room between them for an integer.
    std::fs::write(&path, "low,high\n1,200\n2,30\n").unwrap();
    let (code, stderr) = run_source(
        "narrowed_csv_side_by_side",
        &format!(
            "
uses {{ trendy.files }};

type level {{ {{ value: integer, constraint value >= 0 and value <= 0xFF }} }};

type range {{
    {{
        low: integer,
        high: integer,
        constraint low >= 0 and low <= 0xFF,
        constraint high >= 0 and high <= 0xFF,
    }}
}};

operator main{{}} [
    var rows: array tuple {{ high: level, low: level }};
    var columns := tuple {{ high: tuple {{ value: 2 }}, low: tuple {{ value: 1 }} }};
    call load_csv{{path: {path:?}, header: true, columns: columns, out rows: rows}};
    call debug_value{{v: rows}};
    var ranges: array tuple {{ r: range }};
    var of_a_range := tuple {{ r: tuple {{ low: 1, high: 2 }} }};
    call load_csv{{path: {path:?}, header: true, columns: of_a_range, out rows: ranges}};
    call debug_value{{v: ranges}};
];
"
        ),
    );
    assert_eq!(
        (code, stderr.as_str()),
        (
            0,
            "array [tuple { high: level{value: 200}, low: level{value: 1} }, tuple { high: level{value: 30}, low: level{value: 2} }]\narray [tuple { r: range{high: 200, low: 1} }, tuple { r: range{high: 30, low: 2} }]\n"
        )
    );
}

#[test]
fn bytes() {
    let (code, stderr) = run_example("bytes");
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            "┌────────┬─────────────┐",
            "│ format │ magic       │",
            "├────────┼─────────────┤",
            "│ ELF    │ x\"7f454c46\" │",
            "│ PDF    │ x\"25504446\" │",
            "│ PNG    │ x\"89504e47\" │",
            "│ gzip   │ x\"1f8b\"     │",
            "└────────┴─────────────┘",
            "x\"89504e470d0a1a0a\"",
            "8",
            "x'89'",
            "137",
            "x'09'",
            "'9'",
            "x'76'",
            "x'07'",
            "true",
            "┌────────┬─────────────┐",
            "│ format │ magic       │",
            "├────────┼─────────────┤",
            "│ PNG    │ x\"89504e47\" │",
            "└────────┴─────────────┘",
            "\"x\\\"7f45\\\"\"",
        ]
    );
}

#[test]
fn uuids() {
    let (code, stderr) = run_example("uuid");
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            "┌──────────────────────────────────────┬───────────┬───────────┐",
            "│ as_text                              │ name      │ version   │",
            "├──────────────────────────────────────┼───────────┼───────────┤",
            "│ 017f22e2-79b0-7cc3-98c4-dc0c0c07398f │ timed     │ version 7 │",
            "│ 6ba7b810-9dad-11d1-80b4-00c04fd430c8 │ namespace │ another   │",
            "│ 919108f7-52d1-4320-9bac-f847db4148a8 │ random    │ version 4 │",
            "└──────────────────────────────────────┴───────────┴───────────┘",
            "uuid{bytes: x\"017f22e279b07cc398c4dc0c0c07398f\"}",
            "true",
            "false",
            "\"00000000-0000-0000-0000-000000000000\"",
            "\"00000000-0000-4000-8000-000000000000\"",
            "\"00000000-0000-7000-8000-000000000000\"",
        ]
    );
}

#[test]
fn bytes_when_the_program_runs() {
    let (code, stderr) = run_source(
        "bytes_running",
        r#"
type ascii_only { { b: byte, name: text } };

operator number{from: text}: integer [
    parse_integer{from: from}
];

operator main{} [
    var n := number{from: "200"};
    var b := byte{value: n};
    call debug_value{v: b};
    call debug_value{v: b.value};
    call debug_value{v: b and x'f0'};
    call debug_value{v: b or x'0f'};
    call debug_value{v: b xor x'ff'};
    call debug_value{v: not b};
    call debug_value{v: and{b, x'f0', x'c0'}};
    call debug_value{v: or{b, x'01', x'02'}};
    call debug_value{v: xor{b, b, x'01'}};
    call debug_value{v: to_text{from: b}};
    call debug_value{v: b < x'ff'};
    call debug_value{v: b = x'c8'};
    var bytes := array [b, byte{value: n - 1}, x'00'];
    call debug_value{v: bytes};
    call debug_value{v: to_text{from: bytes}};
    call debug_value{v: length{a: bytes}};
    call debug_value{v: bytes[2]};
    call debug_value{v: bytes || x"ff"};
    call debug_value{v: bytes < x"c9"};
    call debug_value{v: bytes = x"c8c700"};
    bytes[3] := x'7f';
    call debug_value{v: bytes};
    var none: array byte;
    call debug_value{v: none};
    call debug_value{v: to_text{from: none}};
    call debug_value{v: none || none};
    var held := ascii_only{b, name: "x"};
    call debug_value{v: held};
    call debug_value{v: held.b};
    var rows := relation { { b, n: 1 }, { b: x'01', n: 2 } } key { b };
    call debug_value{v: rows};
    call debug_table{r: rows};
    call debug_value{v: max{r: rows, of: b}};
    var sorted: array byte;
    load sorted value from rows{b} order [asc b];
    call debug_value{v: sorted};
    call debug_value{v: byte{value: n + 56}};
];
"#,
    );
    let broken = trendy_diag::Code::BrokenConstraint.id();
    assert_eq!(code, 1, "{stderr}");
    let printed = r#"x'c8'
200
x'c0'
x'cf'
x'37'
x'37'
x'c0'
x'cb'
x'01'
"x'c8'"
true
true
x"c8c700"
"x\"c8c700\""
3
x'c7'
x"c8c700ff"
true
true
x"c8c77f"
x""
"x\"\""
x""
ascii_only{b: x'c8', name: "x"}
x'c8'
relation { b: byte, n: integer } { { b: x'01', n: 2 }, { b: x'c8', n: 1 } }
┌───────┬───┐
│ b     │ n │
├───────┼───┤
│ x'01' │ 2 │
│ x'c8' │ 1 │
└───────┴───┘
x'c8'
x"01c8"
"#;
    assert!(
        stderr.starts_with(&format!(
            "{printed}error[{broken}]: byte{{value: 256}} isn't a value of `byte`: it breaks the constraint `value >= 0 and value <= 0xFF`"
        )),
        "{stderr}"
    );
}

/// A program's own subtype of `byte` puts it in a hierarchy, which changes how its
/// values are stored but not what they are, and leaves another unit's `uuid` as it is.
#[test]
fn bytes_of_a_subtype_when_the_program_runs() {
    let (code, stderr) = run_source(
        "bytes_in_a_hierarchy",
        r#"
uses { trendy.uuid };

type ascii is byte { constraint value < 128 };

operator number{from: text}: integer [
    parse_integer{from: from}
];

operator main{} [
    var n := number{from: "97"};
    var b := byte{value: n};
    call debug_value{v: b};
    call debug_value{v: b is ascii};
    call debug_value{v: not b};
    call debug_value{v: (not b) is ascii};
    call debug_value{v: (b and x'0f') is ascii};
    var bytes := array [b, byte{value: n + 100}];
    call debug_value{v: bytes};
    call debug_value{v: to_text{from: bytes}};
    call debug_value{v: bytes < array [byte{value: n + 1}]};
    call debug_value{v: relation { { a: x'ff' }, { a: b } }};
    var id := uuid{bytes: x"00112233_4455_6677_8899_aabbccddeeff"};
    call debug_value{v: id.text};
    call debug_value{v: id < uuid{text: "ff112233-4455-6677-8899-aabbccddeeff"}};
];
"#,
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            "x'61'",
            "true",
            "x'9e'",
            "false",
            "true",
            "array [x'61', x'c5']",
            "\"array [x'61', x'c5']\"",
            "true",
            "relation { a: byte } { { a: x'61' }, { a: x'ff' } }",
            "\"00112233-4455-6677-8899-aabbccddeeff\"",
            "true",
        ]
    );
}

#[test]
fn uuids_when_the_program_runs() {
    let (code, stderr) = run_source(
        "uuids_running",
        r#"
uses { trendy.uuid };

operator main{} [
    var written := "6ba7b810-9dad-41d1-80b4-00c04fd430c8";
    var id := uuid{text: written};
    call debug_value{v: id};
    call debug_value{v: id.text = written};
    call debug_value{v: id is uuid_v4};
    call debug_value{v: id is uuid_v7};
    var v4 := treat id as uuid_v4;
    call debug_value{v: v4.text};
    call generate{out v4: v4};
    call debug_value{v: v4.bytes[7]};
    call debug_value{v: v4.bytes[9]};
    var seven := uuid{bytes: x"017f22e2_79b0_7cc3_98c4_dc0c0c07398f"};
    call debug_value{v: seven is uuid_v7};
    call debug_value{v: is_uuid_text{text: written || "0"}};
    call debug_value{v: is_uuid_text{text: "6BA7B810-9DAD-41D1-80B4-00C04FD430C8"}};
    call debug_value{v: is_uuid_text{text: "6ba7b810x9dad-41d1-80b4-00c04fd430c8"}};
    call debug_value{v: is_uuid_text{text: "6ba7b810-9dad-41d1-80b4-00c04fd430cg"}};
    call debug_value{v: relation { { id: id }, { id: seven } }};
    var text := "not a UUID";
    call debug_value{v: uuid{text: text}};
];
"#,
    );
    let broken = trendy_diag::Code::BrokenConstraint.id();
    assert_eq!(code, 1, "{stderr}");
    let printed = r#"uuid{bytes: x"6ba7b8109dad41d180b400c04fd430c8"}
true
true
false
"6ba7b810-9dad-41d1-80b4-00c04fd430c8"
x'40'
x'80'
true
false
false
false
false
relation { id: uuid } { { id: uuid{bytes: x"017f22e279b07cc398c4dc0c0c07398f"} }, { id: uuid{bytes: x"6ba7b8109dad41d180b400c04fd430c8"} } }
"#;
    assert!(
        stderr.starts_with(&format!("{printed}error[{broken}]: ")),
        "{stderr}"
    );
    assert!(stderr.contains("is_uuid_text{text}"), "{stderr}");
}

/// A value block in a conversion that nothing folds has local variables of its own.
#[test]
fn a_value_block_in_a_conversion_when_the_program_runs() {
    let (code, stderr) = run_source(
        "conversion_block",
        r#"
type t {
    { n: integer },
    { s: text }
        to { n := [ var p := to_code_points{from: s}; length{a: p} ] }
        from { s := [ var written := to_text{from: n}; written || "!" ] },
};

operator main{} [
    var word := "abc";
    var v := t{s: word || "d"};
    call debug_value{v: v};
    call debug_value{v: v.s};
];
"#,
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stderr, "t{n: 4}\n\"4!\"\n");
}

/// A program's own `byte` isn't the prelude's, which is still written as bytes.
#[test]
fn a_programs_own_byte_when_the_program_runs() {
    let (code, stderr) = run_source(
        "own_byte",
        r#"
type byte { { value: integer } };

operator main{} [
    var n := 7;
    var mine := byte{value: n};
    call debug_value{v: mine};
    call debug_value{v: array [mine]};
    call debug_value{v: x'07'};
    call debug_value{v: x"0708"};
    call debug_value{v: to_text{from: mine}};
];
"#,
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            "own_byte::byte{value: 7}",
            "array [own_byte::byte{value: 7}]",
            "x'07'",
            "x\"0708\"",
            "\"own_byte::byte{value: 7}\"",
        ]
    );
}

/// `trendyc --check` checks a unit or a program, and makes nothing.
#[test]
fn check_writes_nothing_and_needs_no_main_of_a_unit_with_an_interface() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("check");
    std::fs::create_dir_all(&dir).unwrap();
    let run = |name: &str, source: &str| {
        let input = dir.join(format!("{name}.trendy"));
        std::fs::write(&input, source).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_trendyc"))
            .current_dir(&dir)
            .arg("--check")
            .arg(&input)
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap(),
        )
    };
    let unit = "interface\n\noperator f{};\n\nimplementation\n\noperator f{} [];\n";
    assert_eq!(run("unit", unit), (true, String::new(), String::new()));
    assert!(!dir.join("unit").exists());
    assert_eq!(
        run("program", "operator main{} [];\n"),
        (true, String::new(), String::new())
    );
    assert!(!dir.join("program").exists());
    // What is wrong with one is reported, as building reports it.
    let (ok, stdout, stderr) = run("broken", "interface\n\nvar a := 1;\n");
    assert!(!ok);
    assert_eq!(stdout, "");
    assert!(
        stderr.contains(trendy_diag::Code::InterfaceDeclaration.id()),
        "{stderr}"
    );
    // With neither, nothing can run it or use it.
    let (ok, _, stderr) = run("neither", "operator f{} [];\n");
    assert!(!ok);
    assert!(
        stderr.contains(trendy_diag::Code::MissingMain.id()),
        "{stderr}"
    );
    // Building one with an interface can't succeed, and says why.
    let (ok, _, stderr) = trendyc("library", unit, &[]);
    assert!(!ok);
    assert!(stderr.contains("so it can't be run"), "{stderr}");
    assert!(stderr.contains("`trendyc --check`"), "{stderr}");
    // It makes nothing, so it can't be told where.
    let out = Command::new(env!("CARGO_BIN_EXE_trendyc"))
        .args(["--check", "x.trendy", "-o", "x"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8(out.stderr).unwrap().starts_with("usage:"));
}

#[test]
fn dates() {
    let (code, stderr) = run_example("dates");
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            "┌────────┬─────┬──────┬────────┐",
            "│ city   │ day │ hour │ minute │",
            "├────────┼─────┼──────┼────────┤",
            "│ Baku   │  20 │    7 │      6 │",
            "│ Kabul  │  20 │    7 │     36 │",
            "│ Lima   │  19 │   22 │      6 │",
            "│ Tehran │  20 │    6 │     36 │",
            "└────────┴─────┴──────┴────────┘",
            "19802",
            "tuple { day: 1, month: 4 }",
            "2",
            "1",
            "true",
            "false",
            "true",
            "true",
            "true",
            "1",
            "true",
            "true",
            "true",
            "true",
            "8765",
            "31",
            "false",
        ]
    );
}

/// Exact lengths of time, and months, with nothing known when compiling.
#[test]
fn durations_when_the_program_runs() {
    let (code, stderr) = run_source(
        "durations_running",
        r#"
uses { trendy.time };

operator main{} [
    var one := parse_integer{from: "1"};
    const first := timestamp{date: date{days: -10_000_000_000_000 * one}, time: time{nanos_of_day: 0}};
    const last := timestamp{date: date{days: 10_000_000_000_000 * one}, time: time{nanos_of_day: 86_399_999_999_999}};
    const whole := elapsed{from: first, to: last};
    call debug_value{v: whole};
    call debug_value{v: elapsed{from: last, to: first}};
    call debug_value{v: tuple { there: after{t: first, by: whole} = last, back: before{t: last, by: whole} = first }};

    /* Written any way, one value: a relation holds it once, and a key sees it as one. */
    const half_back := milliseconds{n: -500 * one};
    call debug_value{v: half_back};
    var rows := relation {
        { d: seconds{n: one} },
        { d: milliseconds{n: 1_000 * one} },
        { d: nanoseconds{n: 1_000_000_000 * one} },
        { d: half_back },
    };
    call debug_value{v: count{r: rows}};

    /* Ordered by how long it is: as it is stored, for what goes by that. */
    call debug_value{v: min{r: rows, of: d}};
    call debug_value{v: max{r: rows, of: d}};
    var sorted: array duration;
    load sorted value from rows order [desc d];
    call debug_value{v: sorted[1] = seconds{n: one}};
    call debug_value{v: array [half_back] < array [seconds{n: one}]};

    /* The same day of another month. */
    var paid := date{year: 2024, month: 1, day: 31 * one};
    const march := add_months{d: paid, months: 2 * one};
    call debug_value{v: tuple { y: march.year, m: march.month, d: march.day }};
    call debug_value{v: has_day_after_months{d: paid, months: one}};

    /* Which fails where the call is, not in the unit. */
    call debug_value{v: "not yet"};
    call debug_value{v: add_months{d: paid, months: one}};
    call debug_value{v: "not here"};
];
"#,
    );
    assert_eq!(code, 1, "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        lines[..12],
        [
            "duration{seconds: 1728000000000086399, subsec_nanos: 999999999}",
            "duration{seconds: -1728000000000086400, subsec_nanos: 1}",
            "tuple { back: true, there: true }",
            "duration{seconds: -1, subsec_nanos: 500000000}",
            "2",
            "duration{seconds: -1, subsec_nanos: 500000000}",
            "duration{seconds: 1, subsec_nanos: 0}",
            "true",
            "true",
            "tuple { d: 31, m: 3, y: 2024 }",
            "false",
            "\"not yet\"",
        ],
        "{stderr}"
    );
    assert!(
        lines[12].starts_with("error[R0024]: add_months{"),
        "{stderr}"
    );
    assert!(
        lines[12].contains("it breaks the constraint `has_day_after_months{d, months}`"),
        "{stderr}"
    );
    assert!(lines[13].contains("durations_running.trendy:"), "{stderr}");
    assert!(!stderr.contains("not here"), "{stderr}");
}

/// Past the end of a `date`'s range there is no instant, and the failure leads to the call.
#[test]
fn an_instant_past_the_end_of_time_fails() {
    let (code, stderr) = run_source(
        "durations_past_the_end",
        r#"
uses { trendy.time };

operator main{} [
    var one := parse_integer{from: "1"};
    const last := timestamp{date: date{days: 10_000_000_000_000 * one}, time: time{nanos_of_day: 0}};
    call debug_value{v: after{t: last, by: days{n: one}}};
];
"#,
    );
    assert_eq!(code, 1, "{stderr}");
    let broken = trendy_diag::Code::BrokenConstraint.id();
    assert!(
        stderr.starts_with(&format!("error[{broken}]: date{{days: 10000000000001}}")),
        "{stderr}"
    );
    assert!(
        stderr.contains("durations_past_the_end.trendy:7"),
        "{stderr}"
    );
}

/// What comes with the compiler and isn't used doesn't change a program's names, or what
/// they print as. Used, there are two of the name, and it says which.
#[test]
fn a_programs_own_type_is_not_renamed_by_a_unit_it_does_not_use() {
    let program = |uses: &str| {
        format!(
            r#"{uses}type duration {{ {{ seconds: integer, label: text }} }};
operator days{{n: integer}}: integer [ n * 2 ];

operator main{{}} [
    var one := parse_integer{{from: "1"}};
    call debug_value{{v: duration{{seconds: one, label: "ours"}}}};
    call debug_value{{v: days{{n: one}}}};
];
"#
        )
    };
    let (code, stderr) = run_source("own_duration", &program(""));
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stderr, "duration{label: \"ours\", seconds: 1}\n2\n");
    let (code, stderr) = run_source("own_duration_used", &program("uses { trendy.time };\n"));
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr,
        "own_duration_used::duration{label: \"ours\", seconds: 1}\n2\n"
    );
}

/// Every day of a whole cycle of the calendar, which then repeats, either way
/// from 1970.
#[test]
fn dates_when_the_program_runs() {
    let (code, stderr) = run_source(
        "dates_running",
        r#"
uses { trendy.time };

operator main{} [
    var start := parse_integer{from: "-146097"};
    var days := start;
    var previous := date{days: days - 1};
    var wrong := 0;
    while days <= 146097 loop [
        var d := date{days};
        var back := date{year: d.year, month: d.month, day: d.day};
        var follows := if d.day = 1 then
                previous.day >= 28
                    and (d.month = previous.month + 1
                        or d.month = 1 and previous.month = 12 and d.year = previous.year + 1)
            else
                d.day = previous.day + 1 and d.month = previous.month and d.year = previous.year;
        if back <> d or not follows then [
            wrong := wrong + 1;
        ];
        previous := d;
        days := days + 1;
    ];
    call debug_value{v: wrong};
];
"#,
    );
    assert_eq!((code, stderr.as_str()), (0, "0\n"));
}

/// What an offset does at each end of the range, and what the types are like
/// to use, with nothing known when compiling.
#[test]
fn timestamps_when_the_program_runs() {
    let (code, stderr) = run_source(
        "timestamps_running",
        r#"
uses { trendy.time };

operator main{} [
    var start := parse_integer{from: "0"};
    var dates := array [
        date{days: start},
        date{days: start + 10_000_000_000_000},
        date{days: start - 10_000_000_000_000},
    ];
    var clocks := array [
        time{nanos_of_day: 0},
        time{nanos_of_day: 1},
        time{hour: 12, minute: 0, second: 0, nanosecond: 0},
        time{nanos_of_day: 86_399_999_999_999},
    ];
    var offsets := array [-86_399, -18_000, 0, 1, 20_700, 86_399];
    var mismatched := 0;
    var seen := 0;
    for d in dates loop [
        for t in clocks loop [
            for o in offsets loop [
                var at := offset_timestamp{timestamp: timestamp{date: d, time: t}, utc_offset: o};
                var again := offset_timestamp{
                    year: at.year,
                    month: at.month,
                    day: at.day,
                    hour: at.hour,
                    minute: at.minute,
                    second: at.second,
                    nanosecond: at.nanosecond,
                    utc_offset: at.utc_offset,
                };
                seen := seen + 1;
                if again <> at then [
                    mismatched := mismatched + 1;
                ];
            ];
        ];
    ];
    call debug_value{v: tuple { mismatched, seen }};

    /* Stored, ordered and keyed like anything else. */
    var a := timestamp{date: dates[1], time: clocks[2]};
    var b := timestamp{date: dates[1], time: clocks[3]};
    var c := timestamp{date: dates[3], time: clocks[4]};
    call debug_value{v: tuple { ab: a < b, ca: c < a, arrays: array [c] < array [a] }};
    var rows := relation { { at: a, n: 1 }, { at: c, n: 2 } } key { at };
    call debug_value{v: min{r: rows, of: at}.date.year};
    var sorted: array timestamp;
    load sorted value from relation { { at: a }, { at: b }, { at: c } } order [desc at];
    call debug_value{v: sorted[3] = c};

    /* A change in one representation, the offset carried. */
    var here := offset_timestamp{timestamp: a, utc_offset: 3_600};
    update here { utc_offset := 0 };
    call debug_value{v: here.timestamp = a};
    update here { hour := 5, utc_offset := 3_600 };
    call debug_value{v: tuple { hour: here.hour, utc: here.timestamp.time.hour }};

    /* Past the end of a date's range, selecting fails as a constraint does. */
    var year := start + 30_000_000_000;
    call debug_value{v: offset_timestamp{
        year,
        month: 1,
        day: 1,
        hour: 0,
        minute: 0,
        second: 0,
        nanosecond: 0,
        utc_offset: 0,
    }};
];
"#,
    );
    let broken = trendy_diag::Code::BrokenConstraint.id();
    assert_eq!(code, 1, "{stderr}");
    let printed = "tuple { mismatched: 0, seen: 72 }
tuple { ab: true, arrays: true, ca: true }
-27379068100
true
true
tuple { hour: 5, utc: 4 }
";
    assert!(
        stderr.starts_with(&format!("{printed}error[{broken}]: date{{days: ")),
        "{stderr}"
    );
}

#[test]
fn csv_files_load_dates_and_times() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let path = dir.join("dates.csv");
    std::fs::write(
        &path,
        "city,year,month,day,hour,minute,second,nano,offset\nTehran,2024,3,20,6,36,26,0,12600\nLima,2024,3,19,22,6,26,0,-18000\n",
    )
    .unwrap();
    let (code, stderr) = run_source(
        "dates_csv",
        &format!(
            "
uses {{ trendy.files, trendy.time }};

operator main{{}} [
    var rows: array tuple {{ city: text, at: offset_timestamp }};
    var columns := tuple {{
        city: 1,
        at: tuple {{
            year: 2,
            month: 3,
            day: 4,
            hour: 5,
            minute: 6,
            second: 7,
            nanosecond: 8,
            utc_offset: 9,
        }},
    }};
    call load_csv{{path: {path:?}, header: true, columns, out rows}};
    call debug_value{{v: rows[1].at.timestamp = rows[2].at.timestamp}};
    call debug_value{{v: rows[1].at = rows[2].at}};
    call debug_value{{v: rows[2].at.timestamp.date.day}};
    var days: array tuple {{ on: date }};
    var of_days := tuple {{ on: tuple {{ year: 2, month: 3, day: 4 }} }};
    call load_csv{{path: {path:?}, header: true, columns: of_days, out rows: days}};
    call debug_value{{v: days[2].on.days}};
];
"
        ),
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stderr, "true\nfalse\n20\n19801\n");
}

/// ASCII characters share bytes that are never released, which nothing can tell
/// from one with bytes of its own, however it is held and given up.
#[test]
fn ascii_characters_behave_as_any_other() {
    let (code, stderr) = run_source(
        "ascii_characters",
        r#"
var kept: array character;

operator first{of: text}: character [
    to_characters{from: of}[1]
];

operator main{} [
    var word := "né" || "a\r\nb";
    var characters := to_characters{from: word};
    call debug_value{v: characters};
    call debug_value{v: from_characters{from: characters} = word};
    call debug_value{v: length{a: characters}};
    call debug_value{v: characters[1] = 'n'};
    call debug_value{v: characters[1] < characters[2]};
    call debug_value{v: to_text{from: characters[1]}};
    call debug_value{v: from_character{from: characters[3]} || "!"};

    /* Kept past the statements and the operators that made them, and given up. */
    var i := 0;
    while i < 1000 loop [
        kept := to_characters{from: "kept" || "é"};
        kept := kept || array [first{of: "z"}];
        i := i + 1;
    ];
    call debug_value{v: kept};
    kept[1] := first{of: "é"};
    kept[2] := first{of: "q"};
    call debug_value{v: kept};

    /* In relations, as keys too, and sorted. */
    var rows := relation {
        { c: first{of: "b"}, n: 1 },
        { c: first{of: "a"}, n: 2 },
        { c: first{of: "é"}, n: 3 },
    }
    key { c };
    call debug_value{v: rows};
    call debug_value{v: max{r: rows, of: c}};
    var sorted: array character;
    load sorted value from rows{c} order [desc c];
    call debug_value{v: sorted};
];
"#,
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            r"array ['n', 'é', 'a', '\r\n', 'b']",
            "true",
            "5",
            "true",
            "true",
            r#""'n'""#,
            r#""a!""#,
            "array ['k', 'e', 'p', 't', 'é', 'z']",
            "array ['é', 'q', 'p', 't', 'é', 'z']",
            "relation { c: character, n: integer } { { c: 'a', n: 2 }, { c: 'b', n: 1 }, { c: 'é', n: 3 } }",
            "'é'",
            "array ['é', 'b', 'a']",
        ]
    );
}

/// A constraint's condition is planned, which changes nothing it says.
#[test]
fn a_planned_constraint_says_what_it_said() {
    let (code, stderr) = run_source(
        "planned_constraint",
        r#"
var regions: relation { region: integer, name: text };
var customers: relation { customer: integer, region: integer };
var orders: relation { customer: integer, amount: integer };

constraint no_refunds_in_one := is_empty{
    r: (orders join customers join regions) where amount < 0 and name = "one",
};

operator main{} [
    regions := relation { { region: 1, name: "one" }, { region: 2, name: "two" } };
    customers := relation { { customer: 7, region: 1 }, { customer: 8, region: 2 } };
    orders := relation { { customer: 7, amount: 5 }, { customer: 8, amount: -5 } };
    call debug_value{v: count{r: orders}};
    insert orders relation { { customer: 8, amount: -9 } };
    call debug_value{v: count{r: orders}};
    insert orders relation { { customer: 7, amount: -1 }, { customer: 7, amount: 2 } };
    call debug_value{v: "not here"};
];
"#,
    );
    let broken = trendy_diag::Code::ConstraintViolated.id();
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.starts_with(&format!("2\n3\nerror[{broken}]: ")),
        "{stderr}"
    );
    assert!(
        stderr.contains("the constraint `no_refunds_in_one` is broken"),
        "{stderr}"
    );
    // Only the tuple that breaks it, with what it joins.
    assert!(stderr.contains("amount: -1"), "{stderr}");
    assert!(!stderr.contains("amount: 2"), "{stderr}");
    assert!(!stderr.contains("amount: -9"), "{stderr}");
    assert!(!stderr.contains("not here"), "{stderr}");
}

/// Operands for [`joined_as_it_runs`]: `a` is large enough for the order of joining to be chosen
/// as the program runs, and `one` isn't known when compiling. `keyed` is too large, and not in
/// order on `z`, to be searched: it is read as it is, so its join with `labels` is in `o`'s order
/// and left so, with `l` first in its name order, and what takes that to be in order on `l` finds
/// few of what it should. `of_some` has few of `a`'s tuples, and each of its tuples many of
/// `fans_out`'s, so `a` is joined first, and then isn't needed, unless what it points to is.
const SEVERAL_OPERANDS: &str = "
    var one := 1;
    const of_some := series{from: 1, to: 20} extend { p := value * 10 * one, fan := value mod 3 } remove {value};
    const fans_out := series{from: 1, to: 2000} extend { fan := value mod 3 * one, spread := value } remove {value};
    const keyed := series{from: 1, to: 50000} extend { z := value * one, o := value } remove {value};
    const labels := series{from: 1, to: 5000} extend { z := value * 3 * one, l := (value * 37) mod 97 } remove {value};
    const by_label := series{from: 0, to: 96} extend { l := value * one, m := value mod 7 } remove {value};
    const by_m := series{from: 0, to: 6} extend { m := value * one, n := value } remove {value};
    const a := series{from: 1, to: 5000} extend { k := value mod 50 * one, p := value } remove {value};
    const b := series{from: 1, to: 300} extend { k := value mod 50 * one, q := value mod 70 } remove {value};
    const c := series{from: 1, to: 400} extend { q := value mod 70 * one, s := value } remove {value};
    const d := series{from: 1, to: 9} extend { s := value * 7 * one, t := to_text{from: value} } remove {value};
    const e := series{from: 1, to: 3} extend { u := value * one } remove {value};
    const f := series{from: 1, to: 2} extend { v := value * one } remove {value};
";

/// Whether lowering `source` leaves the order of joining to be chosen as the program runs.
fn joined_as_it_runs(source: &str) -> bool {
    let lowered = trendy_lower::lower(&trendy_opt::optimized(analyze(source)));
    format!("{lowered:?}").contains("function: Join")
}

/// Joins of several from [`SEVERAL_OPERANDS`], each with its name, the same operands joined two
/// at a time, each pair by a `const`, in an order written by hand, which never chooses, and
/// whether it comes to no tuples.
const JOINS_OF_SEVERAL: [(&str, &str, &str, bool); 23] = [
    (
        "text_only_a_binding_holds",
        "[ const x := a extend { w := to_text{from: p} }; const y := x; join{y, of_some, fans_out} ]",
        "const x := a extend { w := to_text{from: p} };
        const y := x join of_some;
        y join fans_out",
        false,
    ),
    (
        "an_operand_left_as_it_comes",
        "((keyed join labels) rename {o as w}) join by_label join by_m",
        "const x := (keyed join labels) rename {o as w};
        const y := by_label join by_m;
        y join x",
        false,
    ),
    (
        "text_only_its_operand_holds",
        "join{with_text{r: a}, of_some, fans_out}",
        "const x := with_text{r: a};
        const y := x join of_some;
        y join fans_out",
        false,
    ),
    (
        "attributes_of_every_width",
        "join{a extend { f := k mod 2 = 0, g := x'7f', h := 'x' }, b extend { f := k mod 2 = 0, g := x'7f', i := to_approximate{from: k} }, c}",
        "const x := a extend { f := k mod 2 = 0, g := x'7f', h := 'x' };
        const y := b extend { f := k mod 2 = 0, g := x'7f', i := to_approximate{from: k} };
        const z := x join y;
        z join c",
        false,
    ),
    (
        "relation_valued",
        "join{b group { g := {q} }, a, a rename {p as r}}",
        "const x := b group { g := {q} };
        const y := x join a;
        y join (a rename {p as r})",
        false,
    ),
    (
        "an_operand_as_it_comes",
        "((a join b) rename {p as o}) join ((c join d) rename {t as w}) join (e extend { q := u * 7 })",
        "const x := (a join b) rename {p as o};
        const y := (c join d) rename {t as w};
        const z := x join y;
        z join (e extend { q := u * 7 })",
        false,
    ),
    (
        "three",
        "join{a, b, c}",
        "const x := a join b; x join c",
        false,
    ),
    (
        "three_from_the_other_end",
        "join{c, b, a}",
        "const x := c join b; x join a",
        false,
    ),
    (
        "four_with_text",
        "join{a, b, c, d}",
        "const x := c join d; const y := x join b; y join a",
        false,
    ),
    (
        "common_text",
        "join{a extend { w := to_text{from: k} }, b extend { w := to_text{from: k} }, c}",
        "const x := a extend { w := to_text{from: k} };
        const y := b extend { w := to_text{from: k} };
        const z := x join y;
        z join c",
        false,
    ),
    (
        "text_carried",
        "join{a extend { w := to_text{from: p} }, b, c}",
        "const x := a extend { w := to_text{from: p} };
        const y := x join b;
        y join c",
        false,
    ),
    (
        "renamed",
        "join{a rename {k as j, p as z}, b rename {k as j}, c rename {s as z}}",
        "const x := a rename {k as j, p as z};
        const y := b rename {k as j};
        const n := x join y;
        n join (c rename {s as z})",
        false,
    ),
    (
        "swapped_by_rename",
        "join{a rename {k as p, p as k}, b rename {k as p, q as w}, c rename {q as w}}",
        "const x := a rename {k as p, p as k};
        const y := b rename {k as p, q as w};
        const n := x join y;
        n join (c rename {q as w})",
        false,
    ),
    (
        "same_heading",
        "(a where p > 10) join a join (a where p < 3000)",
        "const x := a where p > 10;
        const y := x join a;
        y join (a where p < 3000)",
        false,
    ),
    (
        "one_times_the_rest",
        "join{a, b, c} times e",
        "const x := a join b; const y := x join c; y times e",
        false,
    ),
    (
        "all_times",
        "times{a, e, f}",
        "const x := a times e; x times f",
        false,
    ),
    (
        "two_parts_times",
        "(a join (b where q = 3)) times ((c join (d where s < 22)) rename {q as w})",
        "const x := a join (b where q = 3);
        const y := c join (d where s < 22);
        x times (y rename {q as w})",
        false,
    ),
    (
        "guarded_operands",
        "join{a, b, c} where p > 100 and s < 200",
        "const x := a where p > 100;
        const y := x join b;
        y join (c where s < 200)",
        false,
    ),
    (
        "guard_across_two_of_four",
        "join{a, b, c, d} where p < q",
        "const x := a join b;
        const y := x where p < q;
        const z := y join c;
        z join d",
        false,
    ),
    (
        "guard_over_the_whole",
        "join{a, b, c} where p > s",
        "const x := a join b;
        const y := x join c;
        y where p > s",
        false,
    ),
    (
        "one_empty",
        "join{a, b where q < 0, c}",
        "const x := a join (b where q < 0); x join c",
        true,
    ),
    (
        "none_from_a_join_on_the_way",
        "join{a, b where q = 1, c where q = 2}",
        "const x := a join (b where q = 1); x join (c where q = 2)",
        true,
    ),
    (
        "empty_and_times",
        "times{a, e where u < 0, f}",
        "const x := a times (e where u < 0); x times f",
        true,
    ),
];

/// The order of joining several, chosen as the program runs, changes nothing a program can see,
/// wanted in order or not, and neither does the fixed plan it falls back on with fewer tuples.
#[test]
fn a_join_of_several_gives_what_joining_them_by_hand_does() {
    for (name, several, by_hand, none) in JOINS_OF_SEVERAL {
        for tuples in ["5000", "1000"] {
            let source = format!(
            "operator with_text{{r: relation {{ k: integer, p: integer }}}}: relation {{ k: integer, p: integer, w: text }} [
    var made := r extend {{ w := to_text{{from: p}} }};
    made
];

operator main{{}} [{SEVERAL_OPERANDS}
    call debug_value{{v: count{{r: {several}}}}};
    call debug_value{{v: ({several}) = [ {by_hand} ]}};
    call debug_value{{v: [ {by_hand} ] = ({several})}};
    call debug_value{{v: count{{r: [ {by_hand} ]}}}};
];
"
        );
            let source = source.replace("to: 5000", &format!("to: {tuples}"));
            assert!(joined_as_it_runs(&source), "{name}");
            let (code, stderr) = run_source(&format!("several_{name}_{tuples}"), &source);
            assert_eq!(code, 0, "{name}: {stderr}");
            let lines: Vec<&str> = stderr.lines().collect();
            assert_eq!(lines.len(), 4, "{name}: {stderr}");
            assert_eq!(lines[0], lines[3], "{name}");
            assert_eq!(lines[1..3], ["true", "true"], "{name}");
            assert_eq!(lines[0] == "0", none, "{name}: {stderr}");
        }
    }
}

/// What a join of several gives is written in canonical order, and each tuple's attributes
/// where its type has them.
#[test]
fn a_join_of_several_writes_what_it_makes() {
    let (code, stderr) = run_source(
        "several_written",
        &format!(
            "operator main{{}} [{SEVERAL_OPERANDS}
    call debug_value{{v: join{{a, (b where q = 21) extend {{ p := k }}, c where s = 21, d}}}};
];
"
        ),
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr,
        "relation { k: integer, p: integer, q: integer, s: integer, t: text } { \
         { k: 11, p: 11, q: 21, s: 21, t: \"3\" }, \
         { k: 21, p: 21, q: 21, s: 21, t: \"3\" }, \
         { k: 31, p: 31, q: 21, s: 21, t: \"3\" }, \
         { k: 41, p: 41, q: 21, s: 21, t: \"3\" } }\n"
    );
}

/// A `per` parameter's attributes are read through what wraps each tuple whole, whose names
/// aren't what the runtime could go by.
#[test]
fn a_join_of_several_carries_what_a_per_parameter_wraps() {
    let (code, stderr) = run_source(
        "several_per",
        "operator through{
            r: any relation,
            of: integer per r,
            s: relation { n: integer, x: integer },
            t: relation { n: integer, y: integer },
        }: integer [
            sum{r: join{r extend { n := of }, s, t}, of: of + x + y}
        ];

        operator on_its_own{
            r: any relation,
            of: integer per r,
            s: relation { n: integer, x: integer },
            t: relation { n: integer, y: integer },
        }: relation { n: integer, x: integer, y: integer } [
            join{r{of} rename {of as n}, s, t}
        ];

        operator main{} [
            var one := 1;
            const a := series{from: 1, to: 6000} extend { p := value * one, q := value mod 7 } remove {value};
            const s := series{from: 1, to: 6000} extend { n := value * one, x := value mod 3 } remove {value};
            const t := series{from: 1, to: 20} extend { n := value * 2 * one, y := value } remove {value};
            call debug_value{v: through{r: a, of: p + q, s, t}};
            const m := a extend { n := p + q };
            const j := m join s;
            const by_hand := j join t;
            call debug_value{v: sum{r: by_hand, of: n + x + y}};
            call debug_value{v: on_its_own{r: a, of: p + q, s, t} = by_hand{n, x, y}};
        ];
",
    );
    assert_eq!(code, 0, "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 3, "{stderr}");
    assert_eq!(lines[0], lines[1], "{stderr}");
    assert_ne!(lines[0], "0", "{stderr}");
    assert_eq!(lines[2], "true", "{stderr}");
}

/// An operand a join of several can give up early is no use to anything after it, and one that
/// goes on being read isn't given up: whichever way it comes to be the last read, and with the
/// join in a loop, so that a slot is used again.
#[test]
fn a_join_of_several_leaves_what_is_read_after_it() {
    let (code, stderr) = run_source(
        "several_read_after",
        &format!(
            "operator main{{}} [{SEVERAL_OPERANDS}
    const once := a extend {{ w := p + 1 }};
    call debug_value{{v: count{{r: join{{once, b, c}}}}}};
    const kept := a extend {{ w := p + 2 }};
    call debug_value{{v: count{{r: join{{kept, b, c}}}}}};
    call debug_value{{v: count{{r: kept}}}};
    call debug_value{{v: count{{r: join{{kept, kept rename {{ w as y }}, b}}}}}};
    var i := 0;
    var total := 0;
    while i < 3 loop [
        const each := a extend {{ w := p + i }};
        total := total + count{{r: join{{each, b, c}}}};
        i := i + 1;
    ];
    call debug_value{{v: total}};
];
"
        ),
    );
    assert_eq!(code, 0, "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 5, "{stderr}");
    assert_eq!(lines[0], lines[1], "{stderr}");
    assert_eq!(lines[2], "5000", "{stderr}");
    let once: u64 = lines[0].parse().unwrap();
    assert!(once > 0, "{stderr}");
    assert_eq!(lines[4], (once * 3).to_string(), "{stderr}");
}

/// Computing the operands first, in the plan's order, leaves the same failure first, however
/// the join goes on to be ordered.
#[test]
fn a_join_of_several_fails_as_the_plan_would() {
    let (code, stderr) = run_source(
        "several_fails",
        &format!(
            "operator main{{}} [{SEVERAL_OPERANDS}
    const n := count{{r: e}} - 3;
    call debug_value{{v: count{{r: join{{a extend {{ w := 1 div n }}, b extend {{ y := 2 div n }}, c}}}}}};
];
"
        ),
    );
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("1 div 0"), "{stderr}");
    assert!(!stderr.contains("2 div 0"), "{stderr}");
}

/// A guard across operands comes between two of them, at a place fixed when compiling, and what
/// it is compiled for has the attributes of those under it, so it stays there, and what it
/// restricts is one operand of the rest, if any. A condition that reads the whole AND, as one
/// that can fail does, reads what any order gives. A closure is found from what it is matched with, and a relation of one tuple
/// can be a lookup. Everything else chooses.
#[test]
fn what_cant_be_reordered_keeps_the_plan_fixed_when_compiling() {
    let program = |several: &str| {
        format!(
            "operator main{{}} [{SEVERAL_OPERANDS}
    call debug_value{{v: count{{r: {several}}}}};
];
"
        )
    };
    // The test can fail: each of these is the same but for what the name says.
    for several in [
        "join{a, b, c}",
        "join{a, b, c} where p > 1",
        "join{a, b, c} where s > 1 and p > 1",
        "join{a, b, c} where p < s",
        "join{a, b, c} where p + q < k",
        "join{a, b, c, d} where p < q",
    ] {
        assert!(joined_as_it_runs(&program(several)), "{several}");
    }
    for several in [
        "a join b",
        "join{a, b, c} where p < q",
        "join{a, b, c} where p < q and q > k",
        "join{a, b, relation { { q: one } }}",
        "join{a, c, (b{k, q}) tclose}",
    ] {
        assert!(!joined_as_it_runs(&program(several)), "{several}");
    }
}

/// `join{few, big, keys}` and its reverse, for `big` of `LEFT` tuples: joining `big` with `few`
/// first makes nearly all of them again, and with `keys` first, hardly any. Which is which is
/// only known as the program runs. What is held at the peak is what the operands hold, either
/// way round.
const JOIN_OPERANDS_FOR_MEMORY: &str = "
    var one := 1;
    const few := series{from: 1, to: 100} extend { s := value mod 5 * one } remove {value};
    const big := series{from: 1, to: LEFT} extend { s := value mod 5 * one, k := value * one } remove {value};
    const keys := series{from: 1, to: 2000} extend { k := value * 150 * one } remove {value};
";

#[test]
fn a_join_of_several_starts_from_what_makes_least() {
    let operators = |body: &str| {
        format!(
            "operator main{{}} [{JOIN_OPERANDS_FOR_MEMORY}\n    call debug_value{{v: {body}}};\n];\n"
        )
    };
    let left = 300_000;
    let holding = peak_memory_of(
        "several_holding",
        &operators("count{r: few} + count{r: big} + count{r: keys}"),
        left,
    );
    let source = operators("count{r: join{few, big, keys}}").replace("LEFT", &left.to_string());
    assert!(joined_as_it_runs(&source));
    for (name, body) in [
        ("several_few_first", "count{r: join{few, big, keys}}"),
        ("several_keys_first", "count{r: join{keys, big, few}}"),
    ] {
        let joined = peak_memory_of(name, &operators(body), left);
        assert!(
            joined < holding + holding / 8,
            "{name}: {joined} bytes at the peak, with the operands {holding}"
        );
    }
}

/// What is made on the way goes as soon as it has been joined in its turn. Here, each join makes
/// as many tuples as any operand has, with an attribute more, so at the worst, the last but one
/// and what is made of it are held with the last operand, and what it takes to find the matches:
/// which came to 158 bytes for each tuple of `big` when this was written, and 24 more if what
/// is made lasts until the end.
#[test]
fn a_join_of_several_gives_up_what_it_makes_on_the_way() {
    let program = "operator main{} [
    var one := 1;
    call debug_value{v: count{r: join{
        series{from: 1, to: LEFT} extend { k := value * one, a := value } remove {value},
        series{from: 1, to: LEFT} extend { k := value * one, b := value } remove {value},
        series{from: 1, to: LEFT} extend { k := value * one, c := value } remove {value},
        series{from: 1, to: LEFT} extend { k := value * one, d := value } remove {value},
    }}};
];
";
    let big = 400_000;
    let held = peak_memory_of("several_on_the_way", program, big)
        - peak_memory_of("several_on_the_way", program, 1);
    assert!(
        held < u64::from(big) * 170,
        "{} bytes for each tuple",
        held / u64::from(big)
    );
}

/// Operands that nothing else reads are given up as soon as they are joined, before what they
/// make is joined with the rest, which is what takes most of the memory here. A `const` read
/// again later can't be, which is what the two are compared with.
#[test]
fn a_join_of_several_gives_up_what_it_has_joined() {
    let wide = "series{from: 1, to: LEFT} extend { k := value * one, a1 := value, a2 := value, a3 := value } remove {value}";
    let rest = "const keys := series{from: 1, to: 1000} extend { k := value * one, b := value } remove {value};
    const many := series{from: 1, to: LEFT} extend { k := value mod 1000 + 1, c := value * one } remove {value};";
    let left = 400_000;
    let program =
        |body: String| format!("operator main{{}} [\n    var one := 1;\n    {rest}\n{body}\n];\n");
    let read_later = peak_memory_of(
        "several_read_later",
        &program(format!(
            "    const wide := {wide};
    call debug_value{{v: count{{r: join{{wide, keys, many}}}}}};
    call debug_value{{v: count{{r: wide}}}};"
        )),
        left,
    );
    for (name, body) in [
        (
            "several_made_for_it",
            format!("    call debug_value{{v: count{{r: join{{{wide}, keys, many}}}}}};"),
        ),
        (
            "several_bound_once",
            format!(
                "    call debug_value{{v: count{{r: [
        const wide := {wide};
        const other := wide;
        join{{other, keys, many}}
    ]}}}};"
            ),
        ),
    ] {
        let peak = peak_memory_of(name, &program(body), left);
        assert!(
            peak < read_later - read_later / 8,
            "{name}: {peak} bytes at the peak, and {read_later} with the operand kept"
        );
    }
}
