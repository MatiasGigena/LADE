//! Platform-free LADE domain core: typed identities, Objective lifecycle, execution generations, Contextual
//! Feedback, frozen review and approval rules, plus small use cases over narrow ports.
//!
//! No I/O, clock, randomness, Git, SQL, GTK or worker types: time and identities are supplied by callers.

pub mod application;
pub mod domain;
pub mod error;
pub mod ports;

#[cfg(test)]
mod testing;

pub use domain::ids::Revision;
pub use error::DomainError;
