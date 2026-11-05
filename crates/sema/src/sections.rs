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
    types_completed(file, errors);
    let mut implementation: BTreeMap<String, SyntaxNode> = BTreeMap::new();
    for item in file.items() {
        let (name, node) = match &item {
            // The part of a type the interface declares, which it completes.
            Item::Type(decl) if decl.interface().is_some() => continue,
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

/// Report what, of each type `file`'s interface declares, isn't where its
/// users see what they need and nothing else: its possible
/// representations' conversions, which the interface writes none of, as
/// they are the implementation's (A0067), and its declaration in the
/// implementation, which completes it (see
/// [`ast::TypeDecl::implementation`]), unless it repeats the interface's
/// representations' components as the interface writes them, with the
/// conversions, and says nothing else (A0068, A0067).
fn types_completed(file: &SourceFile, errors: &mut Vec<Diagnostic>) {
    for decl in file.types() {
        if ast::section_of(decl.syntax()) != Section::Interface {
            continue;
        }
        let part = decl.implementation();
        let converted: Vec<ast::Representation> = decl
            .representations()
            .filter(|r| r.conversions().next().is_some())
            .collect();
        if !converted.is_empty() {
            let moved = conversions_moved(file, &decl, part.as_ref(), &converted);
            errors.push(errors::interface_conversions(&decl, &converted, moved));
        }
        if let Some(part) = part {
            part_checked(&decl, &part, errors);
        }
    }
}

/// The edits that move the conversions of `converted`, representations of
/// the type `decl`, of `file`'s interface, declares, to its declaration in
/// the implementation, `part`, after the same components there, or, if it
/// has none, to a new one, at the implementation's end; or `None` if
/// `part` has none of those, or they have conversions of their own.
fn conversions_moved(
    file: &SourceFile,
    decl: &ast::TypeDecl,
    part: Option<&ast::TypeDecl>,
    converted: &[ast::Representation],
) -> Option<Vec<Edit>> {
    // What follows each one's components' braces: its conversions.
    let after = |r: &ast::Representation| -> Option<(Span, String)> {
        let heading = r.components()?;
        let start = usize::from(heading.syntax().text_range().end());
        let end = usize::from(r.syntax().text_range().end());
        let text = r.syntax().text().to_string();
        let from = start - usize::from(r.syntax().text_range().start());
        Some((Span::new(start, end), text[from..].to_string()))
    };
    let mut edits = Vec::new();
    let mut moved = Vec::new();
    for representation in converted {
        let (span, text) = after(representation)?;
        edits.push(Edit {
            span,
            replacement: String::new(),
        });
        moved.push((representation, text));
    }
    let Some(part) = part else {
        // A declaration of its own, with each representation's components.
        let name = decl.name()?.ident()?.text().to_string();
        let mut written = Vec::new();
        for representation in decl.representations() {
            let components = components_written(&representation)?;
            let conversions = moved
                .iter()
                .find(|(r, _)| *r == &representation)
                .map_or("", |(_, text)| text.as_str());
            written.push(format!("    {components}{conversions},"));
        }
        let declaration = format!("type {name} {{\n{}\n}};", written.join("\n"));
        edits.push(written_at_end(file, Section::Implementation, &declaration));
        return Some(edits);
    };
    for (representation, text) in moved {
        let names = representation.component_names();
        let into = part
            .representations()
            .find(|r| r.component_names() == names)
            .filter(|r| r.conversions().next().is_none())?;
        let end = usize::from(into.components()?.syntax().text_range().end());
        edits.push(Edit {
            span: Span::empty(end),
            replacement: text,
        });
    }
    Some(edits)
}

/// The components of `representation`, as its braces write them, without
/// its own constraints: `{ a: integer, b: text }`.
fn components_written(representation: &ast::Representation) -> Option<String> {
    let heading = representation.components()?;
    let attributes: Vec<String> = heading
        .attributes()
        .map(|a| a.syntax().text().to_string())
        .collect();
    Some(if attributes.is_empty() {
        "{}".to_string()
    } else {
        format!("{{ {} }}", attributes.join(", "))
    })
}

/// The components of `representation`, as [`components_written`] writes
/// them, without the spaces between their tokens, so that two written
/// alike but for their layout compare equal.
fn components_compared(representation: &ast::Representation) -> Option<String> {
    let heading = representation.components()?;
    let mut attributes: Vec<String> = heading
        .attributes()
        .map(|a| a.syntax().text().to_string().split_whitespace().collect())
        .collect();
    attributes.sort();
    Some(attributes.join(","))
}

/// Report what `part`, the declaration in the implementation of the type
/// `decl` declares in the interface, says beside its possible
/// representations' components and conversions, with fixes moving it to
/// `decl` (A0067), and each representation it doesn't repeat as the
/// interface writes it (A0068).
fn part_checked(decl: &ast::TypeDecl, part: &ast::TypeDecl, errors: &mut Vec<Diagnostic>) {
    let name = decl
        .name()
        .and_then(|n| n.ident())
        .map(|i| i.text().to_string())
        .unwrap_or_default();
    // Where an item moved into the interface's braces goes: after its last.
    let last_item = decl
        .body()
        .and_then(|body| body.syntax().children().last())
        .map(|item| usize::from(item.text_range().end()));
    if let Some(body) = part.body() {
        for item in body.syntax().children() {
            let removal = if let Some(constraint) = ast::Constraint::cast(item.clone()) {
                constraint.removal()
            } else if let Some(order) = ast::TypeOrder::cast(item.clone()) {
                order.removal()
            } else {
                continue;
            };
            let moved = last_item.map(|end| errors::moved_after(removal, end, &item));
            errors.push(errors::implementation_type_item(&name, &item, moved));
        }
    }
    let interface: Vec<ast::Representation> = decl.representations().collect();
    for representation in part.representations() {
        let names = representation.component_names();
        match interface.iter().find(|r| r.component_names() == names) {
            Some(declared) => representation_checked(&name, &representation, declared, errors),
            None => errors.push(errors::undeclared_representation(
                &name,
                &representation,
                &interface,
            )),
        }
    }
    let repeated: Vec<Vec<String>> = part
        .representations()
        .map(|r| r.component_names())
        .collect();
    let missing: Vec<&ast::Representation> = interface
        .iter()
        .filter(|r| !repeated.contains(&r.component_names()))
        .collect();
    if !missing.is_empty() {
        errors.push(errors::unrepeated_representations(&name, part, &missing));
    }
}

/// Report what `representation`, in the implementation's declaration of the
/// type `name`, says beside its components, with fixes moving its own
/// constraints, which the type's users see, to `declared`, the same
/// components in the interface's (A0067), and its components, if they
/// aren't the interface's, with a fix writing those (A0068).
fn representation_checked(
    name: &str,
    representation: &ast::Representation,
    declared: &ast::Representation,
    errors: &mut Vec<Diagnostic>,
) {
    for constraint in representation.constraints() {
        let moved = declared
            .components()
            .and_then(|h| h.syntax().children().last())
            .map(|last| {
                let end = usize::from(last.text_range().end());
                errors::moved_after(constraint.removal(), end, constraint.syntax())
            });
        errors.push(errors::implementation_type_item(
            name,
            constraint.syntax(),
            moved,
        ));
    }
    if components_compared(representation) != components_compared(declared) {
        let written = representation.components().and_then(|heading| {
            let first = heading.attributes().next()?;
            let last = heading.attributes().last()?;
            let span = Span::new(
                usize::from(first.syntax().text_range().start()),
                usize::from(last.syntax().text_range().end()),
            );
            let interface = declared.components()?;
            let texts: Vec<String> = interface
                .attributes()
                .map(|a| a.syntax().text().to_string())
                .collect();
            Some(Edit {
                span,
                replacement: texts.join(", "),
            })
        });
        errors.push(errors::representation_mismatch(
            name,
            representation,
            declared,
            written,
        ));
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

/// The edit that removes the unit named `name` from `uses`, of `file`, or
/// `uses` itself, if it names no other.
pub(crate) fn entry_removal(file: &SourceFile, uses: &ast::UsesDecl, name: &ast::UnitName) -> Edit {
    let units: Vec<ast::UsedUnit> = uses.units().collect();
    let Some(i) = units
        .iter()
        .position(|u| u.name().is_some_and(|n| n.syntax() == name.syntax()))
    else {
        return removal(file, uses.syntax());
    };
    let start = |u: &ast::UsedUnit| usize::from(u.syntax().text_range().start());
    let span = match (units.get(i + 1), i.checked_sub(1).map(|p| &units[p])) {
        _ if units.len() == 1 => return removal(file, uses.syntax()),
        // Up to the next unit, the `,` and the space after it with it.
        (Some(next), _) => Span::new(start(&units[i]), start(next)),
        // From the end of the one before, its `,` with it.
        (None, Some(before)) => Span::new(
            usize::from(before.syntax().text_range().end()),
            usize::from(units[i].syntax().text_range().end()),
        ),
        (None, None) => return removal(file, uses.syntax()),
    };
    Edit {
        span,
        replacement: String::new(),
    }
}

/// The edit that names the unit `unit` in the `uses` of `file`'s
/// `section`: after its other units, or, if it has none, in a new one at
/// the section's start, after its header, or before the first declaration
/// of an implementation without one; none for an interface without one,
/// which has nothing written in it.
pub(crate) fn uses_added(file: &SourceFile, section: Section, unit: &str) -> Option<Edit> {
    let uses = file.uses().find(|u| u.section() == section);
    if let Some(last) = uses.as_ref().and_then(|u| u.units().last()) {
        return Some(Edit {
            span: Span::empty(usize::from(last.syntax().text_range().end())),
            replacement: format!(", {unit}"),
        });
    }
    let added = format!("uses {{ {unit} }};\n\n");
    let at = match file.headers().find(|h| h.section() == section) {
        Some(header) => next_start(file, header.syntax()),
        None if section == Section::Interface => return None,
        // An implementation without a header is what comes before the
        // first header, or the whole file.
        None => file
            .syntax()
            .children()
            .take_while(|node| node.kind() != SyntaxKind::SectionHeader)
            .find(|node| Item::cast(node.clone()).is_some())
            .map_or_else(|| file.span().start, |node| attached_start(&node)),
    };
    Some(Edit {
        span: Span::empty(at),
        replacement: added,
    })
}

/// The edits that move the unit named `unit`, of `file`, from its
/// implementation's `uses` to its interface's (see [`uses_added`]).
pub(crate) fn into_interface_uses(file: &SourceFile, unit: &str) -> Vec<Edit> {
    let Some(added) = uses_added(file, Section::Interface, unit) else {
        return Vec::new();
    };
    let mut edits = vec![added];
    for uses in file
        .uses()
        .filter(|u| u.section() == Section::Implementation)
    {
        for used in uses.units() {
            if let Some(name) = used.name().filter(|n| n.text() == unit) {
                edits.push(entry_removal(file, &uses, &name));
            }
        }
    }
    edits
}

/// The names of types and headings `item`, if it is in its unit's
/// interface, mentions where other units see them, unqualified, as a
/// qualified one is another unit's, each with where: an
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
        Item::Relvar(decl) | Item::Constant(decl)
            if ast::section_of(decl.syntax()) == Section::Interface =>
        {
            decl.type_expr()
                .map(|t| t.syntax().clone())
                .into_iter()
                .collect()
        }
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
            // A qualified name is another unit's.
            let (qualifier, ident) = ast::type_reference(&node)?;
            if qualifier.is_some() {
                return None;
            }
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
