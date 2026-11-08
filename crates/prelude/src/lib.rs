//! The units that come with the compiler, trendy source that is part of
//! every program: the prelude ([`PRELUDE`]), which every unit sees, and
//! which declares the built-in types and most built-in operators, which the
//! compiler implements, and defines others in trendy; the catalog
//! ([`CATALOG`]), which describes the program's units, and whose
//! constraints are the rules between them; `trendy.files` ([`FILES`]), which
//! reads files; `trendy.uuid` ([`UUID`]) and `trendy.time` ([`TIME`]), of
//! ordinary trendy but for reading the clock and giving random bits; and
//! `trendy.process` ([`PROCESS`]), the arguments and environment the program
//! was started with, for the units that use them.
//!
//! Drivers parse them, as [`UNITS`] lists them, alongside the program and
//! hand them all to sema. Only these units may declare `builtin`
//! operators and variables, and sema checks each declaration against what
//! the compiler actually provides.

/// A unit that comes with the compiler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unit {
    /// Its name, under `trendy.`, which is kept for the units that come
    /// with the compiler.
    pub name: &'static str,
    /// The name shown for it, as for a file, in errors and failures.
    pub path: &'static str,
    /// Its source text.
    pub source: &'static str,
    /// Where its source was when the compiler was built: the language
    /// server checks the file there as this unit, not as a program.
    pub source_path: &'static str,
}

/// The prelude, `trendy.prelude`, whose declarations every unit sees.
pub const PRELUDE: Unit = Unit {
    name: "trendy.prelude",
    path: "<prelude>",
    source: include_str!("prelude.trendy"),
    source_path: concat!(env!("CARGO_MANIFEST_DIR"), "/src/prelude.trendy"),
};

/// The catalog, `trendy.catalog`: the database that describes a program's
/// units, which the compiler fills as it checks them, and whose constraints
/// are the rules between units.
pub const CATALOG: Unit = Unit {
    name: "trendy.catalog",
    path: "<catalog>",
    source: include_str!("catalog.trendy"),
    source_path: concat!(env!("CARGO_MANIFEST_DIR"), "/src/catalog.trendy"),
};

/// `trendy.files`, which reads text and CSV files, and declares the errors
/// only reading one gives.
pub const FILES: Unit = Unit {
    name: "trendy.files",
    path: "<files>",
    source: include_str!("files.trendy"),
    source_path: concat!(env!("CARGO_MANIFEST_DIR"), "/src/files.trendy"),
};

/// `trendy.uuid`, RFC 9562's universally unique identifiers.
pub const UUID: Unit = Unit {
    name: "trendy.uuid",
    path: "<uuid>",
    source: include_str!("uuid.trendy"),
    source_path: concat!(env!("CARGO_MANIFEST_DIR"), "/src/uuid.trendy"),
};

/// `trendy.time`, dates, times of day, instants and durations.
pub const TIME: Unit = Unit {
    name: "trendy.time",
    path: "<time>",
    source: include_str!("time.trendy"),
    source_path: concat!(env!("CARGO_MANIFEST_DIR"), "/src/time.trendy"),
};

/// `trendy.process`, the arguments and environment the program was started
/// with.
pub const PROCESS: Unit = Unit {
    name: "trendy.process",
    path: "<process>",
    source: include_str!("process.trendy"),
    source_path: concat!(env!("CARGO_MANIFEST_DIR"), "/src/process.trendy"),
};

/// Every unit that comes with the compiler, in the order their texts
/// follow a program's units' in its spans: the prelude last.
pub const UNITS: [Unit; 6] = [PROCESS, TIME, UUID, FILES, CATALOG, PRELUDE];
