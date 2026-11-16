//! Local identities: 8 base62 characters allocated by Boundrail, qualified as `<model>#<local>`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use sha2::{Digest, Sha256};

/// The length of a local ID.
pub const LEN: usize = 8;
const ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const BASE: u64 = ALPHABET.len() as u64;
const ATTEMPTS: usize = 64;

pub fn is_local(id: &str) -> bool {
    id.len() == LEN && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

pub fn qualify(model: &str, local: &str) -> String {
    format!("{model}#{local}")
}

/// The local part of `subject` when it is qualified with `model`, else `subject` itself.
pub fn local<'a>(model: &str, subject: &'a str) -> &'a str {
    subject
        .strip_prefix(model)
        .and_then(|rest| rest.strip_prefix('#'))
        .unwrap_or(subject)
}

/// A fresh local ID that `taken` (existing and retired IDs) does not hold. Module files are named
/// by ID and collide on case-insensitive file systems, so `taken` is asked about the candidate in
/// ASCII lowercase and must answer for the lowercase forms of the IDs it holds.
pub fn allocate(taken: impl Fn(&str) -> bool) -> Result<String> {
    for _ in 0..ATTEMPTS {
        let id = candidate();
        if !taken(&id.to_ascii_lowercase()) {
            return Ok(id);
        }
    }
    bail!("could not allocate an unused ID after {ATTEMPTS} attempts")
}

fn candidate() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut hasher = Sha256::new();
    hasher.update(nanos.to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(COUNTER.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    let digest = hasher.finalize();
    // A SHA-256 digest is 32 bytes, so its first 8 always form a u64.
    let mut n = u64::from_le_bytes(digest[..8].try_into().expect("32-byte digest"));
    (0..LEN)
        .map(|_| {
            // 62^8 < 2^64, so eight base62 digits come from one u64.
            let digit = ALPHABET[(n % BASE) as usize];
            n /= BASE;
            char::from(digit)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn allocated_ids_are_eight_base62_characters_and_distinct() {
        let a = allocate(|_| false).unwrap();
        let b = allocate(|_| false).unwrap();
        assert!(is_local(&a), "{a}");
        assert!(is_local(&b), "{b}");
        assert_ne!(a, b);
        assert_eq!(qualify("Model123", &a), format!("Model123#{a}"));
    }

    #[test]
    fn allocation_retries_past_taken_ids() {
        let calls = Cell::new(0);
        let rejected = RefCell::new(Vec::new());
        let id = allocate(|id| {
            calls.set(calls.get() + 1);
            if calls.get() <= 3 {
                rejected.borrow_mut().push(id.to_owned());
                true
            } else {
                false
            }
        })
        .unwrap();
        assert_eq!(calls.get(), 4);
        assert!(!rejected.borrow().contains(&id));
        assert!(is_local(&id));
    }

    #[test]
    fn allocation_asks_about_the_lowercase_candidate() {
        let asked = RefCell::new(String::new());
        let id = allocate(|candidate| {
            asked.replace(candidate.to_owned());
            false
        })
        .unwrap();
        assert_eq!(*asked.borrow(), id.to_ascii_lowercase());
    }

    #[test]
    fn allocation_gives_up_when_every_candidate_is_taken() {
        assert!(allocate(|_| true).is_err());
    }

    #[test]
    fn local_id_format() {
        assert!(is_local("abcDEF12"));
        assert!(!is_local("abcDEF1"));
        assert!(!is_local("abcDEF1_"));
        assert!(!is_local("abcDEF123"));
    }
}
