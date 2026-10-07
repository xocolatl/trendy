use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Compile `examples/<name>.trendy`, run the executable, and return its exit code.
fn compile_and_run(name: &str) -> i32 {
    let input = workspace_root()
        .join("examples")
        .join(format!("{name}.trendy"));
    let output = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);

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

    Command::new(&output).status().unwrap().code().unwrap()
}

#[test]
fn empty_main() {
    assert_eq!(compile_and_run("empty_main"), 0);
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
