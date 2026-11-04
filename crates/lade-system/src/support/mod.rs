//! Primitives shared by every layer of the service crate: clock, randomness, hex encoding and
//! private-directory handling. Nothing here knows about requests, storage rows or the wire.

pub mod fs;
pub mod process;

use std::fmt::Write as _;
use std::fs::File;
use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};

use lade_core::domain::ids::Uuid;
use lade_protocol::types::Timestamp;

#[must_use]
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The wire timestamp for `ms` milliseconds since the Unix epoch, or the epoch itself when `ms`
/// is negative or past year 9999.
///
/// # Panics
///
/// Never: the epoch is always representable.
#[must_use]
pub fn wire_ts(ms: i64) -> Timestamp {
    Timestamp::from_unix_ms(ms)
        .or_else(|| Timestamp::from_unix_ms(0))
        .expect("epoch is representable")
}

/// Fills `buf` from the kernel's random source.
///
/// # Errors
///
/// Returns the error from opening or reading `/dev/urandom`.
pub fn random_fill(buf: &mut [u8]) -> std::io::Result<()> {
    File::open("/dev/urandom")?.read_exact(buf)
}

/// A fresh random v4 UUID from the kernel's random source.
///
/// # Panics
///
/// Panics when `/dev/urandom` cannot be read. Every identifier and capability the service
/// mints depends on it, so there is no safe way to continue without it.
#[must_use]
pub fn new_uuid() -> Uuid {
    let mut bytes = [0u8; 16];
    random_fill(&mut bytes).expect("kernel random source unavailable");
    Uuid::from_random_bytes(bytes)
}

#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

#[must_use]
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_comparison() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[test]
    fn hex_is_lowercase_and_padded() {
        assert_eq!(hex(&[0x00, 0x0f, 0xab]), "000fab");
    }
}
