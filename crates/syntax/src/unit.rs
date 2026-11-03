//! Units' names. A unit is a file, named by its path from the directory of
//! `main`'s file: its directories and its file's stem, each a word, joined
//! by `.`, as `lakes/depths.trendy` is `lakes.depths`.

/// What a `uses` is, for the notes of the errors about one.
pub const USES_RULE: &str = "`uses { lakes.depths };`, first in a section, names the units whose interfaces the section's declarations see, each by its path from the directory of `main`'s file, as `lakes/depths.trendy` is `lakes.depths`; every name an interface declares comes with it";

/// What a qualified name is, for the notes of the errors about one.
pub const QUALIFIED_RULE: &str = "a name another unit declares can be qualified by that unit's name and `::`, as `lakes.depths::metres`: the unit's path from the directory of `main`'s file, its parts between `.`s, or the name its `uses` gives it after `as`, which a section sees once it uses the unit, as every unit sees the prelude, `trendy.prelude`; a unit's own names are never qualified, as they hide every other unit's";

/// What starts the names of the units that come with the compiler, which
/// are no files of a program.
pub const COMPILERS: &str = "trendy.";

/// The file of the unit `name`, from the directory of `main`'s file: its
/// parts as directories, the last its file's stem, as `lakes.depths` is
/// `lakes/depths.trendy`.
#[must_use]
pub fn path_of(name: &str) -> String {
    format!("{}.trendy", name.replace('.', "/"))
}

/// The extensions of the files other languages' imports name, which a
/// path's unit leaves out: trendy's own, and those of C, C++, JavaScript,
/// TypeScript, Python, Ruby, Lua, Go, Pascal, Haskell, Rust, Java and C#.
const EXTENSIONS: [&str; 24] = [
    "trendy", "h", "hh", "hpp", "hxx", "c", "js", "mjs", "cjs", "jsx", "ts", "tsx", "mts", "py",
    "rb", "lua", "go", "pas", "pp", "dpr", "hs", "rs", "java", "cs",
];

/// The unit `path` names, as another language's import writes a file or
/// a module, if each of its parts is a word: `./lakes/depths.js` and
/// `lakes/depths` are `lakes.depths`, and so, without a `/`, is the
/// module `lakes.depths`. A path from the root, or out of the program's
/// directory, as `../recipes` is, names none, nor does a site's, as
/// `github.com/x` is.
#[must_use]
pub fn named_by(path: &str) -> Option<String> {
    let path = path.strip_prefix("./").unwrap_or(path);
    let path = match path.rsplit_once('.') {
        Some((stem, extension)) if !extension.contains('/') && EXTENSIONS.contains(&extension) => {
            stem
        }
        _ => path,
    };
    let separator = if path.contains('/') { '/' } else { '.' };
    let parts: Vec<&str> = path.split(separator).collect();
    parts
        .iter()
        .all(|part| crate::is_word(part))
        .then(|| parts.join("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_names_a_unit() {
        for path in [
            "lakes/depths.trendy",
            "./lakes/depths.js",
            "lakes/depths",
            "lakes.depths",
            "lakes/depths.h",
        ] {
            assert_eq!(named_by(path).as_deref(), Some("lakes.depths"), "{path}");
        }
        assert_eq!(named_by("recipes").as_deref(), Some("recipes"));
        for path in [
            "",
            "/lakes/depths",
            "../recipes",
            "github.com/x/y",
            "lakes/9depths",
            "a//b",
        ] {
            assert_eq!(named_by(path), None, "{path}");
        }
    }
}
