//! LADE System Layer library: host adapters, the product store, client admission and the
//! authoritative `laded` service.
//!
//! The adapters own every interaction with Git, the content-addressed artifact store and the
//! bubblewrap sandbox. They carry no protocol or domain coupling; the service maps their typed
//! results onto the wire and domain types.

pub mod adapters {
    pub mod artifacts;
    pub mod git;
    pub mod sandbox;
}
pub mod service;
pub mod storage;
pub mod supervision;
pub mod transport;
