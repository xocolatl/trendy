# trendy

A compiled relational language, inspired by Tutorial D, with a compiler
written in Rust. The language design emerges as it is implemented, so the
project moves slowly on purpose.

## Reference material

The writings this language is based on are in
`~/Projects/thegresqlpost-references/`. Check them before making design
decisions, and cite them when explaining one. Where a folder has an
`INDEX.md`, read that first and load only the chapters you need. The
`.txt` files are cheaper to read than the PDFs.

The primary sources are by C. J. Date and Hugh Darwen:

- `date-2014/`: *Databases, Types, and the Relational Model: The Third
  Manifesto* (Date & Darwen, 3rd ed.), one text file per chapter. It is the
  foundation for this project. Chapter 5 defines Tutorial D, which this
  language deliberately departs from in syntax but not in model. Appendix I
  has the Tutorial D grammar.
- `date-2016/`: *Type Inheritance and Relational Theory* (Date).
- `date-2015-codd-papers/`: Date's critical analysis of Codd's first
  relational papers.
- `darwen-1992.md`: Darwen, "Outer Join with No Nulls and Fewer Tears".

The other folders hold supporting material, such as Codd's 1969 and 1970
papers and a survey of nulls.

## No technical debt

Prefer a large refactor over taking on technical debt. When a change doesn't
fit the current structure, restructure first, even across many crates, rather
than adding a workaround, special case, `TODO`, `#[allow(...)]`, or a shortcut
that crosses a layer boundary. There's no deadline that justifies a shortcut.
If the right fix is big, say so and do it, and don't propose the quick version
as an alternative.

## Diagnostics are a primary interface

LLMs are expected to write most trendy code, so error messages must be
actionable without guesswork. Every diagnostic needs a stable code with an
explanation, a primary span where the fix goes, the relevant language rule
in a note, and a machine-applicable suggestion whenever the fix is
mechanical. When adding syntax or semantics, design its errors in the same
change, including recognition of habits from other languages. The rules are
in the "Diagnostics" section of `docs/pipeline.md`.

## Conventions

- `docs/language.md` is the language reference. Update it with every language
  change; its examples are compiled by `crates/trendyc/tests/explanations.rs`.
- Built-ins (types and operators) are declared in
  `crates/prelude/src/prelude.trendy`, listed in `hir::Builtin`, and usually
  implemented in `crates/runtime`. Sema checks the prelude against the
  compiler, so all three must change together.
- `docs/pipeline.md` defines the compiler layers. Its `layers` block is
  enforced by `crates/trendyc/tests/layers.rs`, so a dependency change means
  editing the doc.
- The pre-commit hook (`.githooks/pre-commit`) requires:
  - rustfmt-clean code;
  - zero warnings from rustc, clippy (including `clippy::pedantic`), and
    rustdoc;
  - every file in `examples/` and `projects/` in canonical form
    (`trendyfmt --check`);
  - the generated TextMate grammar up to date;
  - a clean strict TypeScript check of the VS Code extension.

  Fix lints rather than silencing them.
- Set up a fresh clone with
  `ln -s ../../.githooks/pre-commit .git/hooks/pre-commit` and
  `npm ci --prefix editors/vscode`.
- Editor setup: `cargo install --locked --path crates/lsp` puts `trendy-lsp` on
  `PATH`, and `npm run install-extension --prefix editors/vscode` packages and
  installs the VS Code extension. Rerun the relevant one after changing the
  server or the extension, then reload the VS Code window.
- `editors/vscode/syntaxes/trendy.tmLanguage.json` is generated; after
  changing tokens, run `UPDATE_TEXTMATE=1 cargo test -p trendy_lsp --test textmate`.
  Real highlighting comes from the language server's semantic tokens.
- `trendy_fmt` defines the language's canonical layout. When the syntax
  changes, update the formatter in the same change.
