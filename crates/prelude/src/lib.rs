//! The prelude: trendy source that is part of every program. It declares the
//! built-in operators, which the compiler implements, and defines others in
//! trendy.
//!
//! Drivers parse it alongside the program and hand both to sema. Only the
//! prelude may declare `builtin` operators, and sema checks each declaration
//! against the operator the compiler actually provides.

/// The prelude's source text.
pub const SOURCE: &str = include_str!("prelude.trendy");

/// The name shown for the prelude in internal error messages.
pub const PATH: &str = "<prelude>";

/// Where the prelude's source was when the compiler was built: the
/// language server checks the file there as the prelude, not a program.
pub const SOURCE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/prelude.trendy");
