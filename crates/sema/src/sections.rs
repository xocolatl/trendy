//! A unit's sections: what its interface may declare and mention, and
//! where a declaration in the wrong one moves to, for the fixes.
//!
//! The interface declares the types, headings and operators other units
//! see, each operator by its heading alone; the implementation repeats an
//! interface operator's heading with its body, and keeps the unit's
//! variables, its versions, and whatever else no other unit sees. What
//! comes before either section's header is the implementation's.

use std::collections::{BTreeMap, BTreeSet};

use trendy_diag::{Diagnostic, Edit, Span};
use trendy_syntax::ast::{self, AstNode, Item, Section, SourceFile};
use trendy_syntax::{SyntaxKind, SyntaxNode};

use crate::errors;

/// Report what `file`'s interface can't have that its signatures don't
/// show: variables, which a unit never exports, and constraints on them,
/// and names of types or
/// headings only its implementation declares, which other units couldn't
/// see (A0067), each once.
pub(crate) fn check(file: &SourceFile, errors: &mut Vec<Diagnostic>) {
    uses_placement(file, errors);
    let mut implementation: BTreeMap<String, SyntaxNode> = BTreeMap::new();
    for item in file.items() {
        let (name, node) = match &item {
            Item::Type(decl) => (decl.name(), decl.syntax()),
            Item::Heading(decl) => (decl.name(), decl.syntax()),
            _ => continue,
        };
        if ast::section_of(node) == Section::Implementation
            && let Some(ident) = name.and_then(|n| n.ident())
        {
            implementation
                .entry(ident.text().to_string())
                .or_insert_with(|| node.clone());
        }
    }
    let mut reported = BTreeSet::new();
    for item in file.items() {
        if let Item::Variable(decl) = &item {
            if ast::section_of(decl.syntax()) == Section::Interface {
                errors.push(errors::interface_variable(
                    decl,
                    moved(file, decl.syntax(), Section::Implementation),
                ));
            }
            continue;
        }
        if let Item::Constraint(decl) = &item {
            // A database's constraints are part of it, as visible as it is.
            if decl.part_of().is_none() && ast::section_of(decl.syntax()) == Section::Interface {
                errors.push(errors::interface_constraint(
                    decl,
                    moved(file, decl.syntax(), Section::Implementation),
                ));
            }
            continue;
        }
        for (name, at) in mentioned(&item) {
            let Some(declared) = implementation.get(&name) else {
                continue;
            };
            if reported.insert(name.clone()) {
                errors.push(errors::implementation_name(
                    &name,
                    at,
                    declared_at(declared),
                    moved(file, declared, Section::Interface),
                ));
            }
        }
    }
}

/// Report each `uses` of `file` after a declaration of its section, with
/// a fix moving it before the first, and each second `uses` of a section,
/// with a fix adding the units it names to the first's (A0071): a
/// section's `uses` comes first in it, so that what its declarations see
/// is said at its top.
fn uses_placement(file: &SourceFile, errors: &mut Vec<Diagnostic>) {
    let mut first_uses: BTreeMap<Section, ast::UsesDecl> = BTreeMap::new();
    let mut first_declaration: BTreeMap<Section, SyntaxNode> = BTreeMap::new();
    for node in file.syntax().children() {
        let section = ast::section_of(&node);
        if let Some(uses) = ast::UsesDecl::cast(node.clone()) {
            if let Some(first) = first_uses.get(&section) {
                errors.push(errors::second_uses(
                    &uses,
                    first,
                    merged(file, first, &uses),
                ));
            } else {
                if let Some(declaration) = first_declaration.get(&section) {
                    errors.push(errors::late_uses(
                        &uses,
                        declared_at(declaration),
                        moved_before(file, uses.syntax(), declaration),
                    ));
                }
                first_uses.insert(section, uses);
            }
        } else if Item::cast(node.clone()).is_some() {
            first_declaration.entry(section).or_insert(node);
        }
    }
}

/// The edits that add the units `second` names, but those `first` names
/// too, to `first`'s, and remove `second`, of `file`: none, if `first`
/// names none, which is reported.
fn merged(file: &SourceFile, first: &ast::UsesDecl, second: &ast::UsesDecl) -> Vec<Edit> {
    let text = |unit: &ast::UsedUnit| unit.syntax().text().to_string();
    let Some(last) = first.units().last() else {
        return Vec::new();
    };
    let named: BTreeSet<String> = first.units().map(|u| text(&u)).collect();
    let added: Vec<String> = second
        .units()
        .map(|u| text(&u))
        .filter(|u| !named.contains(u))
        .collect();
    let mut edits = vec![removal(file, second.syntax())];
    if !added.is_empty() {
        edits.insert(
            0,
            Edit {
                span: Span::empty(usize::from(last.syntax().text_range().end())),
                replacement: format!(", {}", added.join(", ")),
            },
        );
    }
    edits
}

/// The edits that move `node`, of `file`, with the comments just before
/// it, to before the declaration `before`, with the comments just before
/// that, and a blank line between them.
fn moved_before(file: &SourceFile, node: &SyntaxNode, before: &SyntaxNode) -> Vec<Edit> {
    let start = attached_start(node);
    let text = file.syntax().text().to_string();
    let file_start = usize::from(file.syntax().text_range().start());
    let moved = &text[start - file_start..usize::from(node.text_range().end()) - file_start];
    vec![
        Edit {
            span: Span::empty(attached_start(before)),
            replacement: format!("{moved}\n\n"),
        },
        removal(file, node),
    ]
}

/// The names of types and headings `item`, if it is in its unit's
/// interface, mentions where other units see them, each with where: an
/// operator's parameters' and result's types, a database's relvar's type,
/// a heading's attributes', and
/// a type's components' and supertype's. Its body, its constraints and its
/// conversions are its unit's own.
fn mentioned(item: &Item) -> Vec<(String, Span)> {
    let parts: Vec<SyntaxNode> = match item {
        Item::Operator(op) if ast::section_of(op.syntax()) == Section::Interface => op
            .params()
            .map(|p| p.syntax().clone())
            .into_iter()
            .chain(op.result().map(|r| r.syntax().clone()))
            .collect(),
        Item::Relvar(decl) if ast::section_of(decl.syntax()) == Section::Interface => decl
            .type_expr()
            .map(|t| t.syntax().clone())
            .into_iter()
            .collect(),
        Item::Heading(decl) if ast::section_of(decl.syntax()) == Section::Interface => decl
            .heading()
            .map(|h| h.syntax().clone())
            .into_iter()
            .collect(),
        Item::Type(decl) if ast::section_of(decl.syntax()) == Section::Interface => decl
            .representations()
            .filter_map(|r| r.components())
            .map(|h| h.syntax().clone())
            .chain(decl.supertype().map(|s| s.syntax().clone()))
            .collect(),
        _ => Vec::new(),
    };
    parts
        .iter()
        .flat_map(SyntaxNode::descendants)
        .filter_map(|node| {
            let ident = ast::TypeRef::cast(node.clone())
                .and_then(|r| r.ident())
                .or_else(|| ast::HeadingRef::cast(node).and_then(|r| r.ident()))?;
            Some((
                ident.text().to_string(),
                trendy_syntax::span(ident.text_range()),
            ))
        })
        .collect()
}

/// Where the declaration `node` is named, for a label.
fn declared_at(node: &SyntaxNode) -> Span {
    let name = ast::TypeDecl::cast(node.clone())
        .and_then(|d| d.name())
        .or_else(|| ast::HeadingDecl::cast(node.clone()).and_then(|d| d.name()))
        .and_then(|n| n.ident());
    name.map_or_else(
        || trendy_syntax::span(node.text_range()),
        |ident| trendy_syntax::span(ident.text_range()),
    )
}

/// The edits that move the declaration `node`, of `file`, to the end of
/// its section `to`, with the comments just before it: removed where it
/// is, with the blank lines after it, and written there.
pub(crate) fn moved(file: &SourceFile, node: &SyntaxNode, to: Section) -> Vec<Edit> {
    let start = attached_start(node);
    let text = file.syntax().text().to_string();
    let file_start = usize::from(file.syntax().text_range().start());
    let declaration =
        text[start - file_start..usize::from(node.text_range().end()) - file_start].to_string();
    vec![removal(file, node), written_at_end(file, to, &declaration)]
}

/// The edit that removes the declaration `node`, of `file`, with the
/// comments just before it and the blank lines after it.
pub(crate) fn removal(file: &SourceFile, node: &SyntaxNode) -> Edit {
    Edit {
        span: Span::new(attached_start(node), next_start(file, node)),
        replacement: String::new(),
    }
}

/// The edit that writes `text`, a declaration, at the end of `file`'s
/// section `section`: before the implementation's header, for the
/// interface, and else at the end of the file, but for an implementation
/// with no header, which comes before the interface's.
pub(crate) fn written_at_end(file: &SourceFile, section: Section, text: &str) -> Edit {
    let header = |wanted: Section| {
        file.headers()
            .find(|h| h.section() == wanted)
            .map(|h| usize::from(h.syntax().text_range().start()))
    };
    let before = match section {
        Section::Interface => header(Section::Implementation),
        Section::Implementation => header(Section::Implementation)
            .is_none()
            .then(|| header(Section::Interface))
            .flatten(),
    };
    if let Some(at) = before {
        return Edit {
            span: Span::empty(at),
            replacement: format!("{text}\n\n"),
        };
    }
    Edit {
        span: Span::empty(usize::from(file.syntax().text_range().end())),
        replacement: format!("\n{text}\n"),
    }
}

/// Where the declaration `node` starts, with the comments just before it,
/// with no blank line between: what is about it.
fn attached_start(node: &SyntaxNode) -> usize {
    let mut start = usize::from(node.text_range().start());
    let mut sibling = node.prev_sibling_or_token();
    while let Some(element) = sibling {
        match element.kind() {
            SyntaxKind::Comment => start = usize::from(element.text_range().start()),
            SyntaxKind::Whitespace
                if element
                    .as_token()
                    .is_some_and(|t| t.text().matches('\n').count() <= 1) => {}
            _ => break,
        }
        sibling = element.prev_sibling_or_token();
    }
    start
}

/// Where what follows the declaration `node`, of `file`, starts: the next
/// declaration, a comment, or a header, or the end of the file, so that
/// removing up to it removes the blank lines after `node` too.
fn next_start(file: &SourceFile, node: &SyntaxNode) -> usize {
    let mut sibling = node.next_sibling_or_token();
    while let Some(element) = sibling {
        if element.kind() != SyntaxKind::Whitespace {
            return usize::from(element.text_range().start());
        }
        sibling = element.next_sibling_or_token();
    }
    usize::from(file.syntax().text_range().end())
}
