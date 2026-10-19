//! The canonical source formatter.
//!
//! The formatter reprints the CST's tokens and decides the whitespace between
//! them. It keeps every comment, placed by where it was: on its own line,
//! after code, or within a line. The only tokens it adds or drops are the
//! commas after lists' last items, which a list laid out one item per line
//! has and an inline one doesn't, and the only ones it moves are the
//! attributes of a relation's tuples, which it writes in the order of the
//! relation's heading; attributes are unordered, so neither ever changes
//! what a program means. This crate defines the language's layout: whatever
//! it prints is the style.

mod order;

use std::collections::HashSet;

use trendy_syntax::ast::{
    Arguments, ArrayLiteral, AssignBlock, AssignList, AstNode, Body, Clause, GatherList, Heading,
    IfExpr, NameList, NaryExpr, OrderList, RelationLiteral, RenameList, SourceFile, TupleLiteral,
};
use trendy_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

/// Render a file in canonical style.
///
/// Callers should only format trees without syntax errors: an erroneous
/// tree keeps all its tokens, but their layout is not meaningful. The
/// output ends with exactly one newline; an empty file formats as "".
#[must_use]
pub fn format(file: &SourceFile) -> String {
    // Lay everything out flat but for the blocks the structure demands, then
    // break the outermost list, or a variable's keys, on the first line
    // that's too long, until every line fits or nothing more can be broken.
    let mut layout = Layout::default();
    loop {
        let (out, lines) = render(file, &layout);
        match layout.too_long(&out, &lines) {
            Some(Break::Node(node)) => {
                layout.broken.insert(node);
            }
            Some(Break::Keys(decl)) => {
                layout.broken_keys.insert(decl);
            }
            None => return out,
        }
    }
}

/// What to break across lines next.
enum Break {
    /// A list, one item per line, or an `if`, with its `then` and its
    /// `else` each starting a line.
    Node(SyntaxNode),
    /// The keys of a variable declaration, each on a line of its own.
    Keys(SyntaxNode),
}

/// The widest a line may be, in characters, before the formatter breaks a
/// list or an `if` in it across lines.
const MAX_WIDTH: usize = 100;

/// Which lists and `if`s, and which variable declarations' keys, are broken
/// across lines beyond those the structure demands.
#[derive(Default)]
struct Layout {
    broken: HashSet<SyntaxNode>,
    broken_keys: HashSet<SyntaxNode>,
}

impl Layout {
    /// The outermost list or keys that can still be broken on the first
    /// line of `out` that is too long, given `lines`, the line each
    /// significant token was written on.
    fn too_long(&self, out: &str, lines: &[(SyntaxToken, usize)]) -> Option<Break> {
        for (line, text) in out.lines().enumerate() {
            if text.chars().count() <= MAX_WIDTH {
                continue;
            }
            // A variable's keys are the outermost thing that can break.
            let keys = lines
                .iter()
                .filter(|(_, l)| *l == line)
                .filter_map(|(token, _)| key_start(token))
                .filter_map(|key| key.parent())
                .find(|decl| !self.keys_broken(decl));
            if let Some(decl) = keys {
                return Some(Break::Keys(decl));
            }
            // The outermost list or `if` that starts on the line and isn't
            // broken yet, and of those the first: in `r{a, b} extend {…}`,
            // the `extend`'s list, which the projection is within.
            let breakable = lines
                .iter()
                .filter(|(_, l)| *l == line)
                .filter_map(|(token, _)| {
                    let node = token.parent()?;
                    let opens = match token.kind() {
                        SyntaxKind::LBrace | SyntaxKind::LBracket => {
                            is_list(&node) && items(&node) > 0
                        }
                        SyntaxKind::IfKw => node.kind() == SyntaxKind::IfExpr,
                        _ => false,
                    };
                    (opens && !self.is_block(&node) && !self.chained_if(&node)).then_some(node)
                })
                .min_by_key(|node| node.ancestors().count());
            if let Some(node) = breakable {
                return Some(Break::Node(node));
            }
        }
        None
    }

    /// Whether a node is laid out one item per line, indented: a body with
    /// statements, a relation value with more than one tuple, a list broken
    /// because it was too long, and a list with several items, one of them
    /// laid out across lines. So a value broken across lines is never
    /// followed by more items on its last line; a single item stays hugged by
    /// its braces, as in `call f{v: relation {` … `}};`.
    fn is_block(&self, node: &SyntaxNode) -> bool {
        self.own_block(node)
            || (is_list(node)
                && items(node) > 1
                && node.descendants().skip(1).any(|n| self.own_block(&n)))
    }

    /// Whether `node` is a block in its own right, not because of what's in
    /// it.
    fn own_block(&self, node: &SyntaxNode) -> bool {
        match node.kind() {
            // An `else if` chained to a broken `if` is laid out with it, at
            // its level.
            SyntaxKind::IfExpr => self.broken_if(node) && !self.chained_if(node),
            SyntaxKind::Body => Body::cast(node.clone())
                .is_some_and(|b| b.statements().next().is_some() || b.value().is_some()),
            SyntaxKind::ValueBlock => true,
            SyntaxKind::AssignBlock => {
                AssignBlock::cast(node.clone()).is_some_and(|b| b.statements().next().is_some())
            }
            _ => is_multi_tuple_relation(node) || self.broken.contains(node),
        }
    }

    /// Whether the keys of the variable declared by `decl` go on lines of
    /// their own: when what comes before them spans lines, or when they
    /// don't fit on its last line.
    fn keys_broken(&self, decl: &SyntaxNode) -> bool {
        self.broken_keys.contains(decl)
            || decl
                .children()
                .filter(|n| n.kind() != SyntaxKind::Key)
                .any(|n| n.descendants().any(|d| self.is_block(&d)))
    }

    /// Whether `node` is an `if` whose `then` and `else` each start a line:
    /// one broken because it was too long, one chained to such an `if`, or
    /// one with something in it laid out across lines, so that no value
    /// across lines is followed by more on its last line.
    fn broken_if(&self, node: &SyntaxNode) -> bool {
        node.kind() == SyntaxKind::IfExpr
            && (self.broken.contains(node) || self.chained_if(node) || self.spans_lines(node))
    }

    /// Whether something in the `if` `node` is laid out across lines, but
    /// for a value block that is a branch, of it or of an `else if` chained
    /// to it, which is laid out as an `if` statement's branch is: `then [`,
    /// then `] else [`.
    fn spans_lines(&self, node: &SyntaxNode) -> bool {
        node.children().any(|child| match child.kind() {
            SyntaxKind::ValueBlock => false,
            SyntaxKind::IfExpr => self.broken.contains(&child) || self.spans_lines(&child),
            _ => child.descendants().any(|n| {
                if n.kind() == SyntaxKind::IfExpr {
                    self.broken.contains(&n)
                } else {
                    self.own_block(&n)
                }
            }),
        })
    }

    /// Whether `node` is an `if` in the `else` branch of a broken `if`,
    /// which is broken with it, as one chain: `else if`.
    fn chained_if(&self, node: &SyntaxNode) -> bool {
        node.kind() == SyntaxKind::IfExpr
            && node.parent().is_some_and(|outer| {
                IfExpr::cast(outer.clone())
                    .and_then(|outer| outer.else_branch())
                    .is_some_and(|branch| branch.syntax() == node)
                    && self.broken_if(&outer)
            })
    }

    /// The `if` that a broken `if`, `node`, is chained to, or itself.
    fn chain_start(&self, node: &SyntaxNode) -> SyntaxNode {
        let mut start = node.clone();
        while self.chained_if(&start) {
            start = start.parent().expect("a chained `if` is in another");
        }
        start
    }

    /// How many blocks enclose `node`, not counting itself: the indentation
    /// level of its own lines' start.
    fn level(&self, node: &SyntaxNode) -> usize {
        node.ancestors()
            .skip(1)
            .filter(|n| self.is_block(n))
            .count()
    }
}

/// Whether `node` is a list that can be broken across lines, one item per
/// line: parameters, arguments, headings, tuple and relation values, lists
/// of names, new attributes, the operands of `join{…}` and `times{…}`, and
/// a load's order.
fn is_list(node: &SyntaxNode) -> bool {
    matches!(
        node.kind(),
        SyntaxKind::Heading
            | SyntaxKind::Arguments
            | SyntaxKind::RequirementList
            | SyntaxKind::TupleLiteral
            | SyntaxKind::RelationLiteral
            | SyntaxKind::ArrayLiteral
            | SyntaxKind::NameList
            | SyntaxKind::RenameList
            | SyntaxKind::AssignList
            | SyntaxKind::GatherList
            | SyntaxKind::NaryExpr
            | SyntaxKind::OrderList
    )
}

/// The file laid out by `layout`, and the line each significant token is on.
fn render(file: &SourceFile, layout: &Layout) -> (String, Vec<(SyntaxToken, usize)>) {
    let mut lines = Vec::new();
    let mut line = 0;
    let mut out = String::new();
    let mut prev: Option<SyntaxToken> = None;
    // Since the previous significant token: whether the source has a blank
    // line, and the comments, each with whether it starts its own line.
    let mut blank_line = false;
    let mut comments: Vec<Comment> = Vec::new();
    // Whether the source has a line break, and a blank line, since the last
    // token or comment.
    let mut newline = false;
    let mut blank_since = false;
    let tokens = order::tokens(file);
    for (i, token) in tokens.iter().cloned().enumerate() {
        match token.kind() {
            SyntaxKind::Whitespace => {
                let breaks = token.text().matches('\n').count();
                blank_line |= breaks >= 2;
                newline |= breaks >= 1;
                blank_since |= breaks >= 2;
                continue;
            }
            SyntaxKind::Comment => {
                comments.push(Comment {
                    own_line: newline || prev.is_none(),
                    blank_before: blank_since && !comments.is_empty(),
                    token,
                });
                newline = false;
                blank_since = false;
                continue;
            }
            _ => {}
        }
        // The comma after a list's last item goes with its layout: an
        // inline list has none, and one laid out one item per line has one,
        // so that adding an item changes no other line.
        let next = tokens[i + 1..].iter().find(|t| !t.kind().is_trivia());
        if trailing_comma(&token, next).is_some_and(|list| !layout.is_block(&list)) {
            continue;
        }
        let sep = match &prev {
            Some(prev) => between(prev, &token, blank_line, layout),
            None => String::new(),
        };
        let gap = Gap {
            sep: &sep,
            prev: prev.as_ref(),
            next: &token,
            blank_after: blank_since,
        };
        let before = out.len();
        if prev
            .as_ref()
            .is_some_and(|prev| needs_trailing_comma(prev, &token, layout))
        {
            out.push(',');
        }
        gap.write(&std::mem::take(&mut comments), &mut out);
        line += out[before..].matches('\n').count();
        lines.push((token.clone(), line));
        out.push_str(token.text());
        prev = Some(token);
        blank_line = false;
        newline = false;
        blank_since = false;
    }
    // Comments after the last token.
    for comment in comments {
        if !out.is_empty() {
            if comment.own_line {
                out.push('\n');
                if comment.blank_before {
                    out.push('\n');
                }
            } else {
                out.push(' ');
            }
        }
        out.push_str(comment.token.text());
    }
    if !out.is_empty() {
        out.push('\n');
    }
    (out, lines)
}

/// The list `comma` ends the last item of, if it is the comma after a
/// list's last item: `next`, the significant token after it, is the
/// list's closing delimiter.
fn trailing_comma(comma: &SyntaxToken, next: Option<&SyntaxToken>) -> Option<SyntaxNode> {
    let list = comma.parent().filter(is_list)?;
    let closes = next.is_some_and(|next| {
        matches!(next.kind(), SyntaxKind::RBrace | SyntaxKind::RBracket)
            && next.parent().as_ref() == Some(&list)
    });
    (comma.kind() == SyntaxKind::Comma && closes).then_some(list)
}

/// Whether a comma goes between `prev` and `closer`: `closer` closes a
/// list laid out one item per line, and `prev`, ending its last item, is
/// no comma.
fn needs_trailing_comma(prev: &SyntaxToken, closer: &SyntaxToken, layout: &Layout) -> bool {
    matches!(closer.kind(), SyntaxKind::RBrace | SyntaxKind::RBracket)
        && prev.kind() != SyntaxKind::Comma
        && closer
            .parent()
            .is_some_and(|list| is_list(&list) && items(&list) > 0 && layout.is_block(&list))
}

/// The whitespace between two adjacent significant tokens.
fn between(prev: &SyntaxToken, next: &SyntaxToken, blank_line: bool, layout: &Layout) -> String {
    if let Some(indent) = broken_line(prev, next, layout) {
        // Statements in a row go on adjacent lines, unless the source groups
        // them with a blank line, which is kept; several become one.
        let statement_end = prev.kind() == SyntaxKind::Semicolon
            && prev.parent().is_some_and(|p| {
                matches!(
                    p.kind(),
                    SyntaxKind::CallStmt
                        | SyntaxKind::AssignStmt
                        | SyntaxKind::AssignBlock
                        | SyntaxKind::ReturnStmt
                        | SyntaxKind::IfStmt
                )
            });
        let blank = blank_line && statement_end && next.kind() != SyntaxKind::RBracket;
        return format!(
            "{}\n{}",
            if blank { "\n" } else { "" },
            INDENT.repeat(indent)
        );
    }
    // Keys on lines of their own end with a blank line, so they don't run
    // into the next definition.
    if prev.kind() == SyntaxKind::Semicolon
        && prev.parent().is_some_and(|decl| {
            decl.kind() == SyntaxKind::VarDecl
                && decl.children().any(|n| n.kind() == SyntaxKind::Key)
                && layout.keys_broken(&decl)
        })
    {
        return "\n\n".to_string();
    }
    let separator = separator(prev, next, blank_line);
    // Never let two tokens run together into others: the formatter must not
    // change what the program means.
    if separator.is_empty() && would_merge(prev, next) {
        " ".to_string()
    } else {
        separator.to_string()
    }
}

/// A comment in the source, between two significant tokens.
struct Comment {
    token: SyntaxToken,
    /// Whether it starts its own line.
    own_line: bool,
    /// Whether the source has a blank line between it and the comment before
    /// it.
    blank_before: bool,
}

/// What goes between two significant tokens: `sep`, the layout's own
/// whitespace, with any comments in the source between them.
struct Gap<'a> {
    sep: &'a str,
    /// The tokens around the gap; there is none before the start of the
    /// file.
    prev: Option<&'a SyntaxToken>,
    next: &'a SyntaxToken,
    /// Whether the source has a blank line after the last comment.
    blank_after: bool,
}

impl Gap<'_> {
    fn at_start(&self) -> bool {
        self.prev.is_none()
    }

    /// Whether the code before is an opening delimiter, which an inline
    /// comment hugs, as in `f{/* none */}`.
    fn after_opener(&self) -> bool {
        self.prev.is_some_and(|p| {
            matches!(
                p.kind(),
                SyntaxKind::LBrace | SyntaxKind::LBracket | SyntaxKind::LParen
            )
        })
    }

    /// Whether the code after is a closing delimiter, `,` or `;`, which
    /// follows an inline comment directly.
    fn before_closer(&self) -> bool {
        matches!(
            self.next.kind(),
            SyntaxKind::RBrace
                | SyntaxKind::RBracket
                | SyntaxKind::RParen
                | SyntaxKind::Comma
                | SyntaxKind::Semicolon
        )
    }

    /// Write the gap, with `comments`, each with whether it starts its own
    /// line in the source. A comment on its own line keeps it where the
    /// layout breaks the line anyway, indented like the code after it;
    /// anywhere else a comment goes inline, which a block comment always
    /// can.
    fn write(&self, comments: &[Comment], out: &mut String) {
        if comments.is_empty() {
            out.push_str(self.sep);
            return;
        }
        let breaks = self.sep.contains('\n') || self.at_start();
        let indent = self.sep.rsplit('\n').next().unwrap_or("");
        // Between code and a comment beside it: the layout's space, or one
        // where the code would be tight, except against delimiters.
        let spaced = |tight: bool| {
            if breaks || self.sep.is_empty() {
                if tight { "" } else { " " }
            } else {
                self.sep
            }
        };
        let before = spaced(self.after_opener());
        let after = spaced(self.before_closer());
        let mut sep_written = false;
        let mut last_own_line = false;
        for (i, comment) in comments.iter().enumerate() {
            last_own_line = comment.own_line && breaks;
            if last_own_line {
                if sep_written || (self.at_start() && i == 0) {
                    if i > 0 || !self.at_start() {
                        out.push('\n');
                        if comment.blank_before {
                            out.push('\n');
                        }
                        out.push_str(indent);
                    }
                } else {
                    out.push_str(self.sep);
                }
                sep_written = true;
            } else if i > 0 {
                out.push(' ');
            } else if !self.at_start() {
                out.push_str(if breaks { " " } else { before });
            }
            out.push_str(comment.token.text());
        }
        if last_own_line {
            out.push('\n');
            if self.blank_after {
                out.push('\n');
            }
            out.push_str(indent);
        } else if breaks && !self.at_start() {
            if sep_written {
                out.push('\n');
                out.push_str(indent);
            } else {
                out.push_str(self.sep);
            }
        } else if self.at_start() {
            out.push(' ');
        } else {
            out.push_str(after);
        }
    }
}

const INDENT: &str = "    ";

/// The type parameters, `types { … }`, that `token` starts, if it starts
/// them.
fn type_parameters_start(token: &SyntaxToken) -> Option<SyntaxNode> {
    token.parent().filter(|p| {
        p.kind() == SyntaxKind::TypeParameters && p.first_token().as_ref() == Some(token)
    })
}

/// The key that `token` starts, if it starts one.
fn key_start(token: &SyntaxToken) -> Option<SyntaxNode> {
    token
        .parent()
        .filter(|p| p.kind() == SyntaxKind::Key && p.first_token().as_ref() == Some(token))
}

fn is_multi_tuple_relation(node: &SyntaxNode) -> bool {
    RelationLiteral::cast(node.clone()).is_some_and(|r| r.elements().nth(1).is_some())
}

/// How many items a list has.
fn items(node: &SyntaxNode) -> usize {
    if let Some(relation) = RelationLiteral::cast(node.clone()) {
        relation.elements().count()
    } else if let Some(array) = ArrayLiteral::cast(node.clone()) {
        array.elements().count()
    } else if let Some(tuple) = TupleLiteral::cast(node.clone()) {
        tuple.attributes().count()
    } else if let Some(args) = Arguments::cast(node.clone()) {
        args.named().count() + args.positional().count()
    } else if let Some(heading) = Heading::cast(node.clone()) {
        heading.attributes().count()
    } else if let Some(names) = NameList::cast(node.clone()) {
        names.names().count()
    } else if node.kind() == SyntaxKind::RequirementList {
        node.children().count()
    } else if let Some(renamings) = RenameList::cast(node.clone()) {
        renamings.renamings().count()
    } else if let Some(assignments) = AssignList::cast(node.clone()) {
        assignments.assignments().count()
    } else if let Some(gatherings) = GatherList::cast(node.clone()) {
        gatherings.gatherings().count()
    } else if let Some(nary) = NaryExpr::cast(node.clone()) {
        nary.operands().count()
    } else if let Some(order) = OrderList::cast(node.clone()) {
        order.items().count()
    } else {
        0
    }
}

/// If a line break goes between `prev` and `next`, the indentation level of
/// the new line. In a block, each item starts its own line, one level in,
/// and the closing delimiter goes on a line of its own.
fn broken_line(prev: &SyntaxToken, next: &SyntaxToken, layout: &Layout) -> Option<usize> {
    let block = |token: &SyntaxToken| token.parent().filter(|p| layout.is_block(p));
    if matches!(next.kind(), SyntaxKind::RBrace | SyntaxKind::RBracket)
        && let Some(block) = block(next)
    {
        return Some(layout.level(&block));
    }
    // A variable's keys, when they don't stay on the line before, each on a
    // line of its own, in the column of the declaration's start, and so of a
    // relation value's closing brace.
    if let Some(key) = key_start(next)
        && key.parent().is_some_and(|decl| layout.keys_broken(&decl))
    {
        return Some(layout.level(&key));
    }
    // An operator's type parameters, on a line of their own, one level in,
    // and then its body's `[`, in the operator's column.
    if let Some(types) = type_parameters_start(next) {
        return Some(layout.level(&types) + 1);
    }
    // A broken `if`'s `then` and `else`, each starting a line one level
    // in from the `if`, or from the one it is chained to.
    if matches!(next.kind(), SyntaxKind::ThenKw | SyntaxKind::ElseKw)
        && let Some(if_expr) = next.parent().filter(|p| layout.broken_if(p))
    {
        return Some(layout.level(&layout.chain_start(&if_expr)) + 1);
    }
    if next.kind() == SyntaxKind::LBracket
        && let Some(body) = next.parent().filter(|p| p.kind() == SyntaxKind::Body)
        && body.parent().is_some_and(|op| {
            op.children()
                .any(|n| n.kind() == SyntaxKind::TypeParameters)
        })
    {
        return Some(layout.level(&body));
    }
    match prev.kind() {
        SyntaxKind::LBrace | SyntaxKind::LBracket | SyntaxKind::Comma => {
            block(prev).map(|b| layout.level(&b) + 1)
        }
        // A statement's `;`, inside a body.
        SyntaxKind::Semicolon => {
            let body = prev.parent()?.parent().filter(|p| layout.is_block(p))?;
            Some(layout.level(&body) + 1)
        }
        _ => None,
    }
}

/// The whitespace that goes between two adjacent tokens. `blank_line` says
/// whether the source has a blank line between them.
fn separator(prev: &SyntaxToken, next: &SyntaxToken, blank_line: bool) -> &'static str {
    use SyntaxKind::{
        BuiltinType, Heading, Ident, LBrace, LBracket, Minus, Plus, RBrace, RelationLiteral,
        Semicolon, TupleLiteral, VarDecl,
    };
    if prev.kind() == Semicolon {
        // Between definitions: variable declarations in a row go on adjacent
        // lines unless the source groups them with a blank line, and
        // everything else is separated by a blank line.
        let both_variables = item_kind(prev) == Some(VarDecl) && item_kind(next) == Some(VarDecl);
        let both_builtin_types =
            item_kind(prev) == Some(BuiltinType) && item_kind(next) == Some(BuiltinType);
        let both_kinds = item_kind(prev) == Some(SyntaxKind::KindDecl)
            && item_kind(next) == Some(SyntaxKind::KindDecl);
        return if (both_variables || both_builtin_types || both_kinds) && !blank_line {
            "\n"
        } else {
            "\n\n"
        };
    }
    // Braces with spaces inside: a heading, a tuple, a relation or new
    // attributes, as in `tuple { a: integer }` and `r extend { b := 1 }`.
    // Parameters and arguments are written without, as in
    // `greet{name: text}`, and so is an operator's empty `{}`.
    let list_brace = |token: &SyntaxToken| {
        token.parent().is_some_and(|p| match p.kind() {
            Heading => p.parent().is_none_or(|owner| {
                !matches!(
                    owner.kind(),
                    SyntaxKind::Operator | SyntaxKind::BuiltinOperator
                )
            }),
            TupleLiteral
            | RelationLiteral
            | SyntaxKind::AssignList
            | SyntaxKind::GatherList
            | SyntaxKind::RequirementList => true,
            // A key's attributes, as in `key { a, b }`, and a type's kinds.
            SyntaxKind::NameList => p
                .parent()
                .is_some_and(|k| matches!(k.kind(), SyntaxKind::Key | BuiltinType)),
            _ => false,
        })
    };
    // The contextual keywords: `type`, in `builtin type name;`, `key`,
    // `value` and `from`, in `tuple from r` and `value from r`, `any`, in
    // `any type`, `types`, before an operator's type parameters, `per`,
    // which a space also comes before, `load`, `from` and `order`, which
    // one comes before too, an order item's `asc` and `desc`, and a `for`
    // loop's `in`.
    let contextual_keyword = prev.kind() == Ident
        && prev.parent().is_some_and(|p| match p.kind() {
            BuiltinType => prev.text() == "type",
            SyntaxKind::Key
            | SyntaxKind::FromExpr
            | SyntaxKind::AnyType
            | SyntaxKind::TypeParameters
            | SyntaxKind::Per
            | SyntaxKind::KindDecl
            | SyntaxKind::Load
            | SyntaxKind::OrderItem
            | SyntaxKind::ForStmt => true,
            _ => false,
        });
    let load_keyword =
        next.kind() == Ident && next.parent().is_some_and(|p| p.kind() == SyntaxKind::Load);
    // A kind's requirements follow its name after a space.
    let requirements = next.kind() == LBrace
        && next
            .parent()
            .is_some_and(|p| p.kind() == SyntaxKind::RequirementList);
    let per = next
        .parent()
        .is_some_and(|p| p.kind() == SyntaxKind::Per && p.first_token().as_ref() == Some(next));
    // A dyadic operator token, such as the `-` in `a - b`, as opposed to the
    // prefix one in `-b`.
    let dyadic = |token: &SyntaxToken| {
        token.kind().is_operator()
            && token
                .parent()
                .is_some_and(|p| p.kind() == SyntaxKind::BinaryExpr)
    };
    let sign = |token: &SyntaxToken| matches!(token.kind(), Plus | Minus);
    // The word ending `any type`, `any tuple` or `any relation`, which
    // nothing of its own follows.
    let ends_any = prev
        .parent()
        .is_some_and(|p| p.kind() == SyntaxKind::AnyType && p.last_token().as_ref() == Some(prev));
    // A body's `[` follows what comes before it after a space, whether the
    // parameters or a read-only operator's result type: `f{}: integer [`,
    // and so does an array selector's, whether its keyword or its elements'
    // type: `array [1, 2]`, `array text []`, and a load's order's, after
    // `order`. An index's `[` follows its operand directly: `a[1]`.
    let spaced_bracket = next.kind() == LBracket
        && next.parent().is_some_and(|p| {
            matches!(
                p.kind(),
                SyntaxKind::Body | SyntaxKind::ArrayLiteral | SyntaxKind::OrderList
            )
        });
    // A projection's braces follow its operand directly: `r{a}{b}`. Those
    // of a selector it projects don't: `tuple { a: 1 }{a}`.
    let projection = next.kind() == LBrace
        && next.parent().is_some_and(|list| {
            list.kind() == SyntaxKind::NameList
                && list
                    .parent()
                    .is_some_and(|p| p.kind() == SyntaxKind::ProjectExpr)
        });
    match (prev.kind(), next.kind()) {
        _ if key_start(next).is_some() || per || requirements => " ",
        // The variable an assignment assigns, as in `update r { … }`.
        _ if is_target(prev) => " ",
        _ if projection => "",
        _ if spaced_bracket => " ",
        (LBrace, RBrace) if list_brace(prev) => "",
        (LBrace, _) if list_brace(prev) => " ",
        (_, RBrace) if list_brace(next) => " ",
        _ if ends_any => "",
        _ if contextual_keyword || load_keyword => " ",
        _ if dyadic(prev) || dyadic(next) => " ",
        // `- -a`, not `--a`.
        _ if sign(prev) && sign(next) => " ",
        (RBrace, LBracket | LBrace) => " ",
        (prev, next) if spaced_after(prev) || spaced_before(next) => " ",
        _ => "",
    }
}

/// Whether a space always follows a token of this kind: a keyword before
/// a name or an operand, or a separator.
fn spaced_after(kind: SyntaxKind) -> bool {
    use SyntaxKind::{
        ArrayKw, AssignKw, BuiltinKw, CallKw, Colon, Comma, DInsertKw, DeleteKw, IDeleteKw, IfKw,
        InsertKw, NotKw, OperatorKw, RelationKw, ReturnKw, TupleKw, UpdateKw, VarKw,
    };
    spaced_before(kind)
        || matches!(
            kind,
            OperatorKw
                | IfKw
                | VarKw
                | TupleKw
                | RelationKw
                | ArrayKw
                | CallKw
                | BuiltinKw
                | Colon
                | Comma
                | NotKw
                | InsertKw
                | DInsertKw
                | DeleteKw
                | IDeleteKw
                | UpdateKw
                | AssignKw
                | ReturnKw
        )
}

/// Whether a space always goes before, and after, a token of this kind:
/// `:=`, and the postfix operators and other words between two parts, such
/// as a loop's `while` and `loop`.
fn spaced_before(kind: SyntaxKind) -> bool {
    use SyntaxKind::{
        AsKw, ColonEq, ElseKw, ExtendKw, GroupKw, LoopKw, RemoveKw, RenameKw, ReplaceKw, ThenKw,
        UngroupKw, UnwrapKw, WhereKw, WhileKw, WrapKw,
    };
    matches!(
        kind,
        ColonEq
            | ThenKw
            | ElseKw
            | WhileKw
            | LoopKw
            | RemoveKw
            | RenameKw
            | ExtendKw
            | ReplaceKw
            | GroupKw
            | UngroupKw
            | WrapKw
            | UnwrapKw
            | AsKw
            | WhereKw
    )
}

/// Whether `token` ends the variable an assignment assigns: the first
/// expression of an assignment or a shorthand for one.
fn is_target(token: &SyntaxToken) -> bool {
    let Some(name) = token.parent().filter(|p| p.kind() == SyntaxKind::NameRef) else {
        return false;
    };
    name.parent().is_some_and(|clause| {
        Clause::cast(clause)
            .and_then(|c| c.target())
            .is_some_and(|target| target.syntax() == &name)
    })
}

/// Whether `prev` and `next`, written with nothing between them, would lex
/// as some other tokens, as `call` and `f` would run together into `callf`.
fn would_merge(prev: &SyntaxToken, next: &SyntaxToken) -> bool {
    let joined = format!("{}{}", prev.text(), next.text());
    let tokens = trendy_lexer::lex(&joined).tokens;
    !(tokens.len() == 2 && tokens[0].kind == prev.kind() && tokens[1].kind == next.kind())
}

/// The kind of the top-level definition that contains `token`.
fn item_kind(token: &SyntaxToken) -> Option<SyntaxKind> {
    token
        .parent_ancestors()
        .find(|node| {
            node.parent()
                .is_some_and(|p| p.kind() == SyntaxKind::SourceFile)
        })
        .map(|node| node.kind())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(source: &str) -> String {
        let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
        format(&parse.tree)
    }

    /// The source's tokens, excluding whitespace.
    fn significant_tokens(source: &str) -> Vec<String> {
        let lexed = trendy_lexer::lex(source);
        lexed
            .tokens
            .iter()
            .filter(|t| t.kind != SyntaxKind::Whitespace)
            .map(|t| source[t.span.start..t.span.end].to_string())
            .collect()
    }

    #[test]
    fn canonical_is_unchanged() {
        assert_eq!(fmt("operator main{} [];\n"), "operator main{} [];\n");
    }

    #[test]
    fn normalizes_whitespace() {
        assert_eq!(
            fmt("  operator\n\tmain {  }\n[\n] ;"),
            "operator main{} [];\n"
        );
    }

    #[test]
    fn separates_operators_with_a_blank_line() {
        assert_eq!(
            fmt("operator a{}[];operator b{}[];"),
            "operator a{} [];\n\noperator b{} [];\n"
        );
    }

    #[test]
    fn variables() {
        assert_eq!(
            fmt("var   i:integer:=0 ;var t :text;var u:=\"x\";\n\n\noperator main{}[];"),
            "var i: integer := 0;\nvar t: text;\nvar u := \"x\";\n\noperator main{} [];\n"
        );
    }

    #[test]
    fn keys_stay_on_the_line_when_they_fit() {
        assert_eq!(
            fmt("var r:relation{a:integer,b:text}key{a}  key {b,a};var e:=relation{{a:1}}key{ };"),
            "var r: relation { a: integer, b: text } key { a } key { b, a };\nvar e := relation { { a: 1 } } key {};\n"
        );
    }

    #[test]
    fn keys_after_a_value_on_several_lines_go_on_lines_of_their_own() {
        assert_eq!(
            fmt("var r := relation {{a: 1}, {a: 2}} key {a} key {b};var s := 1;"),
            "var r := relation {\n    { a: 1 },\n    { a: 2 },\n}\nkey { a }\nkey { b };\n\nvar s := 1;\n"
        );
    }

    #[test]
    fn keys_that_dont_fit_go_on_lines_of_their_own() {
        let heading: Vec<String> = (0..3).map(|i| format!("attribute_{i}: integer")).collect();
        let source = format!(
            "var r: relation {{ {} }} key {{ attribute_0 }} key {{ attribute_1 }};\nvar s := 1;",
            heading.join(", ")
        );
        assert_eq!(
            fmt(&source),
            format!(
                "var r: relation {{ {} }}\nkey {{ attribute_0 }}\nkey {{ attribute_1 }};\n\nvar s := 1;\n",
                heading.join(", ")
            )
        );
    }

    #[test]
    fn a_long_key_breaks_like_other_lists() {
        let names: Vec<String> = (0..8).map(|i| format!("attribute_{i}")).collect();
        let source = format!("var r := s key {{ {} }};", names.join(", "));
        let items = names
            .iter()
            .map(|n| format!("    {n}"))
            .collect::<Vec<_>>()
            .join(",\n");
        assert_eq!(fmt(&source), format!("var r := s\nkey {{\n{items},\n}};\n"));
    }

    #[test]
    fn blank_line_between_variables_and_operators() {
        assert_eq!(
            fmt("operator a{}[];var i:=1;operator b{}[];"),
            "operator a{} [];\n\nvar i := 1;\n\noperator b{} [];\n"
        );
    }

    #[test]
    fn blank_lines_group_variables() {
        assert_eq!(
            fmt("var a := 1;\n\n\n\nvar b := 2;\nvar c := 3;\n\noperator main{} [];"),
            "var a := 1;\n\nvar b := 2;\nvar c := 3;\n\noperator main{} [];\n"
        );
    }

    #[test]
    fn tuples() {
        assert_eq!(
            fmt(
                "var x:tuple{a:tuple{b:boolean},c:text};var e:tuple{ };var q:=tuple{x:1e0,y:2e0,};operator main{}[];"
            ),
            "var x: tuple { a: tuple { b: boolean }, c: text };\nvar e: tuple {};\nvar q := tuple { x: 1e0, y: 2e0 };\n\noperator main{} [];\n"
        );
    }

    #[test]
    fn relations() {
        assert_eq!(
            fmt(
                "var r:=relation{a:integer}{{a:1},tuple{a:2}};var e:relation{a:integer}:=relation{};var d:=relation{}{};var t:=tuple{a:integer}{a:1};"
            ),
            "var r := relation { a: integer } {\n    { a: 1 },\n    tuple { a: 2 },\n};\nvar e: relation { a: integer } := relation {};\nvar d := relation {} {};\nvar t := tuple { a: integer } { a: 1 };\n"
        );
    }

    #[test]
    fn arrays() {
        // A selector's `[` after a space, an index's right after its value.
        assert_eq!(
            fmt(
                "var a:array   integer:=array[1,2 ,3];var e:=array text[ ];var g:=array[array[1],array integer[]];var x:=a [ 1 ]+g[1][1];operator f{a:any   array}:integer[length{a:a}];"
            ),
            "var a: array integer := array [1, 2, 3];\nvar e := array text [];\nvar g := array [array [1], array integer []];\nvar x := a[1] + g[1][1];\n\noperator f{a: any array}: integer [\n    length{a: a}\n];\n"
        );
        // Too long for one line, one element per line.
        let months = r#"var m := array ["January", "February", "March", "April", "May", "June", "July", "August", "September"];"#;
        assert_eq!(
            fmt(months),
            "var m := array [\n    \"January\",\n    \"February\",\n    \"March\",\n    \"April\",\n    \"May\",\n    \"June\",\n    \"July\",\n    \"August\",\n    \"September\",\n];\n"
        );
    }

    #[test]
    fn loops() {
        assert_eq!(
            fmt("operator f{}[while n>0 loop[n:=n-1;];loop[n:=n+1;]while n<3;];"),
            "operator f{} [\n    while n > 0 loop [\n        n := n - 1;\n    ];\n    loop [\n        n := n + 1;\n    ] while n < 3;\n];\n"
        );
    }

    #[test]
    fn for_loops() {
        assert_eq!(
            fmt("operator f{}[for   x   in a loop[call g{};];];"),
            "operator f{} [\n    for x in a loop [\n        call g{};\n    ];\n];\n"
        );
    }

    #[test]
    fn value_blocks() {
        // Each item on a line of its own, and an `if`'s blocks laid out as
        // an `if` statement's.
        assert_eq!(
            fmt("var x:=[var a:=1;a*2];var y:=if c then[var a:=1;a]else[var b:=2;b];"),
            "var x := [\n    var a := 1;\n    a * 2\n];\nvar y := if c then [\n    var a := 1;\n    a\n] else [\n    var b := 2;\n    b\n];\n"
        );
    }

    #[test]
    fn loads() {
        assert_eq!(
            fmt("operator f{} [ load   a   from r{x}where x>1   order[ asc   x ,desc y];];"),
            "operator f{} [\n    load a from r{x} where x > 1 order [asc x, desc y];\n];\n"
        );
        // Too long for one line, the order's items each on a line of their
        // own, as any list's.
        assert_eq!(
            fmt(
                "operator f{} [ load everyone from people where born > 1900 and name <> \"Ada Lovelace\" order [desc born, asc name]; ];"
            ),
            "operator f{} [\n    load everyone from people where born > 1900 and name <> \"Ada Lovelace\" order [\n        desc born,\n        asc name,\n    ];\n];\n"
        );
    }

    #[test]
    fn ifs() {
        assert_eq!(
            fmt("var x:=if a then\"one\"else(if b then 2 else 3)+1;"),
            "var x := if a then \"one\" else (if b then 2 else 3) + 1;\n"
        );
        // Too long, `then` and `else` each start a line, one level in, and
        // an `else if` chain stays at that level.
        assert_eq!(
            fmt(
                "operator sign{n: integer}: text [ if n < 0 then \"negative\" else if n = 0 then \"zero\" else \"positive, and more words to make it long\" ];"
            ),
            "operator sign{n: integer}: text [\n    if n < 0\n        then \"negative\"\n        else if n = 0\n        then \"zero\"\n        else \"positive, and more words to make it long\"\n];\n"
        );
        // A branch across lines breaks the `if`, so nothing follows it on
        // its last line.
        assert_eq!(
            fmt("var r := if a then relation {{x: 1}, {x: 2}} else relation {{x: 3}};"),
            "var r := if a\n    then relation {\n        { x: 1 },\n        { x: 2 },\n    }\n    else relation { { x: 3 } };\n"
        );
    }

    #[test]
    fn if_statements() {
        // Each statement on a line of its own, even one alone in a block.
        assert_eq!(
            fmt(
                "operator f{n:integer}[if n=0 then[call g{};]else if n<0 then[n:=0;call g{};]else[call h{};];call g{};];"
            ),
            "operator f{n: integer} [\n    if n = 0 then [\n        call g{};\n    ] else if n < 0 then [\n        n := 0;\n        call g{};\n    ] else [\n        call h{};\n    ];\n    call g{};\n];\n"
        );
        // A blank line after one is kept.
        assert_eq!(
            fmt("operator f{} [\n    if a then [ call g{}; ];\n\n    call g{};\n];\n"),
            "operator f{} [\n    if a then [\n        call g{};\n    ];\n\n    call g{};\n];\n"
        );
    }

    #[test]
    fn trailing_commas_follow_the_layout() {
        // Added after the last item of a list laid out one item per line,
        // before a comment there, and dropped from an inline one.
        assert_eq!(
            fmt("var r := relation {\n    { a: 1, },\n    { a: 2 } /* last */\n};"),
            "var r := relation {\n    { a: 1 },\n    { a: 2 }, /* last */\n};\n"
        );
        assert_eq!(
            fmt("operator f{a: integer, b: text,} [ call g{x: 1,}; ];"),
            "operator f{a: integer, b: text} [\n    call g{x: 1};\n];\n"
        );
    }

    #[test]
    fn the_outermost_list_breaks_first() {
        // The `extend`'s list, not the projection within its operand, which
        // comes first.
        assert_eq!(
            fmt(
                "var list := scaled{ingredient, unit} extend { total := sum{r: !!scaled, of: amount}, recipes := (!!scaled){recipe} };"
            ),
            "var list := scaled{ingredient, unit} extend {\n    total := sum{r: !!scaled, of: amount},\n    recipes := (!!scaled){recipe},\n};\n"
        );
    }

    #[test]
    fn relations_with_several_tuples_break_across_lines() {
        assert_eq!(
            fmt("var p:=relation{tuple{n:\"Ada\"},{n:\"Edgar\"},};"),
            "var p := relation {\n    tuple { n: \"Ada\" },\n    { n: \"Edgar\" },\n};\n"
        );
        assert_eq!(
            fmt("var p:=relation{n:text}{{n:\"a\"},{n:\"b\"}};"),
            "var p := relation { n: text } {\n    { n: \"a\" },\n    { n: \"b\" },\n};\n"
        );
        assert_eq!(
            fmt("var q:=relation{{r:relation{{x:1},{x:2}}},{r:relation{}}};"),
            "var q := relation {\n    { r: relation {\n        { x: 1 },\n        { x: 2 },\n    } },\n    { r: relation {} },\n};\n"
        );
    }

    #[test]
    fn operators_and_calls() {
        assert_eq!(
            fmt(
                "operator greet { name : text } [ call debug_value { v : name } ; call debug_value{v:1};];operator main{}[call greet{name:\"Ada\"};];"
            ),
            "operator greet{name: text} [\n    call debug_value{v: name};\n    call debug_value{v: 1};\n];\n\noperator main{} [\n    call greet{name: \"Ada\"};\n];\n"
        );
    }

    #[test]
    fn read_only_operators_and_calls() {
        assert_eq!(
            fmt(
                "operator double { x : integer } : integer [ x*2  ];operator main{}[call debug_value{v:double{x:1}};];"
            ),
            "operator double{x: integer}: integer [\n    x * 2\n];\n\noperator main{} [\n    call debug_value{v: double{x: 1}};\n];\n"
        );
        assert_eq!(
            fmt("operator f{}:tuple{a:integer}[tuple{a:1}];"),
            "operator f{}: tuple { a: integer } [\n    tuple { a: 1 }\n];\n"
        );
    }

    #[test]
    fn kinds() {
        assert_eq!(
            fmt(
                "kind ordered{<};kind other { < , < } ;builtin type integer:{ordered};builtin type text : {ordered,other};"
            ),
            "kind ordered { < };\nkind other { <, < };\n\nbuiltin type integer: { ordered };\nbuiltin type text: { ordered, other };\n"
        );
        assert_eq!(
            fmt("operator f{a:any ordered}[];"),
            "operator f{a: any ordered} [];\n"
        );
    }

    #[test]
    fn type_parameters() {
        // On a line of their own, and then the body's `[`.
        assert_eq!(
            fmt("operator same{a:T,b:T,r:any   relation}:boolean types{T:any type}[a=b];"),
            "operator same{a: T, b: T, r: any relation}: boolean\n    types { T: any type }\n[\n    a = b\n];\n"
        );
        assert_eq!(
            fmt("operator show{t:any tuple}types{}[];"),
            "operator show{t: any tuple}\n    types {}\n[];\n"
        );
        // A built-in's, before its `;`.
        assert_eq!(
            fmt(
                "builtin operator max{r:any relation,of:T per r}:T types{T:any ordered};builtin operator count{r: any relation}: integer;"
            ),
            "builtin operator max{r: any relation, of: T per r}: T\n    types { T: any ordered };\n\nbuiltin operator count{r: any relation}: integer;\n"
        );
        // `per`, between spaces.
        assert_eq!(
            fmt("operator f{r:any relation,of:tuple{a:T}per  r}types{T:any type}[];"),
            "operator f{r: any relation, of: tuple { a: T } per r}\n    types { T: any type }\n[];\n"
        );
        // Without them, the body follows the signature.
        assert_eq!(
            fmt("operator show{t:any tuple}[];"),
            "operator show{t: any tuple} [];\n"
        );
        // A long signature breaks its parameters.
        let long = "operator tally{relation_of_things: any relation, of: T, at_least: integer}: relation { value: T, n: integer, at_least: integer } types { T: any type } [];";
        assert_eq!(
            fmt(long),
            "operator tally{\n    relation_of_things: any relation,\n    of: T,\n    at_least: integer,\n}: relation { value: T, n: integer, at_least: integer }\n    types { T: any type }\n[];\n"
        );
        // A comment before them stays on its line.
        assert_eq!(
            fmt("operator f{a: T}: T /* the same */ types { T: any type } [ a ];"),
            "operator f{a: T}: T /* the same */\n    types { T: any type }\n[\n    a\n];\n"
        );
    }

    #[test]
    fn relations_in_calls_indent_inside_the_body() {
        assert_eq!(
            fmt("operator main{}[call debug_value{v:relation{{a:1},{a:2}}};];"),
            "operator main{} [\n    call debug_value{v: relation {\n        { a: 1 },\n        { a: 2 },\n    }};\n];\n"
        );
    }

    #[test]
    fn values_containing_blocks_are_blocks() {
        // Several items, one broken: one per line.
        let source = "operator main{} [ call f{a: tuple { r: relation { { x: 1 }, { x: 2 } }, s: 1 }, b: 1}; ];";
        assert_eq!(
            fmt(source),
            "operator main{} [\n    call f{\n        a: tuple {\n            r: relation {\n                { x: 1 },\n                { x: 2 },\n            },\n            s: 1,\n        },\n        b: 1,\n    };\n];\n"
        );
        // A single item stays hugged.
        assert_eq!(
            fmt(
                "operator main{} [ call f{a: tuple { r: relation { { x: 1 }, { x: 2 } } }, b: 1}; ];"
            ),
            "operator main{} [\n    call f{\n        a: tuple { r: relation {\n            { x: 1 },\n            { x: 2 },\n        } },\n        b: 1,\n    };\n];\n"
        );
        // Nothing broken inside: one line, as before.
        assert_eq!(
            fmt("operator main{} [ call f{a: tuple { r: relation { { x: 1 } } }, b: 2}; ];"),
            "operator main{} [\n    call f{a: tuple { r: relation { { x: 1 } } }, b: 2};\n];\n"
        );
    }

    #[test]
    fn the_prelude_is_canonical() {
        assert_eq!(fmt(trendy_prelude_source()), trendy_prelude_source());
    }

    fn trendy_prelude_source() -> &'static str {
        include_str!("../../prelude/src/prelude.trendy")
    }

    #[test]
    fn tokens_never_run_together() {
        // A parse with errors still has its tokens; the formatter must keep
        // them apart even where no rule puts a space.
        let source = "var x: =1;";
        let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
        let out = format(&parse.tree);
        assert_eq!(significant_tokens(&out), significant_tokens(source));
    }

    #[test]
    fn relation_tuples_follow_the_declared_heading() {
        assert_eq!(
            fmt(
                "var r: relation { a: boolean, b: integer } := relation {{a:true, b:3}, {b:25, a:false}};"
            ),
            "var r: relation { a: boolean, b: integer } := relation {\n    { a: true, b: 3 },\n    { a: false, b: 25 },\n};\n"
        );
    }

    #[test]
    fn heading_order_is_as_written_not_alphabetical() {
        assert_eq!(
            fmt("var r: relation { b: integer, a: boolean } := relation { { a: true, b: 3 } };"),
            "var r: relation { b: integer, a: boolean } := relation { { b: 3, a: true } };\n"
        );
    }

    #[test]
    fn the_selector_heading_wins() {
        assert_eq!(
            fmt(
                "var r := relation { y: integer, x: integer } { { x: 1, y: 2 }, tuple { x: 3, y: 4, } };"
            ),
            "var r := relation { y: integer, x: integer } {\n    { y: 2, x: 1 },\n    tuple { y: 4, x: 3 },\n};\n"
        );
    }

    #[test]
    fn without_a_heading_the_first_tuple_sets_the_order() {
        assert_eq!(
            fmt("var r := relation { { n: 1, m: 2 }, { m: 3, n: 4 } };"),
            "var r := relation {\n    { n: 1, m: 2 },\n    { n: 4, m: 3 },\n};\n"
        );
    }

    #[test]
    fn nested_relations_follow_their_attribute_types() {
        assert_eq!(
            fmt(
                "var t: tuple { r: relation { b: integer, a: integer } } := tuple { r: relation { { a: 1, b: 2 } } };"
            ),
            "var t: tuple { r: relation { b: integer, a: integer } } := tuple { r: relation { { b: 2, a: 1 } } };\n"
        );
        assert_eq!(
            fmt(
                "operator f{r: relation { b: integer, a: integer }} [];operator main{}[call f{r: relation { { a: 1, b: 2 } }};];"
            ),
            "operator f{r: relation { b: integer, a: integer }} [];\n\noperator main{} [\n    call f{r: relation { { b: 2, a: 1 } }};\n];\n"
        );
    }

    #[test]
    fn boolean_operators() {
        assert_eq!(
            fmt("var a:=not(true)and false or  true xor\ntrue implies not false;"),
            "var a := not (true) and false or true xor true implies not false;\n"
        );
        assert_eq!(
            fmt("operator main{}[call debug_value{v:( a or b )};];"),
            "operator main{} [\n    call debug_value{v: (a or b)};\n];\n"
        );
    }

    #[test]
    fn arithmetic_operators() {
        assert_eq!(
            fmt("var a:=-1+2*-x div(3-+4)mod  5;"),
            "var a := -1 + 2 * -x div (3 - +4) mod 5;\n"
        );
        assert_eq!(fmt("var a := - -1;"), "var a := - -1;\n");
        assert_eq!(fmt("var a := 1e0/-2e0;"), "var a := 1e0 / -2e0;\n");
        assert_eq!(fmt("var a := \"a\"||b;"), "var a := \"a\" || b;\n");
        assert_eq!(
            fmt("var a := x<>-1 and (y>=2)=(z<=3);"),
            "var a := x <> -1 and (y >= 2) = (z <= 3);\n"
        );
        assert_eq!(fmt("var a := not-1;"), "var a := not -1;\n");
    }

    #[test]
    fn joins_of_lists() {
        assert_eq!(
            fmt("var a:=join { b,c{x} ,times{d,e} }join f;"),
            "var a := join{b, c{x}, times{d, e}} join f;\n"
        );
        assert_eq!(
            fmt("var a := compose [ b,c ,d ];"),
            "var a := compose[b, c, d];\n"
        );
        let names: Vec<String> = (0..12).map(|i| format!("relation_number_{i}")).collect();
        assert_eq!(
            fmt(&format!("var r := join{{{}}};", names.join(", "))),
            format!("var r := join{{\n    {},\n}};\n", names.join(",\n    "))
        );
    }

    #[test]
    fn assignments() {
        assert_eq!(
            fmt("var a:=r   extend{t:=x+1,u :=r{y}extend{z:=2}};"),
            "var a := r extend { t := x + 1, u := r{y} extend { z := 2 } };\n"
        );
        assert_eq!(
            fmt("var a:=r   replace{t:=t+1};"),
            "var a := r replace { t := t + 1 };\n"
        );
    }

    #[test]
    fn groupings() {
        assert_eq!(
            fmt("var a:=r   group{g:={ x,y },h :={z}};"),
            "var a := r group { g := {x, y}, h := {z} };\n"
        );
    }

    #[test]
    fn ungroupings() {
        assert_eq!(
            fmt("var a:=r   ungroup{ g,h }ungroup {i};"),
            "var a := r ungroup {g, h} ungroup {i};\n"
        );
    }

    #[test]
    fn wrappings() {
        assert_eq!(
            fmt("var a:=r   wrap{w:={ x,y }}unwrap { w };"),
            "var a := r wrap { w := {x, y} } unwrap {w};\n"
        );
    }

    #[test]
    fn taken() {
        assert_eq!(
            fmt("var a:=tuple   from r;var b:=value  from(r{x});"),
            "var a := tuple from r;\nvar b := value from (r{x});\n"
        );
    }

    #[test]
    fn images() {
        assert_eq!(
            fmt("var a:=r extend{t:=!!  (s rename{x as y}),u:=(!! s)join s,v:=s join!!s};"),
            "var a := r extend { t := !!(s rename {x as y}), u := (!!s) join s, v := s join !!s };\n"
        );
    }

    #[test]
    fn join_operators() {
        assert_eq!(
            fmt("var a:=b   join c{x}times(d\ncompose e rename{f as g});"),
            "var a := b join c{x} times (d compose e rename {f as g});\n"
        );
    }

    #[test]
    fn comments() {
        // A header, kept apart by its blank line.
        assert_eq!(
            fmt("/* header */\n\nvar a := 1;"),
            "/* header */\n\nvar a := 1;\n"
        );
        // Own line between definitions, and trailing.
        assert_eq!(
            fmt("var a := 1;   /* one */\n/* about b */\nvar b := 2;"),
            "var a := 1; /* one */\n/* about b */\nvar b := 2;\n"
        );
        // Inside a body: indented like the statement after it.
        assert_eq!(
            fmt("operator main{} [\n/* first */\ncall f{};\n    call g{}; /* last */\n];"),
            "operator main{} [\n    /* first */\n    call f{};\n    call g{}; /* last */\n];\n"
        );
        // Inline, with the spacing the code has there.
        assert_eq!(
            fmt("var a:=1/* one */+  2 /* two */;"),
            "var a := 1 /* one */ + 2 /* two */;\n"
        );
        assert_eq!(
            fmt("operator main{} [ call f{/* none */}; ];"),
            "operator main{} [\n    call f{/* none */};\n];\n"
        );
        // On its own line where the layout keeps one line: inline.
        assert_eq!(
            fmt("var t := tuple { a: 1,\n/* b next */\nb: 2 };"),
            "var t := tuple { a: 1, /* b next */ b: 2 };\n"
        );
        // In a relation's lines.
        assert_eq!(
            fmt("var r := relation {\n{ a: 1 }, /* first */\n/* second */\n{ a: 2 }\n};"),
            "var r := relation {\n    { a: 1 }, /* first */\n    /* second */\n    { a: 2 },\n};\n"
        );
        // Nested and multi-line comments are kept as written.
        let nested = "/* a /* nested */\n   comment */\nvar a := 1;\n";
        assert_eq!(fmt(nested), nested);
        // Blank lines between comments are kept, one at most.
        assert_eq!(
            fmt("var a := 1;\n\n/* one */\n\n\n/* two */\n/* three */\nvar b := 2;"),
            "var a := 1;\n\n/* one */\n\n/* two */\n/* three */\nvar b := 2;\n"
        );
        assert_eq!(
            fmt("var a := 1;\n/* one */\n\n/* two */"),
            "var a := 1;\n/* one */\n\n/* two */\n"
        );
        // At the end of the file.
        assert_eq!(fmt("var a := 1;\n/* end */"), "var a := 1;\n/* end */\n");
        assert_eq!(fmt("var a := 1; /* end */"), "var a := 1; /* end */\n");
        assert_eq!(fmt("/* only */"), "/* only */\n");
    }

    #[test]
    fn comments_are_idempotent_and_kept() {
        let source = "/* h */\nvar a/*x*/:=1;/* t */\n\n/* o */\noperator main{}[/* e */\n/* s */\ncall f{v:a/*y*/};];/* z */";
        let once = fmt(source);
        assert_eq!(fmt(&once), once);
        let comments = |s: &str| {
            trendy_lexer::lex(s)
                .tokens
                .iter()
                .filter(|t| t.kind == SyntaxKind::Comment)
                .map(|t| s[t.span.start..t.span.end].to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(comments(&once), comments(source));
        assert_eq!(significant_tokens(&once), significant_tokens(source));
    }

    #[test]
    fn projections() {
        assert_eq!(
            fmt("var a:=b { c,d }{c}remove{  c }  ;"),
            "var a := b{c, d}{c} remove {c};\n"
        );
        assert_eq!(fmt("var a := b{};"), "var a := b{};\n");
        assert_eq!(
            fmt("var a := - t . b . c+u{ c }.c;"),
            "var a := -t.b.c + u{c}.c;\n"
        );
        assert_eq!(fmt("var a := (b){ c };"), "var a := (b){c};\n");
        assert_eq!(
            fmt("var a:=b rename{ c  as d,e as f }{d};"),
            "var a := b rename {c as d, e as f}{d};\n"
        );
        // A selector projected keeps its own spacing.
        assert_eq!(
            fmt("var a:=relation{{c:1}}{c};var t:=tuple{c:1}{c};"),
            "var a := relation { { c: 1 } }{c};\nvar t := tuple { c: 1 }{c};\n"
        );
    }

    #[test]
    fn blank_lines_group_statements() {
        assert_eq!(
            fmt("operator main{} [\ncall a{};\n\n\n\ncall b{};\ncall c{};\n\n];"),
            "operator main{} [\n    call a{};\n\n    call b{};\n    call c{};\n];\n"
        );
    }

    #[test]
    fn long_lines_break_their_outermost_list() {
        let source = "operator extract{a: tuple { street: text, city: text, country: text, postal_code: text }, p: tuple { x: approximate, y: approximate }} [];";
        let expected = "operator extract{\n    a: tuple { street: text, city: text, country: text, postal_code: text },\n    p: tuple { x: approximate, y: approximate },\n} [];\n";
        assert_eq!(fmt(source), expected);
        assert_eq!(fmt(expected), expected, "idempotent");
        // Within a broken list, an item still too long breaks in turn.
        let deep = fmt(
            "var t := tuple { a: tuple { street: \"1 Rue de Rivoli\", city: \"Paris\", country: \"République française\", postal_code: \"75001\" }, b: 1 };",
        );
        assert_eq!(
            deep,
            "var t := tuple {\n    a: tuple {\n        street: \"1 Rue de Rivoli\",\n        city: \"Paris\",\n        country: \"République française\",\n        postal_code: \"75001\",\n    },\n    b: 1,\n};\n"
        );
        assert!(deep.lines().all(|l| l.chars().count() <= MAX_WIDTH));
        // A line with nothing to break stays as it is.
        let text = format!("var t := \"{}\";", "Невский ".repeat(20));
        assert_eq!(fmt(&text), format!("{text}\n"));
    }

    #[test]
    fn empty_file() {
        assert_eq!(fmt("  \n"), "");
    }

    #[test]
    fn idempotent_and_keeps_every_token() {
        for source in [
            "operator main{} [];",
            "operator a { } [ ] ; operator main{}[];",
            "\n\n  operator   x{}\n\n\n[];\n\n",
            "var a:integer:=1;var b:=\"t\\n\";operator main{}[];",
            "var p:tuple{a:tuple{},b:text}:=tuple{a:tuple{},b:\"\",};",
            "var r:=relation{a:integer}{{a:1},tuple{a:2},};",
            "var r:relation{a:boolean,b:integer}:=relation{{b:1,a:true},{a:false,b:2},};",
            "builtin type text;builtin operator f{v:any type};operator main{}[call f{v:1};call f{v:2};];",
            "operator f{a:T,b:any  relation}:T types{T:any type}[a];",
        ] {
            let once = fmt(source);
            assert_eq!(fmt(&once), once, "not idempotent for {source:?}");
            // Attributes may move, and commas after lists' last items go
            // with the layout, but no other token is added or dropped.
            let sorted = |tokens: Vec<String>| {
                let mut tokens: Vec<String> = tokens.into_iter().filter(|t| t != ",").collect();
                tokens.sort();
                tokens
            };
            assert_eq!(
                sorted(significant_tokens(&once)),
                sorted(significant_tokens(source))
            );
        }
    }
}
