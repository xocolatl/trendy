//! The prelude: trendy source that is part of every program. It declares the
//! built-in operators, which the compiler implements.
//!
//! Drivers parse it alongside the program and hand both to sema. Only the
//! prelude may declare `builtin` operators, and sema checks each declaration
//! against the operator the compiler actually provides.

/// The prelude's source text.
pub const SOURCE: &str = include_str!("prelude.trendy");

/// The name shown for the prelude in internal error messages.
pub const PATH: &str = "<prelude>";
