//! The LADE service (`laded`): everything that runs on the developer's machine on behalf of the
//! desktop client.
//!
//! - [`service`] is the application layer: one module per capability that applies the domain
//!   transitions of `lade-core`, persists them and answers the client protocol.
//! - [`storage`] keeps the SQLite store, its migrations and pre-migration backups.
//! - [`supervision`] launches pi workers and terminals in sandboxes and runs their egress
//!   gateways and credential custody.
//! - [`adapters`] are the remaining external seams: the artifact store, Git, the LADE profile,
//!   provider facts and the bubblewrap sandbox.
//! - [`transport`] admits client links on the control socket and frames their traffic.
//! - [`support`], [`diagnostics`] and [`fault`] hold shared primitives, the service log and test
//!   fault injection.
//!
//! Lower modules never depend on [`service`]; `bin/laded.rs` is the composition root.

pub mod adapters {
    pub mod artifacts;
    pub mod git;
    pub mod profile;
    pub mod providers;
    pub mod sandbox;
}
pub mod diagnostics;
pub mod fault;
pub mod service;
pub mod storage;
pub mod supervision;
pub mod support;
pub mod transport;

/// The repository fixtures shared with the integration tests, for unit tests in this crate.
#[cfg(test)]
#[path = "../tests/adapters_support/mod.rs"]
mod adapters_support;
