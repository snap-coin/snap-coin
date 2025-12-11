use argon2::{Argon2, Params};
use bincode::{Decode, Encode};
use ed25519_dalek::SigningKey;
use ed25519_dalek::ed25519::Error;
use ed25519_dalek::ed25519::signature::SignerMut;
use ed25519_dalek::{Signature as DalekSignature, VerifyingKey};
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::Deref;

use keys::{Private, Public};

/// Public / Private key logic
pub mod keys;

/// Argon2 configuration, includes magic bytes (the salt for hashing)
pub struct Argon2Config {
    pub memory_cost: u32,
    pub time_cost: u32,
    pub parallelism: u32,
    pub output_length: Option<usize>,
    pub algorithm: argon2::Algorithm,
    pub version: argon2::Version,
    pub magic_bytes: [u8; 10]
}

/// The currently used, blockchain argon2 config
pub const ARGON2_CONFIG: Argon2Config = Argon2Config {
    memory_cost: 8 * 1024,
    time_cost: 1,
    parallelism: 2,
    output_length: Some(32),
    algorithm: argon2::Algorithm::Argon2id,
    version: argon2::Version::V0x13,
    magic_bytes: [0xCD, 0xC6, 0x3B, 0xAF, 0x5E, 0x52, 0xE0, 0x9, 0x72, 0xAD]
};

// WARNING: SLOW
pub fn argon2_hash(input: &[u8]) -> [u8; 32] {
    let params = Params::new(ARGON2_CONFIG.memory_cost, ARGON2_CONFIG.time_cost, ARGON2_CONFIG.parallelism, ARGON2_CONFIG.output_length).unwrap();
    let argon2 = Argon2::new(ARGON2_CONFIG.algorithm, ARGON2_CONFIG.version, params);
    let mut hash = [0u8; 32];
    argon2
        .hash_password_into(input, &ARGON2_CONFIG.magic_bytes, &mut hash)
        .unwrap();
    hash
}

/// Store and hash Argon2 hashes (compare too)
/// When used in in hashmaps, the already hashed argon 2 digest gets re-hashed, for speed
#[derive(Clone, Copy, PartialEq, Eq, Encode, Decode, std::hash::Hash)]
pub struct Hash([u8; 32]);

impl Hash {
    /// Create a new hash by hashing some data
    /// WARNING: SLOW
    pub fn new(data: &[u8]) -> Self {
        Hash(argon2_hash(data))
    }

    /// Create a new hash with a buffer of an already existing hash
    pub const fn new_from_buf(hash_buf: [u8; 32]) -> Self {
        Hash(hash_buf)
    }

    /// Compare this hash with some data (check if the data hash is the same)
    pub fn compare_with_data(&self, other_data: &[u8]) -> bool {
        let computed = argon2_hash(other_data);
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
        Self::new_from_base36(&s).ok_or_else(|| serde::de::Error::custom("Invalid base36 signature"))
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
