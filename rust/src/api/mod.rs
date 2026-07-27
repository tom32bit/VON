//! The Dart-facing API.
//!
//! `flutter_rust_bridge` generates bindings from every public function in this
//! module tree, so this is the whole contract between the two languages —
//! nothing below `api` is reachable from Dart.
//!
//! Conventions:
//! * Fallible calls return `Result<T, VonException>`, whose error carries a
//!   stable `kind`. Spelled out rather than aliased — see api/types.rs.
//! * Long-running work streams progress through a `StreamSink` rather than
//!   blocking a Dart `await`.
//! * Nothing here blocks the Flutter UI thread: `flutter_rust_bridge` dispatches
//!   these onto its own worker pool.

pub mod agent_api;
pub mod device;
pub mod files;
pub mod init;
pub mod models;
pub mod preview;
pub mod projects;
pub mod types;

pub use types::VonException;
