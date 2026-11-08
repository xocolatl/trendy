//! A typed view of the CST.
//!
//! Each type wraps a node of one kind and gives named access to its parts.
//! Parts are `Option` because a tree with syntax errors can be missing any of
//! them.

use std::collections::BTreeSet;

use trendy_diag::Span;

use crate::literal;
use crate::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, Trendy, span};

/// A typed wrapper around a node of one particular kind.
pub trait AstNode: Sized {
    fn cast(node: SyntaxNode) -> Option<Self>;
    fn syntax(&self) -> &SyntaxNode;

    fn span(&self) -> Span {
        span(self.syntax().text_range())
    }
}

macro_rules! ast_node {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name(pub(crate) SyntaxNode);

        impl AstNode for $name {
            fn cast(node: SyntaxNode) -> Option<Self> {
                (node.kind() == SyntaxKind::$name).then(|| $name(node))
            }

            fn syntax(&self) -> &SyntaxNode {
                &self.0
            }
        }
    };
}

ast_node!(Program);
ast_node!(SourceFile);
ast_node!(SectionHeader);
ast_node!(UsesDecl);
ast_node!(UnitList);
ast_node!(UsedUnit);
ast_node!(UnitName);
ast_node!(Qualifier);
ast_node!(Operator);
ast_node!(BuiltinOperator);
ast_node!(BuiltinType);
ast_node!(KindDecl);
ast_node!(HeadingDecl);
ast_node!(ConstraintDecl);
ast_node!(DatabaseDecl);
ast_node!(PartOf);
ast_node!(TypeDecl);
ast_node!(Supertype);
ast_node!(TypeBody);
ast_node!(Representation);
ast_node!(Conversion);
ast_node!(Constraint);
ast_node!(TypeOrder);
ast_node!(RequirementList);
ast_node!(Requirement);
ast_node!(Body);
ast_node!(CallStmt);
ast_node!(ReturnStmt);
ast_node!(FailStmt);
ast_node!(WarnStmt);
ast_node!(IfStmt);
ast_node!(WhileStmt);
ast_node!(LoopStmt);
ast_node!(ForStmt);
ast_node!(TransactionStmt);
ast_node!(Collecting);
ast_node!(AssignStmt);
ast_node!(AssignBlock);
ast_node!(Assignment);
ast_node!(Insertion);
ast_node!(Deletion);
ast_node!(Update);
ast_node!(Load);
ast_node!(ValueBlock);
ast_node!(OrderList);
ast_node!(Keeping);
ast_node!(ElementName);
ast_node!(Numbering);
ast_node!(LoadCondition);
ast_node!(OrderItem);
ast_node!(Arguments);
ast_node!(PositionalArg);
ast_node!(NameRef);
ast_node!(UnaryExpr);
ast_node!(BinaryExpr);
ast_node!(IsExpr);
ast_node!(TreatExpr);
ast_node!(FailExpr);
ast_node!(ParenExpr);
ast_node!(ExtractExpr);
ast_node!(ProjectExpr);
ast_node!(MethodCallExpr);
ast_node!(PostfixExpr);
ast_node!(NaryExpr);
ast_node!(ImageExpr);
ast_node!(FromExpr);
ast_node!(CallExpr);
ast_node!(NameList);
ast_node!(RenameList);
ast_node!(Renaming);
ast_node!(GatherList);
ast_node!(Gathering);
ast_node!(AssignList);
ast_node!(Assign);
ast_node!(VarDecl);
ast_node!(QualifiedName);
ast_node!(Key);
ast_node!(Name);
ast_node!(TypeRef);
ast_node!(AnyType);
ast_node!(TypeParameters);
ast_node!(Literal);
ast_node!(Heading);
ast_node!(HeadingRef);
ast_node!(AttributeType);
ast_node!(Per);
ast_node!(Mode);
ast_node!(TupleType);
ast_node!(RelationType);
ast_node!(ArrayType);
ast_node!(ArrayLiteral);
ast_node!(IndexExpr);
ast_node!(IfExpr);
ast_node!(TupleLiteral);
ast_node!(AttributeValue);
ast_node!(RelationLiteral);

/// A top-level definition.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Item {
    Operator(Operator),
    Builtin(BuiltinOperator),
    BuiltinType(BuiltinType),
    Kind(KindDecl),
    Heading(HeadingDecl),
    Type(TypeDecl),
    /// A variable of the program's, without a database's name before it.
    Variable(VarDecl),
    /// A constant of the program's, `const name[: type] := value;`.
    Constant(VarDecl),
    /// A database's relvar, `var kitchen.pantry: …`.
    Relvar(VarDecl),
    Constraint(ConstraintDecl),
    Database(DatabaseDecl),
}

impl Item {
    /// The definition `node` is, if it is one.
    #[must_use]
    pub fn cast(node: SyntaxNode) -> Option<Item> {
        Operator::cast(node.clone())
            .map(Item::Operator)
            .or_else(|| BuiltinOperator::cast(node.clone()).map(Item::Builtin))
            .or_else(|| BuiltinType::cast(node.clone()).map(Item::BuiltinType))
            .or_else(|| KindDecl::cast(node.clone()).map(Item::Kind))
            .or_else(|| HeadingDecl::cast(node.clone()).map(Item::Heading))
            .or_else(|| TypeDecl::cast(node.clone()).map(Item::Type))
            .or_else(|| {
                VarDecl::cast(node.clone()).map(|decl| {
                    if decl.is_const() {
                        Item::Constant(decl)
                    } else if decl.part_of().is_some() {
                        Item::Relvar(decl)
                    } else {
                        Item::Variable(decl)
                    }
                })
            })
            .or_else(|| ConstraintDecl::cast(node.clone()).map(Item::Constraint))
            .or_else(|| DatabaseDecl::cast(node).map(Item::Database))
    }

    #[must_use]
    pub fn syntax(&self) -> &SyntaxNode {
        match self {
            Item::Operator(op) => op.syntax(),
            Item::Builtin(decl) => decl.syntax(),
            Item::BuiltinType(decl) => decl.syntax(),
            Item::Kind(decl) => decl.syntax(),
            Item::Heading(decl) => decl.syntax(),
            Item::Type(decl) => decl.syntax(),
            Item::Variable(decl) | Item::Constant(decl) | Item::Relvar(decl) => decl.syntax(),
            Item::Constraint(decl) => decl.syntax(),
            Item::Database(decl) => decl.syntax(),
        }
    }
}

impl Program {
    /// The program of `files`, with each file's tree, one after another: a
    /// span in it is an offset into their texts one after another, so the
    /// first file's spans are those it has alone.
    #[must_use]
    pub fn new(files: &[SourceFile]) -> Program {
        let children = files
            .iter()
            .map(|file| rowan::NodeOrToken::Node(file.0.green().into_owned()));
        let kind = <Trendy as rowan::Language>::kind_to_raw(SyntaxKind::Program);
        Program(SyntaxNode::new_root(rowan::GreenNode::new(kind, children)))
    }

    /// Its files, in order.
    pub fn files(&self) -> impl Iterator<Item = SourceFile> {
        self.0.children().filter_map(SourceFile::cast)
    }
}

/// The section of a unit a declaration is in: its interface, which other
/// units see, or its implementation, which they don't.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Section {
    Interface,
    Implementation,
}

/// The section of its unit the declaration `node` is in: the one the
/// nearest header before it starts, or the implementation, before any.
#[must_use]
pub fn section_of(node: &SyntaxNode) -> Section {
    // The siblings before it, read in the green tree, which makes no node
    // of each, as asking for every declaration's section would.
    let Some(parent) = node.parent() else {
        return Section::Implementation;
    };
    parent
        .green()
        .children()
        .take(node.index())
        .filter_map(rowan::NodeOrToken::into_node)
        .rfind(|n| {
            <Trendy as rowan::Language>::kind_from_raw(n.kind()) == SyntaxKind::SectionHeader
        })
        .map_or(Section::Implementation, |header| {
            let word = header
                .children()
                .filter_map(rowan::NodeOrToken::into_token)
                .find(|t| !<Trendy as rowan::Language>::kind_from_raw(t.kind()).is_trivia());
            section_started(word.map(rowan::GreenTokenData::text))
        })
}

/// The section a header whose word is `word` starts.
fn section_started(word: Option<&str>) -> Section {
    let interface = word.is_some_and(|word| {
        word.eq_ignore_ascii_case("interface") || word.eq_ignore_ascii_case("public")
    });
    if interface {
        Section::Interface
    } else {
        Section::Implementation
    }
}

impl SectionHeader {
    /// The word, `interface` or `implementation`, in any case, which the
    /// parser reports.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The section it starts: C++'s `public:`, which the parser reports,
    /// starts the interface too.
    #[must_use]
    pub fn section(&self) -> Section {
        section_started(self.keyword().as_ref().map(SyntaxToken::text))
    }
}

impl UsesDecl {
    /// The word `uses`, which the parser reports in another case.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The units it names, in the order written: in its braces, or, in
    /// Delphi's `uses`, which the parser reports, without them.
    pub fn units(&self) -> impl Iterator<Item = UsedUnit> {
        self.0.descendants().filter_map(UsedUnit::cast)
    }

    /// The section its declarations are in, whose declarations see what it
    /// names.
    #[must_use]
    pub fn section(&self) -> Section {
        section_of(&self.0)
    }
}

impl UsedUnit {
    #[must_use]
    pub fn name(&self) -> Option<UnitName> {
        self.0.children().find_map(UnitName::cast)
    }

    /// The name the using unit gives it, after `as`.
    #[must_use]
    pub fn alias(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }
}

impl UnitName {
    /// Its parts, the directories of its path and its file's stem, as in
    /// `lakes` and `depths`: each a word, which can be a keyword, as a
    /// file can be named `join.trendy`.
    pub fn parts(&self) -> impl Iterator<Item = SyntaxToken> {
        significant_tokens(&self.0)
            .filter(|t| t.kind() == SyntaxKind::Ident || t.kind().is_keyword())
    }

    /// Its parts joined by `.`, as in `lakes.depths`.
    #[must_use]
    pub fn text(&self) -> String {
        let parts: Vec<String> = self.parts().map(|part| part.text().to_string()).collect();
        parts.join(".")
    }
}

impl SourceFile {
    /// Whether it declares an operator named `main`, where a program starts.
    #[must_use]
    pub fn declares_main(&self) -> bool {
        self.items().any(|item| match item {
            Item::Operator(operator) => operator
                .name()
                .and_then(|name| name.ident())
                .is_some_and(|ident| ident.text() == "main"),
            _ => false,
        })
    }

    /// Whether its interface declares anything, for other units to see.
    #[must_use]
    pub fn exports(&self) -> bool {
        self.items()
            .any(|item| section_of(item.syntax()) == Section::Interface)
    }

    /// Its sections' headers, in source order.
    pub fn headers(&self) -> impl Iterator<Item = SectionHeader> {
        self.0.children().filter_map(SectionHeader::cast)
    }

    /// Its sections' `uses`, in source order.
    pub fn uses(&self) -> impl Iterator<Item = UsesDecl> {
        self.0.children().filter_map(UsesDecl::cast)
    }

    /// Every definition, in source order.
    pub fn items(&self) -> impl Iterator<Item = Item> {
        self.0.children().filter_map(Item::cast)
    }

    pub fn databases(&self) -> impl Iterator<Item = DatabaseDecl> {
        self.0.children().filter_map(DatabaseDecl::cast)
    }

    pub fn constraints(&self) -> impl Iterator<Item = ConstraintDecl> {
        self.0.children().filter_map(ConstraintDecl::cast)
    }

    pub fn headings(&self) -> impl Iterator<Item = HeadingDecl> {
        self.0.children().filter_map(HeadingDecl::cast)
    }

    /// The heading named `name`'s declaration: the first with its name, as
    /// a second is an error.
    #[must_use]
    pub fn heading(&self, name: &str) -> Option<HeadingDecl> {
        self.headings().find(|decl| {
            decl.name()
                .and_then(|n| n.ident())
                .is_some_and(|i| i.text() == name)
        })
    }

    pub fn types(&self) -> impl Iterator<Item = TypeDecl> {
        self.0.children().filter_map(TypeDecl::cast)
    }

    pub fn operators(&self) -> impl Iterator<Item = Operator> {
        self.0.children().filter_map(Operator::cast)
    }

    /// The program's variables, without the databases' relvars.
    pub fn variables(&self) -> impl Iterator<Item = VarDecl> {
        self.0
            .children()
            .filter_map(VarDecl::cast)
            .filter(|decl| decl.part_of().is_none() && !decl.is_const())
    }

    /// The program's constants.
    pub fn constants(&self) -> impl Iterator<Item = VarDecl> {
        self.0
            .children()
            .filter_map(VarDecl::cast)
            .filter(VarDecl::is_const)
    }

    /// The databases' relvars, each `var kitchen.pantry: …`.
    pub fn relvars(&self) -> impl Iterator<Item = VarDecl> {
        self.0
            .children()
            .filter_map(VarDecl::cast)
            .filter(|decl| decl.part_of().is_some())
    }
}

impl Operator {
    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// The parameters: a heading.
    #[must_use]
    pub fn params(&self) -> Option<Heading> {
        self.0.children().find_map(Heading::cast)
    }

    /// The constraints inside its braces, after its parameters, which say
    /// which arguments a call of it can have.
    pub fn constraints(&self) -> impl Iterator<Item = Constraint> + use<> {
        self.params().into_iter().flat_map(|heading| {
            heading
                .0
                .children()
                .filter_map(Constraint::cast)
                .collect::<Vec<_>>()
        })
    }

    /// The result type, from `: type`: a read-only operator's.
    #[must_use]
    pub fn result(&self) -> Option<TypeExpr> {
        self.0
            .children()
            .find(|n| n.kind() == SyntaxKind::TypeAnnotation)?
            .children()
            .find_map(TypeExpr::cast)
    }

    /// The type parameters, `types { T: any type }`.
    #[must_use]
    pub fn type_parameters(&self) -> Option<TypeParameters> {
        self.0.children().find_map(TypeParameters::cast)
    }

    /// Every type written by name in it, nested ones too, in source order,
    /// but in its type parameters, which declare them.
    pub fn type_refs(&self) -> impl Iterator<Item = TypeRef> + use<> {
        self.0.descendants().filter_map(TypeRef::cast).filter(|t| {
            !t.0.ancestors()
                .any(|a| a.kind() == SyntaxKind::TypeParameters)
        })
    }

    /// The span to delete to remove the result type, `: type`, with the
    /// whitespace before it.
    #[must_use]
    pub fn result_removal(&self) -> Option<Span> {
        let annotation = self
            .0
            .children()
            .find(|n| n.kind() == SyntaxKind::TypeAnnotation)?;
        Some(Span::new(
            preceded_from(&annotation.clone().into()),
            span(annotation.text_range()).end,
        ))
    }

    #[must_use]
    pub fn body(&self) -> Option<Body> {
        self.0.children().find_map(Body::cast)
    }
}

impl BuiltinOperator {
    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// The parameters: a heading.
    #[must_use]
    pub fn params(&self) -> Option<Heading> {
        self.0.children().find_map(Heading::cast)
    }

    /// The result type, from `: type`: a read-only built-in's.
    #[must_use]
    pub fn result(&self) -> Option<TypeExpr> {
        self.0
            .children()
            .find(|n| n.kind() == SyntaxKind::TypeAnnotation)?
            .children()
            .find_map(TypeExpr::cast)
    }

    /// The type parameters, `types { T: any ordered }`.
    #[must_use]
    pub fn type_parameters(&self) -> Option<TypeParameters> {
        self.0.children().find_map(TypeParameters::cast)
    }

    /// The `builtin` keyword.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).find(|t| t.kind() == SyntaxKind::BuiltinKw)
    }
}

impl BuiltinType {
    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// The kinds it is of, `{ ordered, … }`.
    #[must_use]
    pub fn kinds(&self) -> Option<NameList> {
        self.0.children().find_map(NameList::cast)
    }

    /// The `builtin` keyword.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).find(|t| t.kind() == SyntaxKind::BuiltinKw)
    }
}

impl HeadingDecl {
    /// The word `heading`, or another language's for a type, which the
    /// parser reports.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// The attributes it names.
    #[must_use]
    pub fn heading(&self) -> Option<Heading> {
        self.0.children().find_map(Heading::cast)
    }
}

impl ConstraintDecl {
    /// The word `constraint`, or SQL's `create`, which the parser reports.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The database it adds the constraint to, `kitchen.` before its name,
    /// if it is one's.
    #[must_use]
    pub fn part_of(&self) -> Option<PartOf> {
        self.0.children().find_map(PartOf::cast)
    }

    /// Its name, which the parser reports missing.
    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// The condition the unit's variables satisfy.
    #[must_use]
    pub fn condition(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

impl DatabaseDecl {
    /// The word `database`, or SQL's `create`, which the parser reports.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }
}

impl PartOf {
    /// The database's name.
    #[must_use]
    pub fn database(&self) -> Option<NameRef> {
        self.0.children().find_map(NameRef::cast)
    }
}

impl TypeDecl {
    /// The word `type`, which the parser reports in another case.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// The word `union`, which makes it a union type, if it is one, in
    /// any case, which the parser reports.
    #[must_use]
    pub fn union_keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).find(|t| {
            t.kind() == SyntaxKind::UnionKw
                || (t.kind() == SyntaxKind::Ident && t.text().eq_ignore_ascii_case("union"))
        })
    }

    /// `is name`: the union type it is a subtype of, if it is one.
    #[must_use]
    pub fn supertype(&self) -> Option<Supertype> {
        self.0.children().find_map(Supertype::cast)
    }

    /// Its body, the braces around its representations, constraints and
    /// order, which a union type has none of.
    #[must_use]
    pub fn body(&self) -> Option<TypeBody> {
        self.0.children().find_map(TypeBody::cast)
    }

    /// Its first possible representation (see
    /// [`TypeDecl::representations`]).
    #[must_use]
    pub fn representation(&self) -> Option<Representation> {
        self.representations().next()
    }

    /// Its possible representations, in order: in its body, or, where the
    /// components were written without braces of their own, which the
    /// parser reports, in its place.
    pub fn representations(&self) -> impl Iterator<Item = Representation> + use<> {
        let body = self.0.children().find_map(TypeBody::cast);
        let within = body.map_or_else(|| self.0.clone(), |body| body.0);
        within.children().filter_map(Representation::cast)
    }

    /// Its constraints, which say which values it has, in order: those of
    /// the possible representation its values are stored as, whose
    /// components are every value's, then those in its body. Another
    /// representation's are its own ([`Possrep::constraints`]).
    pub fn constraints(&self) -> impl Iterator<Item = Constraint> + use<> {
        self.constraints_of(&self.possreps())
    }

    /// Its constraints, as [`TypeDecl::constraints`] gives them, of its
    /// possible representations `possreps`, as [`TypeDecl::possreps`] gives
    /// them.
    pub fn constraints_of(&self, possreps: &[Possrep]) -> impl Iterator<Item = Constraint> + use<> {
        let stored = possreps
            .iter()
            .find(|p| p.is_stored())
            .map(|p| p.written.constraints().collect::<Vec<_>>())
            .unwrap_or_default();
        let body = self
            .0
            .children()
            .find_map(TypeBody::cast)
            .into_iter()
            .flat_map(|body| {
                body.0
                    .children()
                    .filter_map(Constraint::cast)
                    .collect::<Vec<_>>()
            });
        stored
            .into_iter()
            .chain(body)
            .collect::<Vec<_>>()
            .into_iter()
    }

    /// Its declaration in its unit's implementation, if its unit's
    /// interface declares it and the implementation declares it again,
    /// which gives its possible representations' conversions, which the
    /// type's users don't see.
    #[must_use]
    pub fn implementation(&self) -> Option<TypeDecl> {
        self.other_part(Section::Interface, Section::Implementation)
    }

    /// The declaration in its unit's interface that it completes, if it is
    /// one: a declaration, in the implementation, of a type the interface
    /// declares, which gives its possible representations' conversions.
    #[must_use]
    pub fn interface(&self) -> Option<TypeDecl> {
        self.other_part(Section::Implementation, Section::Interface)
    }

    /// The first declaration of its name in `other`, the section of its
    /// file it isn't in, if it is in `own`.
    fn other_part(&self, own: Section, other: Section) -> Option<TypeDecl> {
        if section_of(&self.0) != own {
            return None;
        }
        let name = self.name()?.ident()?.text().to_string();
        // The file's declarations, each in the section of the header last
        // before it.
        let mut section = Section::Implementation;
        for node in self.0.parent()?.children() {
            if let Some(header) = SectionHeader::cast(node.clone()) {
                section = header.section();
            } else if section == other
                && let Some(decl) = TypeDecl::cast(node)
                && decl
                    .name()
                    .and_then(|n| n.ident())
                    .is_some_and(|i| i.text() == name)
            {
                return Some(decl);
            }
        }
        None
    }

    /// Its possible representations, in order, each where its components
    /// are written, here, with where its conversions are: here too, or, if
    /// its unit's interface declares it, in the representation of the same
    /// components that its declaration in the implementation writes
    /// ([`TypeDecl::implementation`]).
    #[must_use]
    pub fn possreps(&self) -> Vec<Possrep> {
        self.possreps_with(self.implementation().as_ref())
    }

    /// Its possible representations, as [`TypeDecl::possreps`] gives them,
    /// of `implementation`, its declaration in its unit's implementation,
    /// if it has one.
    #[must_use]
    pub fn possreps_with(&self, implementation: Option<&TypeDecl>) -> Vec<Possrep> {
        let implementation: Vec<Representation> = implementation
            .map(|decl| decl.representations().collect())
            .unwrap_or_default();
        self.representations()
            .map(|written| {
                let names = written.component_names();
                let converted = implementation
                    .iter()
                    .find(|r| r.component_names() == names)
                    .cloned()
                    .unwrap_or_else(|| written.clone());
                Possrep { written, converted }
            })
            .collect()
    }

    /// Its orders, of which it may have one: in its body, or, where the
    /// parser reports them out of place, among the components.
    pub fn orders(&self) -> impl Iterator<Item = TypeOrder> {
        self.0.descendants().filter_map(TypeOrder::cast)
    }

    /// The keys written among what it says of itself, in its body or
    /// after a representation's components, rather than after a
    /// component's type, where a key goes.
    pub fn misplaced_keys(&self) -> impl Iterator<Item = Key> + use<> {
        let body = self.body().map(|body| body.0);
        let components = self
            .representations()
            .filter_map(|r| r.components())
            .map(|heading| heading.0);
        body.into_iter()
            .chain(components)
            .flat_map(|node| node.children().filter_map(Key::cast).collect::<Vec<_>>())
    }
}

impl Supertype {
    /// The word `is`.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The supertype, by name.
    #[must_use]
    pub fn type_ref(&self) -> Option<TypeRef> {
        self.0.children().find_map(TypeRef::cast)
    }
}

impl Representation {
    /// Its components, each with its type.
    #[must_use]
    pub fn components(&self) -> Option<Heading> {
        self.0.children().find_map(Heading::cast)
    }

    /// Its conversions, `to` and `from`, in the order written: none for
    /// the representation values are stored as.
    pub fn conversions(&self) -> impl Iterator<Item = Conversion> + use<> {
        self.0.children().filter_map(Conversion::cast)
    }

    /// The constraints inside its braces, after its components, which say
    /// which of them represent a value.
    pub fn constraints(&self) -> impl Iterator<Item = Constraint> + use<> {
        self.components().into_iter().flat_map(|heading| {
            heading
                .0
                .children()
                .filter_map(Constraint::cast)
                .collect::<Vec<_>>()
        })
    }

    /// Its components' names, as written, in name order.
    #[must_use]
    pub fn component_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .components()
            .into_iter()
            .flat_map(|heading| heading.attributes().collect::<Vec<_>>())
            .filter_map(|a| Some(a.name()?.ident()?.text().to_string()))
            .collect();
        names.sort();
        names
    }
}

/// A possible representation of a type, as its declarations write it: its
/// components, with its own constraints, where the type is declared, and
/// its conversions, there too, or, for a type its unit's interface
/// declares, in its declaration in the implementation (see
/// [`TypeDecl::possreps`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Possrep {
    written: Representation,
    converted: Representation,
}

impl Possrep {
    /// Where its components are written.
    #[must_use]
    pub fn written(&self) -> &Representation {
        &self.written
    }

    /// Where its conversions are written: where its components are, or in
    /// its type's declaration in the implementation.
    #[must_use]
    pub fn converted(&self) -> &Representation {
        &self.converted
    }

    /// Its components, each with its type.
    #[must_use]
    pub fn components(&self) -> Option<Heading> {
        self.written.components()
    }

    /// Its conversions, `to` and `from`, in the order written: none for
    /// the representation values are stored as.
    pub fn conversions(&self) -> impl Iterator<Item = Conversion> + use<> {
        self.converted.conversions()
    }

    /// Whether its type's values are stored as it: whether it has no
    /// conversions.
    #[must_use]
    pub fn is_stored(&self) -> bool {
        self.conversions().next().is_none()
    }

    /// Its own constraints, which say which of its components represent a
    /// value, checked as one is selected in it: none for the
    /// representation values are stored as, whose components are every
    /// value's, so that its constraints are the type's
    /// ([`TypeDecl::constraints`]).
    pub fn constraints(&self) -> impl Iterator<Item = Constraint> + use<> {
        let own = (!self.is_stored()).then(|| self.written.clone());
        own.into_iter()
            .flat_map(|r| r.constraints().collect::<Vec<_>>())
    }
}

/// Which way a conversion goes: `to` the stored representation, or `from`
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    To,
    From,
}

impl Conversion {
    /// The word `to` or `from`.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// Which way it goes, as its keyword says, in any case.
    #[must_use]
    pub fn direction(&self) -> Option<Direction> {
        let keyword = self.keyword()?;
        if keyword.text().eq_ignore_ascii_case("to") {
            Some(Direction::To)
        } else if keyword.text().eq_ignore_ascii_case("from") {
            Some(Direction::From)
        } else {
            None
        }
    }

    /// The components it computes, each `name := value`.
    #[must_use]
    pub fn assignments(&self) -> Option<AssignList> {
        self.0.children().find_map(AssignList::cast)
    }

    /// The span to delete to remove it, with the whitespace directly
    /// before it.
    #[must_use]
    pub fn removal(&self) -> Span {
        statement_removal(&self.0)
    }
}

impl TypeOrder {
    /// The word `order`.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The components, in the order they compare values.
    #[must_use]
    pub fn list(&self) -> Option<OrderList> {
        self.0.children().find_map(OrderList::cast)
    }

    /// The span to delete to remove it from its type's braces, or its
    /// representation's, with the comma that separates it from the rest.
    #[must_use]
    pub fn removal(&self) -> Span {
        list_removal(&self.0)
    }
}

impl Constraint {
    /// The word `constraint`, which the parser reports in another case or
    /// spelling.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The condition every value of the type satisfies.
    #[must_use]
    pub fn condition(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    /// Every name written in its condition: of what it reads, and of
    /// attributes and local variables too.
    #[must_use]
    pub fn names(&self) -> BTreeSet<String> {
        self.0
            .descendants_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .filter(|token| token.kind() == SyntaxKind::Ident)
            .map(|token| token.text().to_string())
            .collect()
    }

    /// The span to delete to remove it from its type's braces, or its
    /// representation's, with the comma that separates it from the rest.
    #[must_use]
    pub fn removal(&self) -> Span {
        list_removal(&self.0)
    }
}

impl KindDecl {
    /// The word `kind`.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// What its types must have.
    pub fn requirements(&self) -> impl Iterator<Item = Requirement> + use<> {
        self.0
            .children()
            .filter_map(RequirementList::cast)
            .flat_map(|list| {
                list.0
                    .children()
                    .filter_map(Requirement::cast)
                    .collect::<Vec<_>>()
            })
    }
}

impl Requirement {
    /// The token it is: an operator, such as `<`.
    #[must_use]
    pub fn token(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }
}

impl Body {
    /// A read-only operator's value, the expression its body ends with.
    #[must_use]
    pub fn value(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    /// The span to delete to remove the value, with the whitespace before
    /// it, as for a statement.
    #[must_use]
    pub fn value_removal(&self) -> Option<Span> {
        self.value().map(|value| statement_removal(value.syntax()))
    }

    pub fn statements(&self) -> impl Iterator<Item = Statement> {
        statements_of(&self.0)
    }

    /// Whether a statement of the body decides what runs next (see
    /// [`decides_what_runs`]).
    #[must_use]
    pub fn decides_what_runs(&self) -> bool {
        decides_what_runs(self.statements())
    }

    /// Where something missing from the end of the body goes: just before
    /// its `]`.
    #[must_use]
    pub fn end(&self) -> Option<usize> {
        significant_tokens(&self.0)
            .filter(|t| t.kind() == SyntaxKind::RBracket)
            .last()
            .map(|t| span(t.text_range()).start)
    }
}

/// A statement in an operator's body.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Statement {
    Call(CallStmt),
    Assign(AssignStmt),
    Block(AssignBlock),
    Return(ReturnStmt),
    Fail(FailStmt),
    Warn(WarnStmt),
    If(IfStmt),
    While(WhileStmt),
    Loop(LoopStmt),
    For(ForStmt),
    Transaction(TransactionStmt),
    /// A local variable's declaration, as a program variable's is written.
    Var(VarDecl),
}

/// Whether one of `statements` decides what runs next: an `if`, a loop, a
/// transaction or a `fail`, so that they have to be run, where the others
/// just name values one after another.
pub fn decides_what_runs(mut statements: impl Iterator<Item = Statement>) -> bool {
    statements.any(|statement| {
        matches!(
            statement,
            Statement::If(_)
                | Statement::While(_)
                | Statement::Loop(_)
                | Statement::For(_)
                | Statement::Transaction(_)
                | Statement::Fail(_)
        )
    })
}

/// The statements among `node`'s children.
fn statements_of(node: &SyntaxNode) -> impl Iterator<Item = Statement> + use<> {
    node.children().filter_map(Statement::cast)
}

impl ValueBlock {
    pub fn statements(&self) -> impl Iterator<Item = Statement> {
        statements_of(&self.0)
    }

    /// The value, the expression the block ends with.
    #[must_use]
    pub fn value(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

impl Statement {
    /// The statement `node` is, if it is one. A variable's declaration is
    /// one where a body has it, and a program's definition elsewhere.
    #[must_use]
    pub fn cast(node: SyntaxNode) -> Option<Statement> {
        CallStmt::cast(node.clone())
            .map(Statement::Call)
            .or_else(|| AssignStmt::cast(node.clone()).map(Statement::Assign))
            .or_else(|| AssignBlock::cast(node.clone()).map(Statement::Block))
            .or_else(|| ReturnStmt::cast(node.clone()).map(Statement::Return))
            .or_else(|| FailStmt::cast(node.clone()).map(Statement::Fail))
            .or_else(|| WarnStmt::cast(node.clone()).map(Statement::Warn))
            .or_else(|| IfStmt::cast(node.clone()).map(Statement::If))
            .or_else(|| WhileStmt::cast(node.clone()).map(Statement::While))
            .or_else(|| LoopStmt::cast(node.clone()).map(Statement::Loop))
            .or_else(|| ForStmt::cast(node.clone()).map(Statement::For))
            .or_else(|| TransactionStmt::cast(node.clone()).map(Statement::Transaction))
            .or_else(|| VarDecl::cast(node).map(Statement::Var))
    }

    #[must_use]
    pub fn syntax(&self) -> &SyntaxNode {
        match self {
            Statement::Call(s) => s.syntax(),
            Statement::Assign(s) => s.syntax(),
            Statement::Block(s) => s.syntax(),
            Statement::Return(s) => s.syntax(),
            Statement::Fail(s) => s.syntax(),
            Statement::Warn(s) => s.syntax(),
            Statement::If(s) => s.syntax(),
            Statement::While(s) => s.syntax(),
            Statement::Loop(s) => s.syntax(),
            Statement::For(s) => s.syntax(),
            Statement::Transaction(s) => s.syntax(),
            Statement::Var(s) => s.syntax(),
        }
    }

    #[must_use]
    pub fn span(&self) -> Span {
        span(self.syntax().text_range())
    }
}

/// What follows an `if` statement's `else`: a block, or, for `else if`,
/// another `if`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ElseBranch {
    Block(Body),
    If(IfStmt),
}

impl WhileStmt {
    /// The `while`.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The condition, between `while` and `loop`.
    #[must_use]
    pub fn condition(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    /// The statements run while the condition is true.
    #[must_use]
    pub fn body(&self) -> Option<Body> {
        self.0.children().find_map(Body::cast)
    }
}

impl LoopStmt {
    /// The `loop`, or the word another language writes for it, which is
    /// reported: C's `do`.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The statements run once, then again while the condition is true.
    #[must_use]
    pub fn body(&self) -> Option<Body> {
        self.0.children().find_map(Body::cast)
    }

    /// The `while` after the block.
    #[must_use]
    pub fn while_keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).find(|t| t.kind() == SyntaxKind::WhileKw)
    }

    /// The condition, after `while`.
    #[must_use]
    pub fn condition(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

impl ForStmt {
    /// The `for`, or the word another language writes for it, which is
    /// reported: C#'s `foreach`.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The variable, which takes each element's value in turn.
    #[must_use]
    pub fn variable(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// The array whose elements the loop goes through, after `in`.
    #[must_use]
    pub fn array(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    /// The statements run for each element.
    #[must_use]
    pub fn body(&self) -> Option<Body> {
        self.0.children().find_map(Body::cast)
    }
}

impl TransactionStmt {
    /// The `transaction`.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// `collecting e`, which adds the errors it collects to the variable
    /// `e`, if it has one.
    #[must_use]
    pub fn collecting(&self) -> Option<Collecting> {
        self.0.children().find_map(Collecting::cast)
    }

    /// The statements, undone if one fails.
    #[must_use]
    pub fn body(&self) -> Option<Body> {
        self.0.children().find_map(Body::cast)
    }
}

impl Collecting {
    /// The word `collecting`.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The variable the errors are added to: its name, qualified by its
    /// unit's or not.
    #[must_use]
    pub fn variable(&self) -> Option<Expr> {
        self.0
            .children()
            .find(|node| matches!(node.kind(), SyntaxKind::NameRef | SyntaxKind::QualifiedName))
            .and_then(Expr::cast)
    }
}

impl IfStmt {
    /// The `if`, or the word another language writes for it, which is
    /// reported: Python's `elif` in an `else`.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The condition, between `if` and `then`.
    #[must_use]
    pub fn condition(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    /// The statements run if the condition is true.
    #[must_use]
    pub fn then_block(&self) -> Option<Body> {
        self.0.children().find_map(Body::cast)
    }

    /// What is run if the condition is false, if anything: what follows
    /// the block run if it is true.
    #[must_use]
    pub fn else_branch(&self) -> Option<ElseBranch> {
        self.0
            .children()
            .filter_map(|node| {
                Body::cast(node.clone())
                    .map(ElseBranch::Block)
                    .or_else(|| IfStmt::cast(node).map(ElseBranch::If))
            })
            .nth(1)
    }
}

impl ReturnStmt {
    /// The `return` keyword.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The value returned.
    #[must_use]
    pub fn value(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    /// The span to delete to remove the statement, as for
    /// [`AssignBlock::removal`].
    #[must_use]
    pub fn removal(&self) -> Span {
        statement_removal(&self.0)
    }
}

impl AssignStmt {
    /// The assignments, in source order: one, or, in a statement the
    /// parser reported, several separated by commas.
    pub fn clauses(&self) -> impl Iterator<Item = Clause> + use<> {
        self.0.children().filter_map(Clause::cast)
    }

    /// The `assign` block the statement is in, if it is in one.
    #[must_use]
    pub fn block(&self) -> Option<AssignBlock> {
        AssignBlock::cast(self.0.parent()?)
    }

    /// The span to delete to remove the statement, with the whitespace
    /// directly before it.
    #[must_use]
    pub fn removal(&self) -> Span {
        statement_removal(&self.0)
    }
}

impl AssignBlock {
    /// The `assign` keyword.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The assignment statements in the block, in source order.
    pub fn statements(&self) -> impl Iterator<Item = AssignStmt> + use<> {
        self.0.children().filter_map(AssignStmt::cast)
    }

    /// The span to delete to remove the block, with the whitespace directly
    /// before it.
    #[must_use]
    pub fn removal(&self) -> Span {
        statement_removal(&self.0)
    }
}

/// The span to delete to remove the statement `node`, with the whitespace
/// directly before it. A comment before it stays, since it may be about
/// what follows.
fn statement_removal(node: &SyntaxNode) -> Span {
    let own = span(node.text_range());
    let start = node
        .prev_sibling_or_token()
        .filter(|e| e.kind() == SyntaxKind::Whitespace)
        .map_or(own.start, |e| span(e.text_range()).start);
    Span::new(start, own.end)
}

/// One assignment in an assignment statement: `v := x`, or a shorthand for
/// one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Clause {
    /// `v := x`
    Assignment(Assignment),
    /// `insert r s` or `d_insert r s`
    Insertion(Insertion),
    /// `delete r where p` or `i_delete r s`
    Deletion(Deletion),
    /// `update r { a := x } where p`
    Update(Update),
    /// `load a from r order [asc x]`
    Load(Load),
}

impl Clause {
    #[must_use]
    pub fn cast(node: SyntaxNode) -> Option<Self> {
        Assignment::cast(node.clone())
            .map(Clause::Assignment)
            .or_else(|| Insertion::cast(node.clone()).map(Clause::Insertion))
            .or_else(|| Deletion::cast(node.clone()).map(Clause::Deletion))
            .or_else(|| Update::cast(node.clone()).map(Clause::Update))
            .or_else(|| Load::cast(node).map(Clause::Load))
    }

    #[must_use]
    pub fn syntax(&self) -> &SyntaxNode {
        match self {
            Clause::Assignment(c) => c.syntax(),
            Clause::Insertion(c) => c.syntax(),
            Clause::Deletion(c) => c.syntax(),
            Clause::Update(c) => c.syntax(),
            Clause::Load(c) => c.syntax(),
        }
    }

    #[must_use]
    pub fn span(&self) -> Span {
        span(self.syntax().text_range())
    }

    /// The variable assigned: the first expression.
    #[must_use]
    pub fn target(&self) -> Option<Expr> {
        self.syntax().children().find_map(Expr::cast)
    }

    /// The keyword it starts with, for a shorthand: `insert`, `d_insert`,
    /// `delete`, `i_delete` or `update`.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(self.syntax()).find(|t| t.kind().is_assignment_keyword())
    }

    /// The expression after the target: the value assigned, the relation
    /// inserted, deleted or loaded, or the condition of a `delete` or an
    /// `update`.
    #[must_use]
    pub fn operand(&self) -> Option<Expr> {
        self.syntax().children().filter_map(Expr::cast).nth(1)
    }

    /// The `where` before a condition, if there is one.
    #[must_use]
    pub fn where_keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(self.syntax()).find(|t| t.kind() == SyntaxKind::WhereKw)
    }

    /// The span to delete to remove the `where` and the condition, with the
    /// whitespace before them, up to the end of what comes before.
    #[must_use]
    pub fn condition_removal(&self) -> Option<Span> {
        let keyword = self.where_keyword()?;
        let end = self.operand()?.span().end;
        Some(Span::new(preceded_from(&keyword.into()), end))
    }

    /// The statement the assignment is in.
    #[must_use]
    pub fn statement(&self) -> Option<AssignStmt> {
        AssignStmt::cast(self.syntax().parent()?)
    }

    /// An `update`'s attributes and their new values.
    #[must_use]
    pub fn assignments(&self) -> Option<AssignList> {
        self.syntax().children().find_map(AssignList::cast)
    }
}

impl Load {
    /// `value`, before `from`, if the array takes the values of the
    /// relation's only attribute rather than its tuples.
    #[must_use]
    pub fn value(&self) -> Option<SyntaxToken> {
        self.word("value")
    }

    /// `from`, before the relation.
    #[must_use]
    pub fn from(&self) -> Option<SyntaxToken> {
        self.word("from")
    }

    /// Its contextual keyword `word`, in any case.
    fn word(&self, word: &str) -> Option<SyntaxToken> {
        significant_tokens(&self.0)
            .find(|t| t.kind() == SyntaxKind::Ident && t.text().eq_ignore_ascii_case(word))
    }

    /// `order`, before the order items.
    #[must_use]
    pub fn order_keyword(&self) -> Option<SyntaxToken> {
        self.word("order")
    }

    /// The order items, in brackets.
    #[must_use]
    pub fn order(&self) -> Option<OrderList> {
        self.0.children().find_map(OrderList::cast)
    }

    /// `remove {x}` or `keep {x}`, after the order: the attributes the
    /// elements leave out, or keep. A load has one at most, which the
    /// parser checks.
    pub fn keepings(&self) -> impl Iterator<Item = Keeping> + use<> {
        self.0.children().filter_map(Keeping::cast)
    }

    /// The span to delete to remove its order, with what follows it, and
    /// the whitespace before.
    #[must_use]
    pub fn order_removal(&self) -> Option<Span> {
        let word = self.order_keyword()?;
        let end = self
            .keepings()
            .last()
            .map(|k| k.span().end)
            .or_else(|| Some(self.order()?.span().end))
            .unwrap_or_else(|| span(word.text_range()).end);
        Some(Span::new(preceded_from(&word.into()), end))
    }

    /// `as x`, after an array loaded into a relation.
    #[must_use]
    pub fn element_name(&self) -> Option<ElementName> {
        self.0.children().find_map(ElementName::cast)
    }

    /// `numbered n`, after an array loaded into a relation.
    #[must_use]
    pub fn numbering(&self) -> Option<Numbering> {
        self.0.children().find_map(Numbering::cast)
    }

    /// `where c`: the tuples the load keeps.
    #[must_use]
    pub fn condition(&self) -> Option<LoadCondition> {
        self.0.children().find_map(LoadCondition::cast)
    }

    /// Where its relation or array ends, which the clauses after it follow:
    /// the expression after its target.
    #[must_use]
    pub fn source_end(&self) -> Option<usize> {
        let source = self.0.children().filter_map(Expr::cast).nth(1)?;
        Some(span(source.syntax().text_range()).end)
    }
}

impl ElementName {
    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// The span to delete to remove it, with the whitespace before.
    #[must_use]
    pub fn removal(&self) -> Span {
        Span::new(preceded_from(&self.0.clone().into()), self.span().end)
    }
}

impl Numbering {
    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// The span to delete to remove it, with the whitespace before.
    #[must_use]
    pub fn removal(&self) -> Span {
        Span::new(preceded_from(&self.0.clone().into()), self.span().end)
    }
}

impl LoadCondition {
    /// The word `where`.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    #[must_use]
    pub fn condition(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    /// The span to delete to remove it, with the whitespace before.
    #[must_use]
    pub fn removal(&self) -> Span {
        Span::new(preceded_from(&self.0.clone().into()), self.span().end)
    }
}

impl Keeping {
    /// The word: `remove` or `keep`, or another language's, such as `drop`,
    /// or none, for a projection's braces alone.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// Whether it names the attributes kept, rather than those left out:
    /// `keep`, and another language's word for it, or none.
    #[must_use]
    pub fn keeps(&self) -> bool {
        self.keyword()
            .is_none_or(|k| k.kind() != SyntaxKind::RemoveKw)
            && self
                .keyword()
                .is_none_or(|k| !k.text().eq_ignore_ascii_case("drop"))
    }

    /// The attributes, `{x, y}`.
    #[must_use]
    pub fn names(&self) -> Option<NameList> {
        self.0.children().find_map(NameList::cast)
    }

    /// The span to delete to remove it, with the whitespace before.
    #[must_use]
    pub fn removal(&self) -> Span {
        Span::new(preceded_from(&self.0.clone().into()), self.span().end)
    }
}

impl OrderList {
    pub fn items(&self) -> impl Iterator<Item = OrderItem> {
        self.0.children().filter_map(OrderItem::cast)
    }

    /// The span to delete to remove `item` from the list, with a comma that
    /// separates it from a neighbor.
    #[must_use]
    pub fn removal(&self, item: &OrderItem) -> Span {
        list_removal(item.syntax())
    }
}

impl OrderItem {
    /// `asc` or `desc`.
    #[must_use]
    pub fn direction(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .find(|t| t.kind() == SyntaxKind::Ident)
    }

    /// Whether the direction is `desc`.
    #[must_use]
    pub fn descending(&self) -> bool {
        self.direction().is_some_and(|t| t.text() == "desc")
    }

    /// The attribute ordered by.
    #[must_use]
    pub fn attribute(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }
}

impl CallStmt {
    /// The operator called.
    #[must_use]
    pub fn callee(&self) -> Option<NameRef> {
        self.0.children().find_map(NameRef::cast)
    }

    /// The unit's name and `::` before the operator's, if it is qualified.
    #[must_use]
    pub fn qualifier(&self) -> Option<Qualifier> {
        self.0.children().find_map(Qualifier::cast)
    }

    #[must_use]
    pub fn arguments(&self) -> Option<Arguments> {
        self.0.children().find_map(Arguments::cast)
    }
}

impl CallExpr {
    /// The operator called.
    #[must_use]
    pub fn callee(&self) -> Option<NameRef> {
        self.0.children().find_map(NameRef::cast)
    }

    /// The unit's name and `::` before the operator's, if it is qualified.
    #[must_use]
    pub fn qualifier(&self) -> Option<Qualifier> {
        self.0.children().find_map(Qualifier::cast)
    }

    #[must_use]
    pub fn arguments(&self) -> Option<Arguments> {
        self.0.children().find_map(Arguments::cast)
    }
}

impl Arguments {
    /// The named arguments, `name: value`, or a name alone, `name`, which is
    /// its own value.
    pub fn named(&self) -> impl Iterator<Item = AttributeValue> {
        self.0.children().filter_map(AttributeValue::cast)
    }

    pub fn positional(&self) -> impl Iterator<Item = PositionalArg> {
        self.0.children().filter_map(PositionalArg::cast)
    }

    /// Where each argument is written, in the order they are, if none says
    /// which parameter it is for apart from its value: each a value, or a
    /// name alone.
    #[must_use]
    pub fn unnamed_in_order(&self) -> Option<Vec<Span>> {
        self.0
            .children()
            .filter_map(|child| {
                if let Some(arg) = PositionalArg::cast(child.clone()) {
                    return Some(Some(arg.span()));
                }
                let arg = AttributeValue::cast(child)?;
                Some(arg.is_shorthand().then(|| arg.span()))
            })
            .collect()
    }

    /// Where a new argument goes, as an offset and the text to put around
    /// `name: value`: after the last argument, named or not, keeping any
    /// trailing comma trailing.
    #[must_use]
    pub fn insertion(&self) -> (usize, &'static str, &'static str) {
        let last = self.0.children().filter(|c| {
            matches!(
                c.kind(),
                SyntaxKind::AttributeValue | SyntaxKind::PositionalArg
            )
        });
        list_insertion(&self.0, last.last())
    }

    /// The span to delete to remove `argument`, with its comma.
    #[must_use]
    pub fn removal(&self, argument: &AttributeValue) -> Span {
        list_removal(argument.syntax())
    }
}

impl PositionalArg {
    #[must_use]
    pub fn value(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

impl UnaryExpr {
    /// The operator: `not`, or a foreign spelling of it such as `!`.
    #[must_use]
    pub fn operator(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    #[must_use]
    pub fn operand(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

impl BinaryExpr {
    /// The operator: a keyword such as `and`, or a foreign spelling of one.
    /// For `not matching`, its first word.
    #[must_use]
    pub fn operator(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The operator's words: one, or two for `not matching`.
    pub fn operators(&self) -> impl Iterator<Item = SyntaxToken> {
        significant_tokens(&self.0)
    }

    #[must_use]
    pub fn lhs(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    #[must_use]
    pub fn rhs(&self) -> Option<Expr> {
        self.0.children().filter_map(Expr::cast).nth(1)
    }
}

impl IsExpr {
    /// The value tested.
    #[must_use]
    pub fn operand(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    /// The word `is`, or another language's spelling of the test.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The type the value is tested for.
    #[must_use]
    pub fn type_expr(&self) -> Option<TypeExpr> {
        self.0.children().find_map(TypeExpr::cast)
    }
}

impl FailExpr {
    /// The word `fail`, or another language's, as `throw`.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The error it fails with.
    #[must_use]
    pub fn error(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

impl FailStmt {
    /// The word `fail`, or another language's, as `throw`.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The error it fails with.
    #[must_use]
    pub fn error(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

impl WarnStmt {
    /// The word `warn`.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The error it warns with.
    #[must_use]
    pub fn error(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

impl TreatExpr {
    /// The word `treat`, or another language's spelling of it.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The value treated.
    #[must_use]
    pub fn operand(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    /// The type it is treated as.
    #[must_use]
    pub fn type_expr(&self) -> Option<TypeExpr> {
        self.0.children().find_map(TypeExpr::cast)
    }
}

impl ParenExpr {
    #[must_use]
    pub fn inner(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

impl IndexExpr {
    /// The array, before the brackets.
    #[must_use]
    pub fn operand(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    /// The position, in the brackets.
    #[must_use]
    pub fn index(&self) -> Option<Expr> {
        self.0.children().filter_map(Expr::cast).nth(1)
    }

    /// The `[` that opens the brackets.
    #[must_use]
    pub fn l_bracket(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).find(|t| t.kind() == SyntaxKind::LBracket)
    }

    /// The brackets and the position in them, from the `[` to the end.
    #[must_use]
    pub fn brackets(&self) -> Option<Span> {
        let open = self.l_bracket()?;
        Some(Span::new(span(open.text_range()).start, self.span().end))
    }
}

impl IfExpr {
    /// The `if`, or the word another language writes for it, which is
    /// reported: Python's `elif` in an `else` branch.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The condition, between `if` and `then`.
    #[must_use]
    pub fn condition(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    /// The value if the condition is true, between `then` and `else`.
    #[must_use]
    pub fn then_branch(&self) -> Option<Expr> {
        self.0.children().filter_map(Expr::cast).nth(1)
    }

    /// The value if the condition is false, after `else`.
    #[must_use]
    pub fn else_branch(&self) -> Option<Expr> {
        self.0.children().filter_map(Expr::cast).nth(2)
    }
}

impl ExtractExpr {
    #[must_use]
    pub fn operand(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    /// The attribute's name, after the `.`.
    #[must_use]
    pub fn attribute(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }
}

impl ProjectExpr {
    #[must_use]
    pub fn operand(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    #[must_use]
    pub fn names(&self) -> Option<NameList> {
        self.0.children().find_map(NameList::cast)
    }
}

impl MethodCallExpr {
    /// `x.f`, the value and the name called as its method.
    #[must_use]
    pub fn method(&self) -> Option<ExtractExpr> {
        self.0.children().find_map(ExtractExpr::cast)
    }

    #[must_use]
    pub fn arguments(&self) -> Option<Arguments> {
        self.0.children().find_map(Arguments::cast)
    }
}

impl PostfixExpr {
    /// The operator: `remove`, `rename` or `extend`.
    #[must_use]
    pub fn operator(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    #[must_use]
    pub fn operand(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    #[must_use]
    pub fn names(&self) -> Option<NameList> {
        self.0.children().find_map(NameList::cast)
    }

    #[must_use]
    pub fn renamings(&self) -> Option<RenameList> {
        self.0.children().find_map(RenameList::cast)
    }

    /// The gatherings of a `group` or a `wrap`.
    #[must_use]
    pub fn gatherings(&self) -> Option<GatherList> {
        self.0.children().find_map(GatherList::cast)
    }

    /// The attribute assignments of an `extend` or a `replace`.
    #[must_use]
    pub fn assignments(&self) -> Option<AssignList> {
        self.0.children().find_map(AssignList::cast)
    }
}

impl GatherList {
    pub fn gatherings(&self) -> impl Iterator<Item = Gathering> {
        self.0.children().filter_map(Gathering::cast)
    }
}

impl Gathering {
    /// The new attribute's name.
    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// The attributes it gathers.
    #[must_use]
    pub fn names(&self) -> Option<NameList> {
        self.0.children().find_map(NameList::cast)
    }
}

impl AssignList {
    pub fn assignments(&self) -> impl Iterator<Item = Assign> {
        self.0.children().filter_map(Assign::cast)
    }

    /// The span to delete to remove `assign` from the list, with a comma
    /// that separates it from a neighbor.
    #[must_use]
    pub fn removal(&self, assign: &Assign) -> Span {
        list_removal(assign.syntax())
    }
}

impl Assign {
    /// The attribute's name.
    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// Its value.
    #[must_use]
    pub fn value(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

impl NaryExpr {
    /// The operator: `join`, `times` or `compose`.
    #[must_use]
    pub fn operator(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    pub fn operands(&self) -> impl Iterator<Item = Expr> {
        self.0.children().filter_map(Expr::cast)
    }
}

impl ImageExpr {
    /// The operator, `!!`.
    #[must_use]
    pub fn operator(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The relation whose tuples are matched.
    #[must_use]
    pub fn operand(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

/// What a `from` expression takes from a relation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Taken {
    /// `tuple from r`: its only tuple.
    Tuple,
    /// `value from r`: the only attribute's value of its only tuple.
    Value,
}

impl FromExpr {
    /// What it takes: a tuple, after `tuple`, or a value, after `value`.
    #[must_use]
    pub fn taken(&self) -> Taken {
        match significant_tokens(&self.0).next().map(|t| t.kind()) {
            Some(SyntaxKind::TupleKw) => Taken::Tuple,
            _ => Taken::Value,
        }
    }

    /// The words before the operand: `tuple from` or `value from`.
    pub fn keywords(&self) -> impl Iterator<Item = SyntaxToken> {
        significant_tokens(&self.0)
    }

    /// The relation it takes from.
    #[must_use]
    pub fn operand(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

impl RenameList {
    pub fn renamings(&self) -> impl Iterator<Item = Renaming> {
        self.0.children().filter_map(Renaming::cast)
    }

    /// The span to delete to remove `renaming` from the list, with a comma
    /// that separates it from a neighbor.
    #[must_use]
    pub fn removal(&self, renaming: &Renaming) -> Span {
        list_removal(renaming.syntax())
    }
}

impl Renaming {
    /// The attribute's name before renaming: the one before `as`.
    #[must_use]
    pub fn before(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// Its name after renaming: the one after `as`.
    #[must_use]
    pub fn after(&self) -> Option<Name> {
        self.0.children().filter_map(Name::cast).nth(1)
    }
}

impl NameList {
    pub fn names(&self) -> impl Iterator<Item = Name> {
        self.0.children().filter_map(Name::cast)
    }

    /// Where a new item goes, as for [`Arguments::insertion`].
    #[must_use]
    pub fn insertion(&self) -> (usize, &'static str, &'static str) {
        list_insertion(&self.0, self.names().last().map(|n| n.0))
    }

    /// The span to delete to remove `name` from the list, with a comma that
    /// separates it from a neighbor.
    #[must_use]
    pub fn removal(&self, name: &Name) -> Span {
        list_removal(name.syntax())
    }
}

impl NameRef {
    #[must_use]
    pub fn ident(&self) -> Option<SyntaxToken> {
        ident_of(&self.0)
    }
}

impl QualifiedName {
    /// The name, after the unit's.
    #[must_use]
    pub fn ident(&self) -> Option<SyntaxToken> {
        ident_of(&self.0)
    }

    /// The unit's name and `::` before the name.
    #[must_use]
    pub fn qualifier(&self) -> Option<Qualifier> {
        self.0.children().find_map(Qualifier::cast)
    }
}

impl VarDecl {
    /// The word it starts with, `var` or `const`.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// Whether it declares a constant, which is never assigned, rather than a
    /// variable.
    #[must_use]
    pub fn is_const(&self) -> bool {
        self.keyword()
            .is_some_and(|keyword| keyword.kind() == SyntaxKind::ConstKw)
    }

    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// The database it adds the relvar to, `kitchen.` before its name, if
    /// it is one's.
    #[must_use]
    pub fn part_of(&self) -> Option<PartOf> {
        self.0.children().find_map(PartOf::cast)
    }

    /// The declared type, from `: type`.
    #[must_use]
    pub fn type_expr(&self) -> Option<TypeExpr> {
        self.0
            .children()
            .find(|n| n.kind() == SyntaxKind::TypeAnnotation)?
            .children()
            .find_map(TypeExpr::cast)
    }

    /// The initial value, from `:= value`.
    #[must_use]
    pub fn initializer(&self) -> Option<Expr> {
        self.0
            .children()
            .find(|n| n.kind() == SyntaxKind::Initializer)?
            .children()
            .find_map(Expr::cast)
    }

    /// The keys, `key { a, b }`, in source order.
    pub fn keys(&self) -> impl Iterator<Item = Key> {
        self.0.children().filter_map(Key::cast)
    }
}

impl Key {
    /// The words before the names: `key`, or another language's spelling,
    /// such as SQL's `primary key` or `unique`, which the parser reports.
    pub fn keywords(&self) -> impl Iterator<Item = SyntaxToken> {
        significant_tokens(&self.0).filter(|t| t.kind() == SyntaxKind::Ident)
    }

    /// The attributes, `{ a, b }`.
    #[must_use]
    pub fn names(&self) -> Option<NameList> {
        self.0.children().find_map(NameList::cast)
    }

    /// The span to delete to remove the key, with the whitespace before it,
    /// up to the end of what comes before.
    #[must_use]
    pub fn removal(&self) -> Span {
        Span::new(preceded_from(&self.0.clone().into()), self.span().end)
    }

    /// The span to delete to remove the key from a list it is an item of,
    /// as a type's braces are: with the whitespace before it, and the comma
    /// after it, or, if none follows, the one before it.
    #[must_use]
    pub fn item_removal(&self) -> Span {
        let significant = |e: &SyntaxElement| !e.kind().is_trivia();
        let element: SyntaxElement = self.0.clone().into();
        let after = std::iter::successors(element.next_sibling_or_token(), |e| {
            e.next_sibling_or_token()
        })
        .find(significant)
        .filter(|e| e.kind() == SyntaxKind::Comma);
        if let Some(comma) = after {
            return Span::new(self.removal().start, span(comma.text_range()).end);
        }
        let before = std::iter::successors(element.prev_sibling_or_token(), |e| {
            e.prev_sibling_or_token()
        })
        .find(significant)
        .filter(|e| e.kind() == SyntaxKind::Comma);
        match before {
            Some(comma) => Span::new(span(comma.text_range()).start, self.span().end),
            None => self.removal(),
        }
    }
}

/// Where the trivia before `element` starts: the end of the sibling before
/// it that isn't trivia, or, if there is none, its own start.
fn preceded_from(element: &SyntaxElement) -> usize {
    let own = span(element.text_range()).start;
    std::iter::successors(
        element.prev_sibling_or_token(),
        SyntaxElement::prev_sibling_or_token,
    )
    .find(|e| !e.kind().is_trivia())
    .map_or(own, |e| span(e.text_range()).end)
}

/// A type as written: a name, a tuple type, a relation type, an array type,
/// or what a type parameter can be.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeExpr {
    Named(TypeRef),
    Tuple(TupleType),
    Relation(RelationType),
    Array(ArrayType),
    Any(AnyType),
}

impl TypeExpr {
    #[must_use]
    pub fn cast(node: SyntaxNode) -> Option<Self> {
        TypeRef::cast(node.clone())
            .map(TypeExpr::Named)
            .or_else(|| TupleType::cast(node.clone()).map(TypeExpr::Tuple))
            .or_else(|| RelationType::cast(node.clone()).map(TypeExpr::Relation))
            .or_else(|| ArrayType::cast(node.clone()).map(TypeExpr::Array))
            .or_else(|| AnyType::cast(node).map(TypeExpr::Any))
    }

    #[must_use]
    pub fn syntax(&self) -> &SyntaxNode {
        match self {
            TypeExpr::Named(t) => t.syntax(),
            TypeExpr::Tuple(t) => t.syntax(),
            TypeExpr::Relation(t) => t.syntax(),
            TypeExpr::Array(t) => t.syntax(),
            TypeExpr::Any(t) => t.syntax(),
        }
    }

    #[must_use]
    pub fn span(&self) -> Span {
        span(self.syntax().text_range())
    }
}

/// An expression.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Expr {
    Literal(Literal),
    Tuple(TupleLiteral),
    Relation(RelationLiteral),
    Array(ArrayLiteral),
    Name(NameRef),
    Qualified(QualifiedName),
    Unary(UnaryExpr),
    Binary(BinaryExpr),
    Is(IsExpr),
    Treat(TreatExpr),
    Fail(FailExpr),
    Paren(ParenExpr),
    Extract(ExtractExpr),
    Index(IndexExpr),
    If(IfExpr),
    Project(ProjectExpr),
    Postfix(PostfixExpr),
    Nary(NaryExpr),
    Image(ImageExpr),
    From(FromExpr),
    Call(CallExpr),
    MethodCall(MethodCallExpr),
    Block(ValueBlock),
}

impl Expr {
    #[must_use]
    pub fn cast(node: SyntaxNode) -> Option<Self> {
        Literal::cast(node.clone())
            .map(Expr::Literal)
            .or_else(|| TupleLiteral::cast(node.clone()).map(Expr::Tuple))
            .or_else(|| RelationLiteral::cast(node.clone()).map(Expr::Relation))
            .or_else(|| ArrayLiteral::cast(node.clone()).map(Expr::Array))
            .or_else(|| NameRef::cast(node.clone()).map(Expr::Name))
            .or_else(|| QualifiedName::cast(node.clone()).map(Expr::Qualified))
            .or_else(|| UnaryExpr::cast(node.clone()).map(Expr::Unary))
            .or_else(|| BinaryExpr::cast(node.clone()).map(Expr::Binary))
            .or_else(|| IsExpr::cast(node.clone()).map(Expr::Is))
            .or_else(|| TreatExpr::cast(node.clone()).map(Expr::Treat))
            .or_else(|| FailExpr::cast(node.clone()).map(Expr::Fail))
            .or_else(|| ParenExpr::cast(node.clone()).map(Expr::Paren))
            .or_else(|| ExtractExpr::cast(node.clone()).map(Expr::Extract))
            .or_else(|| IndexExpr::cast(node.clone()).map(Expr::Index))
            .or_else(|| IfExpr::cast(node.clone()).map(Expr::If))
            .or_else(|| ProjectExpr::cast(node.clone()).map(Expr::Project))
            .or_else(|| PostfixExpr::cast(node.clone()).map(Expr::Postfix))
            .or_else(|| NaryExpr::cast(node.clone()).map(Expr::Nary))
            .or_else(|| ImageExpr::cast(node.clone()).map(Expr::Image))
            .or_else(|| FromExpr::cast(node.clone()).map(Expr::From))
            .or_else(|| CallExpr::cast(node.clone()).map(Expr::Call))
            .or_else(|| MethodCallExpr::cast(node.clone()).map(Expr::MethodCall))
            .or_else(|| ValueBlock::cast(node).map(Expr::Block))
    }

    #[must_use]
    pub fn syntax(&self) -> &SyntaxNode {
        match self {
            Expr::Literal(e) => e.syntax(),
            Expr::Tuple(e) => e.syntax(),
            Expr::Relation(e) => e.syntax(),
            Expr::Array(e) => e.syntax(),
            Expr::Name(e) => e.syntax(),
            Expr::Qualified(e) => e.syntax(),
            Expr::Unary(e) => e.syntax(),
            Expr::Binary(e) => e.syntax(),
            Expr::Is(e) => e.syntax(),
            Expr::Treat(e) => e.syntax(),
            Expr::Fail(e) => e.syntax(),
            Expr::Paren(e) => e.syntax(),
            Expr::Extract(e) => e.syntax(),
            Expr::Index(e) => e.syntax(),
            Expr::If(e) => e.syntax(),
            Expr::Project(e) => e.syntax(),
            Expr::Postfix(e) => e.syntax(),
            Expr::Nary(e) => e.syntax(),
            Expr::Image(e) => e.syntax(),
            Expr::From(e) => e.syntax(),
            Expr::Call(e) => e.syntax(),
            Expr::MethodCall(e) => e.syntax(),
            Expr::Block(e) => e.syntax(),
        }
    }

    #[must_use]
    pub fn span(&self) -> Span {
        span(self.syntax().text_range())
    }
}

impl Heading {
    pub fn attributes(&self) -> impl Iterator<Item = AttributeType> {
        self.0.children().filter_map(AttributeType::cast)
    }

    /// Where a new attribute goes, as for [`Arguments::insertion`].
    #[must_use]
    pub fn insertion(&self) -> (usize, &'static str, &'static str) {
        list_insertion(&self.0, self.attributes().last().map(|a| a.0))
    }

    /// The span to delete to remove `attribute`, with its comma.
    #[must_use]
    pub fn removal(&self, attribute: &AttributeType) -> Span {
        list_removal(attribute.syntax())
    }

    /// Every type written by name in it, nested ones too, in source order.
    pub fn type_refs(&self) -> impl Iterator<Item = TypeRef> + use<> {
        self.0.descendants().filter_map(TypeRef::cast)
    }
}

impl TupleType {
    #[must_use]
    pub fn heading(&self) -> Option<HeadingExpr> {
        self.0.children().find_map(HeadingExpr::cast)
    }
}

impl RelationType {
    #[must_use]
    pub fn heading(&self) -> Option<HeadingExpr> {
        self.0.children().find_map(HeadingExpr::cast)
    }
}

/// A heading as written after `relation` or `tuple`: its attributes, or the
/// name of a declared heading.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HeadingExpr {
    Written(Heading),
    Named(HeadingRef),
}

impl HeadingExpr {
    #[must_use]
    pub fn cast(node: SyntaxNode) -> Option<Self> {
        Heading::cast(node.clone())
            .map(HeadingExpr::Written)
            .or_else(|| HeadingRef::cast(node).map(HeadingExpr::Named))
    }

    #[must_use]
    pub fn syntax(&self) -> &SyntaxNode {
        match self {
            HeadingExpr::Written(h) => h.syntax(),
            HeadingExpr::Named(h) => h.syntax(),
        }
    }

    #[must_use]
    pub fn span(&self) -> Span {
        span(self.syntax().text_range())
    }

    /// Where its attributes are written: here, or in the declaration of
    /// the heading it names.
    #[must_use]
    pub fn attributes_written(&self) -> Option<Heading> {
        match self {
            HeadingExpr::Written(heading) => Some(heading.clone()),
            HeadingExpr::Named(named) => named.declaration()?.heading(),
        }
    }
}

impl HeadingRef {
    /// The name, after the qualifier, if it has one.
    #[must_use]
    pub fn ident(&self) -> Option<SyntaxToken> {
        ident_of(&self.0)
    }

    /// The unit's name and `::` before the name, if it is qualified.
    #[must_use]
    pub fn qualifier(&self) -> Option<Qualifier> {
        self.0.children().find_map(Qualifier::cast)
    }

    /// Where the attributes of the heading it names are written, for
    /// labels and layout: its own file's declaration, which hides any
    /// other's, unless the name is qualified, or else that of the first of
    /// its program's other files that declares it, as the tree has no
    /// units' names to tell them apart by, nor scopes, which sema keeps.
    #[must_use]
    pub fn declaration(&self) -> Option<HeadingDecl> {
        let name = self.ident()?;
        let own = self.0.ancestors().find_map(SourceFile::cast)?;
        let mine = self.qualifier().is_none().then(|| own.heading(name.text()));
        mine.flatten().or_else(|| {
            let program = Program::cast(self.0.ancestors().last()?)?;
            program
                .files()
                .filter(|file| *file != own)
                .find_map(|file| file.heading(name.text()))
        })
    }
}

impl AttributeType {
    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// Whether its name is `name`.
    #[must_use]
    pub fn is_named(&self, name: &str) -> bool {
        self.name()
            .and_then(|n| n.ident())
            .is_some_and(|i| i.text() == name)
    }

    #[must_use]
    pub fn type_expr(&self) -> Option<TypeExpr> {
        self.0.children().find_map(TypeExpr::cast)
    }

    /// `per r`, after a parameter's type.
    #[must_use]
    pub fn per(&self) -> Option<Per> {
        self.0.children().find_map(Per::cast)
    }

    /// The keys after its type, which a type's relation-valued component
    /// can have, and nothing else, as the parser reports.
    pub fn keys(&self) -> impl Iterator<Item = Key> {
        self.0.children().filter_map(Key::cast)
    }

    /// `var`, `const` or `out`, before a parameter's name, or another
    /// language's mode, before the name or the type, which the parser
    /// reports.
    #[must_use]
    pub fn mode(&self) -> Option<Mode> {
        self.0.children().find_map(Mode::cast)
    }
}

/// What a mode says of a parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Said {
    /// `const`: the operator only reads its argument's value.
    Const,
    /// `var`: the operator can read and assign its variable.
    Var,
    /// `out`: the operator assigns its variable before it reads it.
    Out,
}

impl Mode {
    /// The word, `var`, `const` or `out`, or another language's, such as
    /// `ref`.
    /// Ada's `in out` is two.
    pub fn words(&self) -> impl Iterator<Item = SyntaxToken> {
        significant_tokens(&self.0)
    }

    /// What it says, as written or as another language writes it: `const`,
    /// as Ada's `in` does too, `out`, or else `var`.
    #[must_use]
    pub fn says(&self) -> Said {
        let words: Vec<String> = self.words().map(|w| w.text().to_lowercase()).collect();
        match words.as_slice() {
            [word] if word == "const" || word == "in" => Said::Const,
            [word] if word == "out" => Said::Out,
            _ => Said::Var,
        }
    }

    /// The span to delete to remove it, with the whitespace after it, up
    /// to the name it is before.
    #[must_use]
    pub fn removal(&self) -> Span {
        let end = std::iter::successors(
            self.0.next_sibling_or_token(),
            SyntaxElement::next_sibling_or_token,
        )
        .find(|e| !e.kind().is_trivia())
        .map_or(self.span().end, |e| span(e.text_range()).start);
        Span::new(self.span().start, end)
    }
}

impl Per {
    /// The word `per`.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The parameter named after `per`, whose argument's tuples the
    /// argument is evaluated for.
    #[must_use]
    pub fn relation(&self) -> Option<NameRef> {
        self.0.children().find_map(NameRef::cast)
    }

    /// The span to delete to remove it, with the whitespace before.
    #[must_use]
    pub fn removal(&self) -> Span {
        Span::new(preceded_from(&self.0.clone().into()), self.span().end)
    }
}

impl TupleLiteral {
    /// The heading written before the attributes, as in
    /// `tuple { a: integer } { a: 1 }` or `tuple home { … }`.
    #[must_use]
    pub fn heading(&self) -> Option<HeadingExpr> {
        self.0.children().find_map(HeadingExpr::cast)
    }

    pub fn attributes(&self) -> impl Iterator<Item = AttributeValue> {
        self.0.children().filter_map(AttributeValue::cast)
    }

    /// The span to delete to remove `attribute`, including the comma that
    /// separates it from a neighbor.
    #[must_use]
    pub fn removal(&self, attribute: &AttributeValue) -> Span {
        list_removal(attribute.syntax())
    }
}

impl ArrayType {
    /// The elements' type, after `array`.
    #[must_use]
    pub fn element(&self) -> Option<TypeExpr> {
        self.0.children().find_map(TypeExpr::cast)
    }
}

impl ArrayLiteral {
    /// The `array` keyword, which a bare `[` lacks.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).find(|t| t.kind() == SyntaxKind::ArrayKw)
    }

    /// The elements' type, written before them, as in `array integer []`.
    #[must_use]
    pub fn element_type(&self) -> Option<TypeExpr> {
        self.0.children().find_map(TypeExpr::cast)
    }

    /// The elements, in order.
    pub fn elements(&self) -> impl Iterator<Item = Expr> {
        self.0.children().filter_map(Expr::cast)
    }
}

impl RelationLiteral {
    /// The heading written before the tuples, as in
    /// `relation { a: integer } {}` or `relation home {}`.
    #[must_use]
    pub fn heading(&self) -> Option<HeadingExpr> {
        self.0.children().find_map(HeadingExpr::cast)
    }

    /// The tuples: tuple selectors, or any expressions of tuples.
    pub fn elements(&self) -> impl Iterator<Item = Expr> {
        self.0.children().filter_map(Expr::cast)
    }

    /// The span to delete to remove `element`, including the comma that
    /// separates it from a neighbor.
    #[must_use]
    pub fn removal(&self, element: &Expr) -> Span {
        list_removal(element.syntax())
    }
}

/// Where a new element goes in the braced list `container`, whose last
/// element is `last_item`: an offset and the text to put around it. After the
/// last element, keeping any trailing comma trailing; or just inside the `{`.
fn list_insertion(
    container: &SyntaxNode,
    last_item: Option<SyntaxNode>,
) -> (usize, &'static str, &'static str) {
    let Some(last_item) = last_item else {
        // The container's own `{` is a token child; a heading's is inside it.
        let after_brace = significant_tokens(container)
            .find(|t| t.kind() == SyntaxKind::LBrace)
            .map_or_else(
                || span(container.text_range()).end,
                |t| span(t.text_range()).end,
            );
        // Laid out as the formatter lays it out: `tuple { a: 1 }`, but
        // `f{a: 1}`.
        let around = if crate::spaced_braces(container) {
            " "
        } else {
            ""
        };
        return (after_brace, around, around);
    };
    match next_significant(&last_item) {
        Some(comma) if comma.kind() == SyntaxKind::Comma => {
            (span(comma.text_range()).end, " ", ",")
        }
        _ => (span(last_item.text_range()).end, ", ", ""),
    }
}

/// The span to delete to remove a comma-separated list element, including
/// the comma that separates it from a neighbor.
fn list_removal(item: &SyntaxNode) -> Span {
    let own = span(item.text_range());
    if let Some(comma) = next_significant(item).filter(|t| t.kind() == SyntaxKind::Comma) {
        // Up to the next element, so its spacing stays as it was.
        let end = comma
            .next_token()
            .filter(|t| t.kind() == SyntaxKind::Whitespace)
            .map_or_else(
                || span(comma.text_range()).end,
                |ws| span(ws.text_range()).end,
            );
        return Span::new(own.start, end);
    }
    if let Some(comma) = previous_significant(item).filter(|t| t.kind() == SyntaxKind::Comma) {
        return Span::new(span(comma.text_range()).start, own.end);
    }
    own
}

fn significant_tokens(node: &SyntaxNode) -> impl Iterator<Item = SyntaxToken> {
    node.children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .filter(|t| !t.kind().is_trivia())
}

/// The next sibling token of `node` that isn't trivia.
fn next_significant(node: &SyntaxNode) -> Option<SyntaxToken> {
    std::iter::successors(
        node.next_sibling_or_token(),
        SyntaxElement::next_sibling_or_token,
    )
    .find(|e| !e.kind().is_trivia())?
    .into_token()
}

/// The previous sibling token of `node` that isn't trivia.
fn previous_significant(node: &SyntaxNode) -> Option<SyntaxToken> {
    std::iter::successors(
        node.prev_sibling_or_token(),
        SyntaxElement::prev_sibling_or_token,
    )
    .find(|e| !e.kind().is_trivia())?
    .into_token()
}

impl AttributeValue {
    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// Whether its name is `name`.
    #[must_use]
    pub fn is_named(&self, name: &str) -> bool {
        self.name()
            .and_then(|n| n.ident())
            .is_some_and(|i| i.text() == name)
    }

    /// `var`, `const` or `out`, before an argument's name, or another
    /// language's mode, before the name or the value, which the parser
    /// reports.
    #[must_use]
    pub fn mode(&self) -> Option<Mode> {
        self.0.children().find_map(Mode::cast)
    }

    /// Whether it is a name alone, `a`, which is `a: a`.
    #[must_use]
    pub fn is_shorthand(&self) -> bool {
        self.0.children().find_map(Expr::cast).is_none()
            && !significant_tokens(&self.0).any(|token| {
                matches!(
                    token.kind(),
                    SyntaxKind::Colon | SyntaxKind::Eq | SyntaxKind::ColonEq
                )
            })
    }

    /// The value: as written after its name, or, for a name alone, the
    /// name's own.
    #[must_use]
    pub fn value(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast).or_else(|| {
            self.is_shorthand()
                .then(|| self.name().map(|name| name.value()))
                .flatten()
        })
    }
}

impl TypeRef {
    /// The name, after the qualifier, if it has one.
    #[must_use]
    pub fn ident(&self) -> Option<SyntaxToken> {
        ident_of(&self.0)
    }

    /// The unit's name and `::` before the name, if it is qualified.
    #[must_use]
    pub fn qualifier(&self) -> Option<Qualifier> {
        self.0.children().find_map(Qualifier::cast)
    }
}

/// The type or heading `node` names, if it is a reference to one, a
/// `TypeRef` or a `HeadingRef`: its qualifier, if it has one, and its name.
#[must_use]
pub fn type_reference(node: &SyntaxNode) -> Option<(Option<Qualifier>, SyntaxToken)> {
    if let Some(r) = TypeRef::cast(node.clone()) {
        return Some((r.qualifier(), r.ident()?));
    }
    let r = HeadingRef::cast(node.clone())?;
    Some((r.qualifier(), r.ident()?))
}

impl Qualifier {
    /// The name of the unit whose declaration the name is.
    #[must_use]
    pub fn unit(&self) -> Option<UnitName> {
        self.0.children().find_map(UnitName::cast)
    }
}

impl AnyType {
    /// The word `any`.
    #[must_use]
    pub fn any(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).find(|t| t.kind() == SyntaxKind::Ident && t.text() == "any")
    }

    /// The word for what it can be: `type`, `tuple` or `relation`, which
    /// follows `any`, or, as the parser reports, comes before it.
    #[must_use]
    pub fn what(&self) -> Option<SyntaxToken> {
        let any = self.any()?;
        significant_tokens(&self.0).find(|t| *t != any)
    }
}

impl TypeParameters {
    /// The word `types`, or how it is cased otherwise.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The type parameters, `{ T: any type }`: a heading.
    #[must_use]
    pub fn heading(&self) -> Option<Heading> {
        self.0.children().find_map(Heading::cast)
    }

    /// The span to delete to remove them, with the whitespace before.
    #[must_use]
    pub fn removal(&self) -> Span {
        Span::new(preceded_from(&self.0.clone().into()), self.span().end)
    }
}

/// What a literal stands for.
#[derive(Debug, Clone, PartialEq)]
pub enum LiteralValue {
    Boolean(bool),
    Integer(i64),
    Approximate(f64),
    Text(String),
    Character(String),
    Byte(u8),
    Bytes(Vec<u8>),
}

/// Why a literal has no value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiteralError {
    /// An integer literal that doesn't fit in 64 bits.
    IntegerTooLarge,
    /// An approximate literal too large to represent.
    ApproximateTooLarge,
    /// A literal the lexer already reported as malformed.
    Malformed,
}

impl Literal {
    #[must_use]
    pub fn token(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .find(|t| t.kind().is_literal())
    }

    /// # Errors
    ///
    /// Returns why the literal has no value.
    pub fn value(&self) -> Result<LiteralValue, LiteralError> {
        let token = self.token().ok_or(LiteralError::Malformed)?;
        let text = token.text();
        match token.kind() {
            SyntaxKind::TrueKw => return Ok(LiteralValue::Boolean(true)),
            SyntaxKind::FalseKw => return Ok(LiteralValue::Boolean(false)),
            SyntaxKind::IntLiteral => {
                literal::check_int(text).map_err(|_| LiteralError::Malformed)?;
                return literal::int_value(text)
                    .map(LiteralValue::Integer)
                    .ok_or(LiteralError::IntegerTooLarge);
            }
            SyntaxKind::ApproximateLiteral => {
                literal::check_approximate(text).map_err(|_| LiteralError::Malformed)?;
                return literal::approximate_value(text)
                    .map(LiteralValue::Approximate)
                    .ok_or(LiteralError::ApproximateTooLarge);
            }
            _ => {}
        }
        if token.kind() == SyntaxKind::ByteLiteral {
            let bytes = text
                .strip_prefix("x'")
                .and_then(|t| t.strip_suffix('\''))
                .and_then(|body| literal::hex_bytes(body).ok())
                .ok_or(LiteralError::Malformed)?;
            return match *bytes.as_slice() {
                [byte] => Ok(LiteralValue::Byte(byte)),
                _ => Err(LiteralError::Malformed),
            };
        }
        if token.kind() == SyntaxKind::BytesLiteral {
            return text
                .strip_prefix("x\"")
                .and_then(|t| t.strip_suffix('"'))
                .and_then(|body| literal::hex_bytes(body).ok())
                .map(LiteralValue::Bytes)
                .ok_or(LiteralError::Malformed);
        }
        if token.kind() == SyntaxKind::CharacterLiteral {
            let (value, problems) = text
                .strip_prefix('\'')
                .and_then(|t| t.strip_suffix('\''))
                .filter(|_| text.len() >= 2)
                .map(literal::decode_character)
                .ok_or(LiteralError::Malformed)?;
            return if problems.is_empty() && trendy_text::is_one_character(&value) {
                Ok(LiteralValue::Character(value))
            } else {
                Err(LiteralError::Malformed)
            };
        }
        let body = text
            .strip_prefix('"')
            .and_then(|t| t.strip_suffix('"'))
            .filter(|_| text.len() >= 2)
            .ok_or(LiteralError::Malformed)?;
        let (value, problems) = literal::decode_text(body);
        if problems.is_empty() {
            Ok(LiteralValue::Text(value))
        } else {
            Err(LiteralError::Malformed)
        }
    }
}

/// Whether a name spelled `name` is written anywhere in `node`, other than as
/// what the value after it is for, as in `name: value`.
#[must_use]
pub fn mentions(node: &SyntaxNode, name: &str) -> bool {
    let labels = |token: &SyntaxToken| {
        token
            .parent()
            .filter(|parent| parent.kind() == SyntaxKind::Name)
            .and_then(|parent| parent.parent())
            .and_then(AttributeValue::cast)
            .is_some_and(|attribute| !attribute.is_shorthand())
    };
    node.descendants_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .any(|token| token.kind() == SyntaxKind::Ident && token.text() == name && !labels(&token))
}

fn ident_of(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .find(|t| t.kind() == SyntaxKind::Ident)
}

impl Name {
    #[must_use]
    pub fn ident(&self) -> Option<SyntaxToken> {
        ident_of(&self.0)
    }

    /// The value of what is named as it is, where a name alone gives its own
    /// value: an attribute written `a`, which is `a: a`, or an argument of a
    /// call the parser took for a projection, `f{a}`.
    #[must_use]
    pub fn value(&self) -> Expr {
        Expr::Name(NameRef(self.0.clone()))
    }
}
