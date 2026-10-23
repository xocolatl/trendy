//! The order tokens are printed in: the tree's, except that the attributes of
//! a relation's tuples are written in the order of the relation's heading.
//! Attributes are unordered, so this never changes what a program means.

use trendy_syntax::ast::{
    AstNode, AttributeValue, Expr, Heading, Operator, RelationLiteral, SourceFile, TupleLiteral,
    TypeExpr, VarDecl,
};
use trendy_syntax::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};

/// Every token of `file`, whitespace included, in the order to print them.
pub(crate) fn tokens(file: &SourceFile) -> Vec<SyntaxToken> {
    let mut out = Vec::new();
    collect(file.syntax(), file, &mut out);
    out
}

fn collect(node: &SyntaxNode, file: &SourceFile, out: &mut Vec<SyntaxToken>) {
    let mut children: Vec<SyntaxElement> = node.children_with_tokens().collect();
    if let Some(order) = TupleLiteral::cast(node.clone())
        .filter(|t| {
            t.syntax()
                .parent()
                .is_some_and(|p| p.kind() == SyntaxKind::RelationLiteral)
        })
        .and_then(|t| relation_order(&RelationLiteral::cast(t.syntax().parent()?)?, file))
    {
        reorder(&mut children, &order);
    }
    for child in children {
        match child {
            SyntaxElement::Token(token) => out.push(token),
            SyntaxElement::Node(node) => collect(&node, file, out),
        }
    }
}

/// Put the attribute nodes among `children` in `order`, keeping every other
/// element, commas included, where it is. Attributes `order` doesn't name
/// keep their relative order, after the named ones.
fn reorder(children: &mut [SyntaxElement], order: &[String]) {
    let slots: Vec<usize> = children
        .iter()
        .enumerate()
        .filter(|(_, c)| c.kind() == SyntaxKind::AttributeValue)
        .map(|(i, _)| i)
        .collect();
    let mut attributes: Vec<SyntaxElement> = slots.iter().map(|&i| children[i].clone()).collect();
    let rank = |element: &SyntaxElement| {
        let name = element
            .as_node()
            .and_then(|n| AttributeValue::cast(n.clone()))
            .and_then(|a| Some(a.name()?.ident()?.text().to_string()));
        name.and_then(|n| order.iter().position(|o| *o == n))
            .unwrap_or(order.len())
    };
    attributes.sort_by_key(rank);
    for (slot, attribute) in slots.into_iter().zip(attributes) {
        children[slot] = attribute;
    }
}

/// The attribute order of `relation`'s tuples: its heading's if there is
/// one, else its first tuple selector's.
fn relation_order(relation: &RelationLiteral, file: &SourceFile) -> Option<Vec<String>> {
    if let Some(heading) = relation_heading(relation, file) {
        return Some(names(&heading));
    }
    let first = relation.elements().find_map(|element| match element {
        Expr::Tuple(tuple) => Some(tuple),
        _ => None,
    })?;
    Some(
        first
            .attributes()
            .filter_map(|a| Some(a.name()?.ident()?.text().to_string()))
            .collect(),
    )
}

/// `relation`'s own heading, else the heading of the type expected of it,
/// where its attributes are written: in a declared heading's declaration,
/// for one named.
fn relation_heading(relation: &RelationLiteral, file: &SourceFile) -> Option<Heading> {
    relation
        .heading()
        .or_else(|| match expected_type(relation.syntax(), file)? {
            TypeExpr::Relation(r) => r.heading(),
            _ => None,
        })?
        .attributes_written()
}

fn names(heading: &Heading) -> Vec<String> {
    heading
        .attributes()
        .filter_map(|a| Some(a.name()?.ident()?.text().to_string()))
        .collect()
}

/// The type written for the value `expr`, where the program says what it must
/// be: a variable's declared type, a parameter's type for a call's argument,
/// or the type of the attribute it is the value of.
fn expected_type(expr: &SyntaxNode, file: &SourceFile) -> Option<TypeExpr> {
    let parent = expr.parent()?;
    match parent.kind() {
        SyntaxKind::Initializer => VarDecl::cast(parent.parent()?)?.type_expr(),
        SyntaxKind::AttributeValue => {
            let attribute = AttributeValue::cast(parent.clone())?;
            let name = attribute.name()?.ident()?.text().to_string();
            let owner = parent.parent()?;
            let heading = match owner.kind() {
                SyntaxKind::TupleLiteral => tuple_heading(&TupleLiteral::cast(owner)?, file)?,
                SyntaxKind::Arguments => {
                    let call = owner.parent()?;
                    let callee = call
                        .children()
                        .find(|n| n.kind() == SyntaxKind::NameRef)?
                        .first_token()?;
                    file.syntax()
                        .children()
                        .filter_map(Operator::cast)
                        .find(|op| {
                            op.name()
                                .and_then(|n| n.ident())
                                .is_some_and(|i| i.text() == callee.text())
                        })?
                        .params()?
                }
                _ => return None,
            };
            heading
                .attributes()
                .find(|a| {
                    a.name()
                        .and_then(|n| n.ident())
                        .is_some_and(|i| i.text() == name)
                })?
                .type_expr()
        }
        _ => None,
    }
}

/// The heading a tuple value must have, if the program writes one, where
/// its attributes are written.
fn tuple_heading(tuple: &TupleLiteral, file: &SourceFile) -> Option<Heading> {
    if let Some(heading) = tuple.heading() {
        return heading.attributes_written();
    }
    let parent = tuple.syntax().parent()?;
    if let Some(relation) = RelationLiteral::cast(parent) {
        return relation_heading(&relation, file);
    }
    match expected_type(tuple.syntax(), file)? {
        TypeExpr::Tuple(t) => t.heading()?.attributes_written(),
        _ => None,
    }
}
