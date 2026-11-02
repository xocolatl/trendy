//! Other languages' ways to use a unit, written where a definition goes:
//! Python's `import a.b` and `from a.b import c`, Java's `import a.b.*;`,
//! Haskell's `import qualified A.B as C`, Go's `import "a/b"`, JavaScript's
//! `import { c } from "./a/b.js";`, Rust's `use a::b::*;`, C#'s `using
//! A.B;`, C++'s `using namespace a::b;`, Ruby's and Lua's `require "a/b"`,
//! Ada's `with A.B;` and `use A.B;`, and OCaml's and F#'s `open A.B`. The
//! parser reports each with the `uses` it means, when that is certain.

use trendy_syntax::SyntaxKind;

use crate::Found;

/// Another language's import, as [`recognized`] reads it.
pub(crate) struct Import {
    /// The language's way, as the message names it: "Python's `import`".
    pub(crate) habit: &'static str,
    /// What it uses: the units, each with the name the import gives it, or
    /// else what the `uses` would be for it, if the import doesn't say.
    pub(crate) uses: Uses,
}

/// What an import uses, in trendy.
pub(crate) enum Uses {
    /// These units, each with the name it gives one, if any.
    Units(Vec<(String, Option<String>)>),
    /// The unit a name of Rust's or C++'s path is in, or the path itself:
    /// `use lakes::depths::deepest;` imports `deepest` from `lakes.depths`,
    /// or the unit `lakes.depths.deepest`, which only the program knows.
    Either { item: String, within: String },
    /// Nothing the parser can name: a relative import, as Python's `from .
    /// import x`, or a path that isn't a unit's.
    Unknown,
}

/// The words that start another language's import.
pub(crate) const IMPORT_WORDS: [&str; 8] = [
    "import",
    "from",
    "use",
    "using",
    "require",
    "require_relative",
    "with",
    "open",
];

/// The import the statement `tokens` is, starting with one of
/// [`IMPORT_WORDS`], in any case, if it is one.
pub(crate) fn recognized(tokens: &[Found]) -> Option<Import> {
    let word = tokens.first()?.text.to_lowercase();
    let rest = without_semicolon(&tokens[1..]);
    match word.as_str() {
        "import" => import(rest),
        "from" => from(rest),
        "use" => used(rest),
        "using" => using(rest),
        "require" | "require_relative" => require(rest),
        "with" => with(tokens),
        "open" => {
            let (units, end) = dotted_list(rest)?;
            (end == rest.len()).then_some(Import {
                habit: "OCaml's and F#'s `open`",
                uses: Uses::Units(units.into_iter().map(|u| (u, None)).collect()),
            })
        }
        _ => None,
    }
}

/// `tokens` without the `;` that ends them, if it does.
fn without_semicolon<'t, 'a>(tokens: &'t [Found<'a>]) -> &'t [Found<'a>] {
    match tokens.split_last() {
        Some((last, rest)) if last.kind == SyntaxKind::Semicolon => rest,
        _ => tokens,
    }
}

/// Whether `found` is a word: a name, or a keyword, which a unit's name can
/// have as a part.
fn is_word(found: &Found) -> bool {
    found.kind == SyntaxKind::Ident || found.kind.is_keyword()
}

/// Whether `found` is the word `word`, in any case.
fn is(found: Option<&Found>, word: &str) -> bool {
    found.is_some_and(|f| is_word(f) && f.text.eq_ignore_ascii_case(word))
}

/// A name of words separated by `.` or by `::` at the start of `tokens`:
/// its words, the place after it, and whether `::` separated them.
fn path(tokens: &[Found]) -> Option<(Vec<String>, usize, bool)> {
    let first = tokens.first().filter(|f| is_word(f))?;
    let mut words = vec![first.text.to_string()];
    let mut i = 1;
    let mut colons = false;
    loop {
        let at = |n: usize| tokens.get(i + n);
        match at(0).map(|f| f.kind) {
            Some(SyntaxKind::Dot) if at(1).is_some_and(is_word) => {
                words.push(at(1)?.text.to_string());
                i += 2;
            }
            Some(SyntaxKind::Colon)
                if at(1).is_some_and(|f| f.kind == SyntaxKind::Colon)
                    && at(2).is_some_and(is_word) =>
            {
                words.push(at(2)?.text.to_string());
                colons = true;
                i += 3;
            }
            _ => return Some((words, i, colons)),
        }
    }
}

/// Names separated by `,`, each a [`path`] with `.`: the units, and the
/// place after them.
fn dotted_list(tokens: &[Found]) -> Option<(Vec<String>, usize)> {
    let mut units = Vec::new();
    let mut i = 0;
    loop {
        let (words, end, colons) = path(&tokens[i..])?;
        if colons {
            return None;
        }
        units.push(words.join("."));
        i += end;
        if tokens.get(i).is_some_and(|f| f.kind == SyntaxKind::Comma) {
            i += 1;
        } else {
            return Some((units, i));
        }
    }
}

/// The unit a path in quotes names, as `"./lakes/depths.js"` names
/// `lakes.depths`.
fn quoted(found: &Found) -> Option<String> {
    let text = found
        .text
        .strip_prefix('"')?
        .strip_suffix('"')
        .filter(|t| !t.contains('\\'))?;
    trendy_syntax::unit::named_by(text)
}

/// The name after `as` at the start of `tokens`, if it is there: the name,
/// and the place after it.
fn alias(tokens: &[Found]) -> (Option<String>, usize) {
    match tokens {
        [as_, name, ..] if as_.kind == SyntaxKind::AsKw && name.kind == SyntaxKind::Ident => {
            (Some(name.text.to_string()), 2)
        }
        _ => (None, 0),
    }
}

/// What follows `import`: Haskell's `qualified`, Go's paths, JavaScript's
/// `… from "path"`, or Python's and Java's names.
fn import(rest: &[Found]) -> Option<Import> {
    if let Some(from) = rest.iter().position(|f| is(Some(f), "from"))
        && let Some(path) = rest
            .get(from + 1)
            .filter(|f| f.kind == SyntaxKind::TextLiteral)
    {
        // `* as d`, the only clause that names the module itself.
        let named = match &rest[..from] {
            [star, as_, name] if star.kind == SyntaxKind::Star && as_.kind == SyntaxKind::AsKw => {
                Some(name.text.to_string())
            }
            _ => None,
        };
        return Some(Import {
            habit: "JavaScript's `import … from`",
            uses: quoted(path).map_or(Uses::Unknown, |unit| Uses::Units(vec![(unit, named)])),
        });
    }
    let go = "Go's `import`";
    match rest {
        [path] if path.kind == SyntaxKind::TextLiteral => {
            return Some(Import {
                habit: go,
                uses: quoted(path).map_or(Uses::Unknown, |unit| Uses::Units(vec![(unit, None)])),
            });
        }
        [name, path] if name.kind == SyntaxKind::Ident && path.kind == SyntaxKind::TextLiteral => {
            return Some(Import {
                habit: go,
                uses: quoted(path).map_or(Uses::Unknown, |unit| {
                    Uses::Units(vec![(unit, Some(name.text.to_string()))])
                }),
            });
        }
        [open, inner @ .., close]
            if open.kind == SyntaxKind::LParen && close.kind == SyntaxKind::RParen =>
        {
            return go_group(inner);
        }
        _ => {}
    }
    let (habit, rest) = if is(rest.first(), "qualified") {
        ("Haskell's `import qualified`", &rest[1..])
    } else {
        ("Python's and Java's `import`", rest)
    };
    let mut units = Vec::new();
    let mut i = 0;
    loop {
        let (words, end, colons) = path(&rest[i..])?;
        if colons {
            return None;
        }
        i += end;
        // Java's `.*`, every name of the package.
        if rest.get(i).is_some_and(|f| f.kind == SyntaxKind::Dot)
            && rest.get(i + 1).is_some_and(|f| f.kind == SyntaxKind::Star)
        {
            i += 2;
        }
        let (named, after) = alias(&rest[i..]);
        i += after;
        units.push((words.join("."), named));
        // Haskell's list of the names it imports, `(Map, fromList)`.
        if rest.get(i).is_some_and(|f| f.kind == SyntaxKind::LParen) {
            break;
        }
        if rest.get(i).is_some_and(|f| f.kind == SyntaxKind::Comma) {
            i += 1;
        } else {
            break;
        }
    }
    let haskell_list = rest.get(i).is_some_and(|f| f.kind == SyntaxKind::LParen)
        && rest.last().is_some_and(|f| f.kind == SyntaxKind::RParen);
    (i == rest.len() || haskell_list).then_some(Import {
        habit,
        uses: Uses::Units(units),
    })
}

/// Go's `import ( "a" d "b" )`, each path with the name it gives it, if
/// any.
fn go_group(inner: &[Found]) -> Option<Import> {
    let mut units = Vec::new();
    let mut certain = true;
    let mut i = 0;
    while i < inner.len() {
        let (named, path) = match &inner[i..] {
            [name, path, ..]
                if name.kind == SyntaxKind::Ident && path.kind == SyntaxKind::TextLiteral =>
            {
                i += 2;
                (Some(name.text.to_string()), path)
            }
            [path, ..] if path.kind == SyntaxKind::TextLiteral => {
                i += 1;
                (None, path)
            }
            _ => return None,
        };
        match quoted(path) {
            Some(unit) => units.push((unit, named)),
            None => certain = false,
        }
    }
    Some(Import {
        habit: "Go's `import ( … )`",
        uses: if certain {
            Uses::Units(units)
        } else {
            Uses::Unknown
        },
    })
}

/// What follows Python's `from`: the module, then `import` and its names.
fn from(rest: &[Found]) -> Option<Import> {
    let habit = "Python's `from … import`";
    // `from . import x` and `from .lakes import x`, from the package the
    // importing module is in.
    if rest.first().is_some_and(|f| f.kind == SyntaxKind::Dot) {
        return rest
            .iter()
            .any(|f| is(Some(f), "import"))
            .then_some(Import {
                habit,
                uses: Uses::Unknown,
            });
    }
    let (words, end, colons) = path(rest)?;
    (!colons && is(rest.get(end), "import")).then(|| Import {
        habit,
        uses: Uses::Units(vec![(words.join("."), None)]),
    })
}

/// What follows `use`: Rust's path, with `::`, or Ada's names, with `.`.
fn used(rest: &[Found]) -> Option<Import> {
    let (mut words, end, colons) = path(rest)?;
    if !colons {
        // Ada's `use Lakes.Depths;`, of the packages `with` named.
        let (units, end) = dotted_list(rest)?;
        return (end == rest.len()).then_some(Import {
            habit: "Ada's `use`",
            uses: Uses::Units(units.into_iter().map(|u| (u, None)).collect()),
        });
    }
    let habit = "Rust's `use`";
    // The crate's own modules are the program's units.
    if words.first().is_some_and(|w| w == "crate") {
        words.remove(0);
    }
    if words.first().is_some_and(|w| w == "self" || w == "super") || words.is_empty() {
        return Some(Import {
            habit,
            uses: Uses::Unknown,
        });
    }
    let after = &rest[end..];
    let separator = |f: &[Found]| {
        f.len() >= 2 && f[0].kind == SyntaxKind::Colon && f[1].kind == SyntaxKind::Colon
    };
    // `use lakes::depths::*;` and `use lakes::depths::{deepest, record};`
    // import from the module itself.
    let whole = separator(after)
        && matches!(
            after.get(2).map(|f| f.kind),
            Some(SyntaxKind::Star | SyntaxKind::LBrace)
        );
    if whole {
        return Some(Import {
            habit,
            uses: Uses::Units(vec![(words.join("."), None)]),
        });
    }
    let (_, after_alias) = alias(after);
    if after_alias != after.len() {
        return None;
    }
    Some(Import {
        habit,
        uses: either(&words),
    })
}

/// A path whose last word may be a name its unit declares, or a unit.
fn either(words: &[String]) -> Uses {
    match words.split_last() {
        Some((_, within)) if !within.is_empty() => Uses::Either {
            item: words.join("."),
            within: within.join("."),
        },
        _ => Uses::Units(vec![(words.join("."), None)]),
    }
}

/// What follows `using`: C#'s namespace, or its alias, `using d =
/// Lakes.Depths;`, or C++'s `namespace` and its path.
fn using(rest: &[Found]) -> Option<Import> {
    if let [name, eq, target @ ..] = rest
        && name.kind == SyntaxKind::Ident
        && eq.kind == SyntaxKind::Eq
    {
        let (words, end, colons) = path(target)?;
        return (end == target.len() && !colons).then(|| Import {
            habit: "C#'s `using`",
            uses: Uses::Units(vec![(words.join("."), Some(name.text.to_string()))]),
        });
    }
    let (habit, rest) = if is(rest.first(), "namespace") {
        ("C++'s `using namespace`", &rest[1..])
    } else if is(rest.first(), "static") {
        ("C#'s `using static`", &rest[1..])
    } else {
        ("C#'s `using`", rest)
    };
    let (words, end, colons) = path(rest)?;
    if end != rest.len() {
        return None;
    }
    // C++'s `using lakes::depths;` names one thing a namespace declares.
    let uses = if colons && habit == "C#'s `using`" {
        either(&words)
    } else {
        Uses::Units(vec![(words.join("."), None)])
    };
    Some(Import {
        habit: if colons && habit == "C#'s `using`" {
            "C++'s `using`"
        } else {
            habit
        },
        uses,
    })
}

/// What follows Ruby's and Lua's `require`: a path in quotes, or in
/// parentheses too.
fn require(rest: &[Found]) -> Option<Import> {
    let path = match rest {
        [path] => path,
        [open, path, close]
            if open.kind == SyntaxKind::LParen && close.kind == SyntaxKind::RParen =>
        {
            path
        }
        _ => return None,
    };
    (path.kind == SyntaxKind::TextLiteral).then(|| Import {
        habit: "Ruby's and Lua's `require`",
        uses: quoted(path).map_or(Uses::Unknown, |unit| Uses::Units(vec![(unit, None)])),
    })
}

/// Ada's `with Lakes.Depths, Recipes;`, which only names, then `;`, make,
/// so that SQL's `WITH t AS (…)` isn't one.
fn with(tokens: &[Found]) -> Option<Import> {
    let (last, rest) = tokens[1..].split_last()?;
    if last.kind != SyntaxKind::Semicolon {
        return None;
    }
    let (units, end) = dotted_list(rest)?;
    (end == rest.len()).then_some(Import {
        habit: "Ada's `with`",
        uses: Uses::Units(units.into_iter().map(|u| (u, None)).collect()),
    })
}
