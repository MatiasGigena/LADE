//! LADE's domain: the values, invariants and allowed transitions of Objectives, their Execution,
//! Feedback, Inspection, Review and restart Recovery.
//!
//! The crate has no platform dependencies: no I/O, clock, storage or protocol types. Callers pass
//! timestamps and identifiers in, and a refused transition returns a [`DomainError`]. Use-case
//! orchestration lives in `lade-system`'s service, which calls these transitions directly.

pub mod domain;
pub mod error;

#[cfg(test)]
mod testing;

pub use domain::ids::Revision;
pub use error::DomainError;
