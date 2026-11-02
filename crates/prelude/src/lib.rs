//! The units that come with the compiler, trendy source that is part of
//! every program. The prelude declares the built-in operators, which the
//! compiler implements, and defines others in trendy; the catalog
//! ([`catalog`]) describes the program's units, and its constraints are the
//! rules between them.
//!
//! Drivers parse them alongside the program and hand them all to sema. Only
//! the prelude may declare `builtin` operators, and sema checks each
//! declaration against the operator the compiler actually provides.

/// The prelude's source text.
pub const SOURCE: &str = include_str!("prelude.trendy");

/// The name shown for the prelude in internal error messages.
pub const PATH: &str = "<prelude>";

/// The prelude's unit's name, under `trendy.`, which is kept for the units
/// that come with the compiler.
pub const NAME: &str = "trendy.prelude";

/// Where the prelude's source was when the compiler was built: the
/// language server checks the file there as the prelude, not a program.
pub const SOURCE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/prelude.trendy");

/// The catalog, `trendy.catalog`: the database that describes a program's
/// units, which the compiler fills as it checks them, and whose constraints
/// are the rules between units.
pub mod catalog {
    /// The catalog's source text.
    pub const SOURCE: &str = include_str!("catalog.trendy");

    /// The name shown for the catalog in internal error messages.
    pub const PATH: &str = "<catalog>";

    /// The catalog's unit's name.
    pub const NAME: &str = "trendy.catalog";

    /// Where the catalog's source was when the compiler was built: the
    /// language server checks the file there as the catalog, not a program.
    pub const SOURCE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/catalog.trendy");
}
