//! Headings declared with names: each one's attributes resolved once, after
//! those of the headings it names, which can be declared in any order. A
//! heading that contains itself, directly or through others, is reported.

use std::collections::{HashMap, HashSet};

use trendy_diag::{Diagnostic, Span};
use trendy_syntax::ast::{AstNode, HeadingDecl, HeadingRef, SourceFile};

use crate::Namespaces;
use crate::errors;
use crate::types::{self, TypeParameters};

/// A heading's declaration, and the headings it names, each with where.
struct Declared {
    decl: HeadingDecl,
    names: Vec<(String, HeadingRef)>,
}

/// A heading in a cycle: its name, where it is declared, and where it names
/// the next one, which the last names the first of.
pub(crate) struct Cycled {
    pub(crate) name: String,
    pub(crate) declared: Span,
    pub(crate) via: Span,
}

/// Resolve the attributes of each heading `file` declares into
/// `namespaces`, reporting each cycle among them, and every error in their
/// attributes.
pub(crate) fn resolve(
    file: &SourceFile,
    namespaces: &mut Namespaces,
    errors: &mut Vec<Diagnostic>,
) {
    let declared: Vec<(String, Declared)> = file
        .headings()
        .filter_map(|decl| {
            let ident = decl.name()?.ident()?;
            if !namespaces.is_first_heading(&ident) {
                return None;
            }
            let names = decl
                .heading()
                .into_iter()
                .flat_map(|h| h.syntax().descendants().filter_map(HeadingRef::cast))
                .filter_map(|r| Some((r.ident()?.text().to_string(), r)))
                .filter(|(name, _)| namespaces.heading(name).is_some())
                .collect();
            Some((ident.text().to_string(), Declared { decl, names }))
        })
        .collect();
    let by_name: HashMap<&str, &Declared> = declared.iter().map(|(n, d)| (n.as_str(), d)).collect();

    // Each heading after those it names, and the cycles found on the way.
    let mut order = Search::default();
    for (name, _) in &declared {
        order.visit(name, &by_name, namespaces, errors);
    }
    for name in &order.finished {
        let Some(heading) = by_name[name.as_str()].decl.heading() else {
            continue;
        };
        // A heading in a cycle has no attributes, but its others' errors
        // are reported all the same.
        let resolved =
            types::resolve_heading(&heading, &mut TypeParameters::None, namespaces, errors);
        if let Some(resolved) = resolved.filter(|_| !order.cyclic.contains(name)) {
            namespaces.attributes.insert(name.clone(), resolved);
        }
    }
}

/// A depth-first search of the headings, through the ones each names.
#[derive(Default)]
struct Search {
    /// The headings being visited, each with where it names the next.
    path: Vec<(String, Option<Span>)>,
    /// The headings visited, each after those it names.
    finished: Vec<String>,
    /// The headings in a cycle.
    cyclic: HashSet<String>,
}

impl Search {
    fn visit(
        &mut self,
        name: &str,
        by_name: &HashMap<&str, &Declared>,
        namespaces: &Namespaces,
        errors: &mut Vec<Diagnostic>,
    ) {
        if self.finished.iter().any(|f| f == name) {
            return;
        }
        if let Some(start) = self.path.iter().position(|(n, _)| n == name) {
            let cycle: Vec<Cycled> = self.path[start..]
                .iter()
                .map(|(name, via)| Cycled {
                    name: name.clone(),
                    declared: namespaces.heading(name).expect("a declared heading").0,
                    via: via.expect("a heading in the path names the next"),
                })
                .collect();
            self.cyclic.extend(cycle.iter().map(|c| c.name.clone()));
            errors.push(errors::cyclic_headings(&cycle));
            return;
        }
        self.path.push((name.to_string(), None));
        for (next, named) in &by_name[name].names {
            self.path.last_mut().expect("just pushed").1 = Some(named.span());
            self.visit(next, by_name, namespaces, errors);
        }
        self.path.pop();
        self.finished.push(name.to_string());
    }
}
