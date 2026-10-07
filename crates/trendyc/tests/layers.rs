//! Checks that crate dependencies match the `layers` block in
//! `docs/pipeline.md`. See the "Enforcement" section there for the rules.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// One line of the `layers` block, in document order.
struct Layer {
    name: String,
    allowed: BTreeSet<String>,
}

fn read_layers() -> Vec<Layer> {
    let doc = std::fs::read_to_string(workspace_root().join("docs/pipeline.md")).unwrap();
    let block = doc
        .split_once("```layers\n")
        .and_then(|(_, rest)| rest.split_once("```"))
        .map(|(block, _)| block)
        .expect("docs/pipeline.md has no ```layers block");

    block
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (name, deps) = line
                .split_once(':')
                .unwrap_or_else(|| panic!("layers line has no `:`: {line}"));
            Layer {
                name: name.trim().to_string(),
                allowed: deps.split_whitespace().map(String::from).collect(),
            }
        })
        .collect()
}

#[derive(Default)]
struct Deps {
    normal: BTreeSet<String>,
    dev: BTreeSet<String>,
}

/// Declared dependencies of every workspace package, keyed by package name.
fn read_workspace() -> BTreeMap<String, Deps> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let output = Command::new(cargo)
        .args(["metadata", "--format-version=1", "--no-deps"])
        .current_dir(workspace_root())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout).unwrap();

    let mut packages = BTreeMap::new();
    for package in metadata["packages"].as_array().unwrap() {
        let mut deps = Deps::default();
        for dep in package["dependencies"].as_array().unwrap() {
            let name = dep["name"].as_str().unwrap().to_string();
            match dep["kind"].as_str() {
                Some("dev") => deps.dev.insert(name),
                _ => deps.normal.insert(name),
            };
        }
        packages.insert(package["name"].as_str().unwrap().to_string(), deps);
    }
    packages
}

#[test]
fn dependencies_match_pipeline_doc() {
    let layers = read_layers();
    let workspace = read_workspace();
    let mut errors = Vec::new();

    let mut listed = BTreeSet::new();
    for layer in &layers {
        if !listed.insert(layer.name.as_str()) {
            errors.push(format!("{} is listed more than once", layer.name));
        }
    }
    for name in workspace.keys() {
        if !listed.contains(name.as_str()) {
            errors.push(format!("{name} is a workspace crate but is not listed"));
        }
    }

    for (i, layer) in layers.iter().enumerate() {
        let Some(deps) = workspace.get(&layer.name) else {
            errors.push(format!(
                "{} is listed but is not a workspace crate",
                layer.name
            ));
            continue;
        };
        let earlier: BTreeSet<&str> = layers[..i].iter().map(|l| l.name.as_str()).collect();

        for dep in &layer.allowed {
            if workspace.contains_key(dep) && !earlier.contains(dep.as_str()) {
                errors.push(format!(
                    "{}: allows {dep}, which is not listed above it",
                    layer.name
                ));
            }
        }
        for dep in deps.normal.difference(&layer.allowed) {
            errors.push(format!(
                "{}: depends on {dep}, which is not allowed",
                layer.name
            ));
        }
        for dep in layer.allowed.difference(&deps.normal) {
            errors.push(format!(
                "{}: is allowed {dep} but does not depend on it",
                layer.name
            ));
        }
        for dep in &deps.dev {
            if workspace.contains_key(dep) && !earlier.contains(dep.as_str()) {
                errors.push(format!(
                    "{}: dev-depends on {dep}, which is not an earlier layer",
                    layer.name
                ));
            }
        }
    }

    assert!(
        errors.is_empty(),
        "layer violations (see docs/pipeline.md):\n  {}",
        errors.join("\n  ")
    );
}
