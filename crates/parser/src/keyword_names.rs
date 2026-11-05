//! A keyword used as the name of what a program declares. The word means something
//! else to the parser, so each place it is used as a name would report something else
//! again, none of them the cause. So once a keyword is declared as a name, the file
//! is parsed again with that word a name wherever it was used as one, and there is
//! one error, at where it is declared, whose fix renames every one of them.

use std::collections::{BTreeMap, BTreeSet};

use trendy_diag::{Code, Diagnostic, Edit, Span};
use trendy_lexer::Token;
use trendy_syntax::ast::AstNode;
use trendy_syntax::{SyntaxElement, SyntaxKind, SyntaxToken, span};

use crate::{Parse, errors};

/// A keyword used as a name.
pub(crate) struct Named {
    /// The token where it is first declared.
    declared: Span,
    /// Each token it is a name in, by its index among the tokens.
    used: Vec<usize>,
}

/// Each keyword `tokens` of `source` use as a name, by its kind, given how they
/// parse as they are: `first`.
pub(crate) fn used(source: &str, tokens: &[Token], first: &Parse) -> BTreeMap<SyntaxKind, Named> {
    // What parses without an error uses every keyword as one.
    if first.errors.is_empty() {
        return BTreeMap::new();
    }
    let as_keyword = keyword_uses(first);
    let significant: Vec<usize> = (0..tokens.len())
        .filter(|&i| !tokens[i].kind.is_trivia())
        .collect();
    let token_at = |i: usize| {
        (
            tokens[i].kind,
            &source[tokens[i].span.start..tokens[i].span.end],
        )
    };
    // Where it is declared, wherever the tokens around it say so, as much of this
    // isn't anywhere the parser would look for a name.
    let mut declared: BTreeMap<SyntaxKind, Span> = BTreeMap::new();
    for (at, &i) in significant.iter().enumerate() {
        let before = |n: usize| at.checked_sub(n).map(|at| token_at(significant[at]));
        if tokens[i].kind.is_keyword()
            && !as_keyword.contains(&tokens[i].span.start)
            && is_declared_name(
                before(1),
                before(2),
                significant.get(at + 1).map(|&after| token_at(after)),
            )
        {
            declared.entry(tokens[i].kind).or_insert(tokens[i].span);
        }
    }
    // A use of it, as in `group := 1;`, if nothing here declares it.
    for error in &first.errors {
        if error.code == Code::KeywordAsName
            && let Some(token) = tokens.iter().find(|t| t.span == error.primary.span)
        {
            declared.entry(token.kind).or_insert(token.span);
        }
    }
    let mut named: BTreeMap<SyntaxKind, Named> = declared
        .into_iter()
        .map(|(kind, declared)| {
            (
                kind,
                Named {
                    declared,
                    used: Vec::new(),
                },
            )
        })
        .collect();
    for (i, token) in tokens.iter().enumerate() {
        if let Some(found) = named.get_mut(&token.kind)
            && !as_keyword.contains(&token.span.start)
        {
            found.used.push(i);
        }
    }
    named
}

/// Whether a keyword the parser didn't take as one is where a name is declared, with
/// `before`, `further` before that, and `after`, each its kind and its text, as some of
/// what starts a declaration is a keyword only there.
fn is_declared_name(
    before: Option<(SyntaxKind, &str)>,
    further: Option<(SyntaxKind, &str)>,
    after: Option<(SyntaxKind, &str)>,
) -> bool {
    use SyntaxKind as K;
    let Some((before, word)) = before else {
        return false;
    };
    let next = after.map(|(kind, _)| kind);
    match before {
        K::VarKw | K::ConstKw | K::OperatorKw => matches!(
            next,
            Some(K::ColonEq | K::Colon | K::Semicolon | K::LBrace | K::Comma | K::RBrace)
        ),
        K::AsKw => true,
        K::ForKw => after.is_some_and(|(_, text)| text == "in"),
        // An attribute, a parameter, an argument or a type parameter, or a value
        // for one, which is no keyword's place.
        K::LBrace | K::Comma => matches!(next, Some(K::Colon | K::ColonEq)),
        K::Ident if word.eq_ignore_ascii_case("out") => next == Some(K::Colon),
        K::Ident if starts_definition(further) => match word.to_ascii_lowercase().as_str() {
            "heading" => matches!(next, Some(K::ColonEq | K::Eq | K::LBrace)),
            "type" => {
                matches!(next, Some(K::LBrace | K::UnionKw))
                    || after.is_some_and(|(_, text)| text == "is")
            }
            "database" => next == Some(K::Semicolon),
            "kind" => next == Some(K::LBrace),
            "constraint" => matches!(next, Some(K::ColonEq | K::Colon)),
            _ => false,
        },
        _ => false,
    }
}

/// Whether what comes after `before` can start a definition: the file's first token,
/// or what comes after the last one, or after a section's header. So `x := kind where {`,
/// of a variable named `kind`, declares nothing.
fn starts_definition(before: Option<(SyntaxKind, &str)>) -> bool {
    match before {
        None | Some((SyntaxKind::Semicolon | SyntaxKind::RBracket | SyntaxKind::RBrace, _)) => true,
        Some((SyntaxKind::Ident, word)) => {
            word.eq_ignore_ascii_case("interface") || word.eq_ignore_ascii_case("implementation")
        }
        _ => false,
    }
}

/// Where each keyword starts that `first` parses as that keyword: not in what it
/// skipped, not as a name, and with no error about it or what comes next, unless that
/// is a keyword itself, as `var var := 1;` starts with one.
fn keyword_uses(first: &Parse) -> Vec<usize> {
    // What is missing is reported where it goes, with what was found there.
    let reported: BTreeSet<usize> = first
        .errors
        .iter()
        .flat_map(|e| std::iter::once(&e.primary).chain(&e.secondary))
        .map(|label| label.span.start)
        .collect();
    let at = |token: &SyntaxToken| span(token.text_range());
    let missing_after = |token: &SyntaxToken| {
        std::iter::successors(token.next_token(), SyntaxToken::next_token)
            .find(|next| !next.kind().is_trivia())
            .filter(|next| !next.kind().is_keyword())
            .is_some_and(|next| reported.contains(&at(&next).start))
    };
    first
        .tree
        .syntax()
        .descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .filter(|token| token.kind().is_keyword())
        .filter(|token| {
            !token.parent_ancestors().any(|node| {
                matches!(
                    node.kind(),
                    SyntaxKind::Error | SyntaxKind::Name | SyntaxKind::NameRef
                )
            })
        })
        .filter(|token| !reported.contains(&at(token).start) && !missing_after(token))
        .map(|token| at(&token).start)
        .collect()
}

/// Whether `error`, of the second parse, is only that a word of `tokens` in `named` is
/// now an identifier spelled as a keyword is: not a keyword in another case.
pub(crate) fn is_of_the_second_parse(
    error: &Diagnostic,
    tokens: &[Token],
    named: &BTreeMap<SyntaxKind, Named>,
) -> bool {
    error.code == Code::MiscasedKeyword
        && named
            .values()
            .flat_map(|found| &found.used)
            .any(|&i| tokens[i].span == error.primary.span)
}

/// `tokens`, with each word in `named` an identifier wherever it is a name.
pub(crate) fn as_identifiers(tokens: &[Token], named: &BTreeMap<SyntaxKind, Named>) -> Vec<Token> {
    let mut tokens = tokens.to_vec();
    for found in named.values() {
        for &i in &found.used {
            tokens[i].kind = SyntaxKind::Ident;
        }
    }
    tokens
}

/// The error for each of `named`, the keywords of `tokens` of `source` used as names:
/// at where it is declared, with a fix that renames each, to a name nothing there has.
pub(crate) fn errors(
    source: &str,
    tokens: &[Token],
    named: &BTreeMap<SyntaxKind, Named>,
) -> Vec<Diagnostic> {
    let text = |t: &Token| &source[t.span.start..t.span.end];
    named
        .values()
        .map(|found| {
            let word = source[found.declared.start..found.declared.end].to_string();
            let renamed =
                trendy_diag::unused_name(&word, |name| tokens.iter().any(|t| text(t) == name));
            let edits = found
                .used
                .iter()
                .map(|&i| Edit {
                    span: tokens[i].span,
                    replacement: renamed.clone(),
                })
                .collect();
            errors::keyword_named(&word, found.declared, &renamed, edits)
        })
        .collect()
}
