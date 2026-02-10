use bincode::{Decode, Encode};
use ed25519_dalek::SigningKey;
use ed25519_dalek::ed25519::Error;
use ed25519_dalek::ed25519::signature::SignerMut;
use ed25519_dalek::{Signature as DalekSignature, VerifyingKey};
use num_bigint::BigUint;
use randomx_rs::{RandomXCache, RandomXDataset, RandomXFlag, RandomXVM};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::fmt;
use std::ops::Deref;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use keys::{Private, Public};

/// Public / Private key logic
pub mod keys;

/// Merkle tree
pub mod merkle_tree;

/// Address inclusion filter
pub mod address_inclusion_filter;

pub const RANDOMX_SEED: &[u8] = b"snap-coin-testnet";

/// Wrapper for the dataset to assert Sync manually
#[derive(Clone)]
struct SharedDataset(RandomXDataset);

// SAFETY: RandomXDataset is immutable after creation, safe for concurrent reads
unsafe impl Sync for SharedDataset {}
unsafe impl Send for SharedDataset {}

static DATASET: OnceLock<SharedDataset> = OnceLock::new();
static IS_LIGHT_MODE: AtomicBool = AtomicBool::new(true);
static HUGE_PAGES: AtomicBool = AtomicBool::new(false);

/// This can only be called at the beginning of a program to be effective (before all Hash::new() or Hash::compare_with_data() calls to work)
/// Enables full memory mode, substantially increasing hash rate, by allocating a 2GB scratch pad for hashing
pub fn randomx_optimized_mode(huge_pages: bool) {
    IS_LIGHT_MODE.store(false, Ordering::SeqCst);
    HUGE_PAGES.store(huge_pages, Ordering::SeqCst);
}

fn has_huge_pages_support() -> bool {
    // Linux check
    #[cfg(target_os = "linux")]
    {
        // Check if transparent huge pages are enabled

        use std::fs;

        // Check if huge pages are configured
        if let Ok(pages) = fs::read_to_string("/proc/sys/vm/nr_hugepages") {
            if let Ok(count) = pages.trim().parse::<u32>() {
                if count >= 2000 {
                    // absolute minimum is 2GB
                    return true;
                }
            }
        }
    }

    // Windows check
    #[cfg(target_os = "windows")]
    {
        // On Windows, large pages require the SeLockMemoryPrivilege
        // You could check this, but it's complex. For now, assume available.
        return true;
    }

    // macOS doesn't support huge pages in the same way
    #[cfg(target_os = "macos")]
    {
        return false;
    }

    false
}

/// Returns a reference to the shared dataset
fn get_dataset() -> RandomXDataset {
    let dataset = DATASET.get_or_init(|| {
        let mut flags = RandomXFlag::get_recommended_flags()
            | RandomXFlag::FLAG_FULL_MEM
            | RandomXFlag::FLAG_JIT;
        if HUGE_PAGES.load(Ordering::SeqCst) {
            if has_huge_pages_support() {
                flags |= RandomXFlag::FLAG_LARGE_PAGES;
                println!("[RANDOMX] Full page support detected");
            } else {
                println!("[RANDOMX] Full page support was not detected or configured correctly with 2000 pages!");
            }
        }
        println!("[RANDOMX] Starting optimized RandomX: {:?}", flags);

        println!("[RANDOMX] Creating dataset...");
        let cache = RandomXCache::new(flags, RANDOMX_SEED).expect(&format!(
            "Failed to create RandomX cache{}",
            if HUGE_PAGES.load(Ordering::SeqCst) {
                " (does your system support huge pages?)"
            } else {
                ""
            }
        ));

        let dataset =
            RandomXDataset::new(flags, cache, 0).expect("Failed to create RandomX dataset");

        let shared_dataset = SharedDataset(dataset);
        println!("[RANDOMX] Dataset created!");
        shared_dataset
    });
    dataset.clone().0
}

// Thread-local VM
thread_local! {
    static THREAD_VM: RefCell<RandomXVM> = RefCell::new({
        if IS_LIGHT_MODE.load(Ordering::SeqCst) {
            let flags = RandomXFlag::FLAG_JIT;
            let cache = RandomXCache::new(flags, RANDOMX_SEED).expect("Failed to create RandomX cache (light mode)");
            RandomXVM::new(flags, Some(cache), None)
                .expect("Failed to create RandomX VM (light mode)")
        } else {
            let mut flags = RandomXFlag::get_recommended_flags()
                | RandomXFlag::FLAG_FULL_MEM
                | RandomXFlag::FLAG_JIT;
            if HUGE_PAGES.load(Ordering::SeqCst) {
                if has_huge_pages_support() {
                    flags |= RandomXFlag::FLAG_LARGE_PAGES;
                }
            }
            let dataset = get_dataset();
            RandomXVM::new(flags, None, Some(dataset))
                .expect("Failed to create RandomX VM (full mode)")
        }
    });
}

pub fn randomx_hash(input: &[u8]) -> [u8; 32] {
    THREAD_VM.with(|vm_cell| {
        let vm = vm_cell.borrow_mut();
        unsafe {
            let hash_vec = vm.calculate_hash(input).unwrap_unchecked();
            *(hash_vec.as_ptr() as *const [u8; 32])
        }
    })
}

/// Store and hash Argon2 hashes (compare too)
/// When used in in hashmaps, the already hashed argon 2 digest gets re-hashed, for speed
#[derive(Clone, Copy, PartialEq, Eq, Encode, Decode, std::hash::Hash)]
pub struct Hash([u8; 32]);

impl Hash {
    /// Create a new hash by hashing some data
    /// WARNING: SLOW
    pub fn new(data: &[u8]) -> Self {
        Hash(randomx_hash(data))
    }

    /// Create a new hash with a buffer of an already existing hash
    pub const fn new_from_buf(hash_buf: [u8; 32]) -> Self {
        Hash(hash_buf)
    }

    /// Compare this hash with some data (check if the data hash is the same)
    pub fn compare_with_data(&self, other_data: &[u8]) -> bool {
        let computed = randomx_hash(other_data);
        computed == self.0
    }

    /// Create a new hash from a already existing hash encoded in a base36 string
    pub fn new_from_base36(s: &str) -> Option<Self> {
        // Convert base36 string to bytes
        let big_int = BigUint::parse_bytes(s.as_bytes(), 36)?;
        let mut buf = big_int.to_bytes_be();

        // Ensure the buffer is exactly 32 bytes
        if buf.len() > 32 {
            return None;
        } else if buf.len() < 32 {
            // Pad with zeros at the front
            let mut padded = vec![0u8; 32 - buf.len()];
            padded.extend(buf);
            buf = padded;
        }

        // Convert Vec<u8> to [u8; 32]
        let buf: [u8; 32] = buf.try_into().ok()?;

        Some(Hash(buf))
    }

    pub fn dump_base36(&self) -> String {
        let big_int = BigUint::from_bytes_be(&self.0);
        big_int.to_str_radix(36)
    }

    pub fn dump_buf(&self) -> [u8; 32] {
        self.0
    }
}

impl Serialize for Hash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.dump_base36())
    }
}

impl<'de> Deserialize<'de> for Hash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::new_from_base36(&s).ok_or_else(|| serde::de::Error::custom("Invalid base36 hash"))
    }
}

impl Deref for Hash {
    type Target = [u8; 32];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Hash: {}", self.dump_base36())
    }
}

/// Store sign and verify ed25519
#[derive(Clone, PartialEq, Eq, Encode, Decode, Copy, Hash)]
pub struct Signature([u8; 64]);

impl Signature {
    /// Create a signature by signing some data
    pub fn new_signature(private: &mut Private, data: &[u8]) -> Self {
        let mut key = SigningKey::from_bytes(private.dump_buf());
        let signature = key.sign(data);
        Signature(signature.to_bytes()) // [u8; 64]
    }

    /// Create a signature with the raw signature bytes (no signing)
    pub fn new_from_buf(signature: &[u8; 64]) -> Self {
        Signature(signature.clone())
    }

    /// Validate a signature and return true if valid
    pub fn validate_with_public(&self, public: &Public, data: &[u8]) -> Result<bool, Error> {
        let key = VerifyingKey::from_bytes(public.dump_buf())?;
        Ok(key
            .verify_strict(data, &DalekSignature::from_bytes(&self.0))
            .is_ok())
    }

    /// Validate a signature and return true if valid
    pub fn validate_with_private(&self, private: &Private, data: &[u8]) -> Result<bool, Error> {
        let key = SigningKey::from_bytes(private.dump_buf());
        Ok(key
            .verify_strict(data, &DalekSignature::from_bytes(&self.0))
            .is_ok())
    }

    /// Create a signature from a base 36 string
    pub fn new_from_base36(s: &str) -> Option<Self> {
        // Convert base36 string to bytes
        let big_int = BigUint::parse_bytes(s.as_bytes(), 36)?;
        let mut buf = big_int.to_bytes_be();

        // Ensure the buffer is exactly 64 bytes
        if buf.len() > 64 {
            return None;
        } else if buf.len() < 64 {
            // Pad with zeros at the front
            let mut padded = vec![0u8; 64 - buf.len()];
            padded.extend(buf);
            buf = padded;
        }

        // Convert Vec<u8> to [u8; 64]
        let buf: [u8; 64] = buf.try_into().ok()?;

        Some(Self(buf))
    }

    pub fn dump_base36(&self) -> String {
        let big_int = BigUint::from_bytes_be(&self.0);
        big_int.to_str_radix(36)
    }

    pub fn dump_buf(&self) -> [u8; 64] {
        self.0
    }
}

impl Serialize for Signature {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.dump_base36())
    }
}

impl<'de> Deserialize<'de> for Signature {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::new_from_base36(&s)
            .ok_or_else(|| serde::de::Error::custom("Invalid base36 signature"))
    }
}

impl Deref for Signature {
    type Target = [u8; 64]; // match the array size
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Signature: {}", self.dump_base36())
    }
}
