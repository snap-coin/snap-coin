use std::{fmt, ops::Deref};

use bincode::{Decode, Encode};
use ed25519_dalek::SigningKey;
use num_bigint::BigUint;
use rand::Rng;
use serde::{Deserialize, Serialize};

/// Store a public key, and use it to verify signatures
#[derive(Encode, Decode, Clone, Copy, PartialEq)]
pub struct Public {
    public: [u8; 32],
}

impl Public {
    /// Create a new public key from an existing key, encoded in a base36 string
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

        Some(Public { public: buf })
    }

    /// Create a new public key from an existing keys (direct) buffer
    pub const fn new_from_buf(buf: &[u8; 32]) -> Self {
        Public { public: *buf }
    }

    /// Get the keys raw buffer
    pub fn dump_buf(&self) -> &[u8; 32] {
        &self.public
    }

    pub fn dump_base36(&self) -> String {
        let big_int = BigUint::from_bytes_be(&self.public);
        big_int.to_str_radix(36)
    }
}

impl Serialize for Public {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.dump_base36())
    }
}

impl<'de> Deserialize<'de> for Public {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::new_from_base36(&s).ok_or_else(|| serde::de::Error::custom("Invalid base36 public key"))
    }
}

impl fmt::Debug for Public {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Public: {}", self.dump_base36())
    }
}

impl Deref for Public {
    type Target = [u8; 32];
    fn deref(&self) -> &Self::Target {
        &self.public
    }
}

/// Store a private key, and use it to verify signatures or derive a public key
#[derive(Encode, Decode, Clone, Copy, PartialEq)]
pub struct Private {
    private: [u8; 32],
}

impl Private {
    /// Create a new random private key
    pub fn new_random() -> Self {
        Private {
            private: rand::rng().random(),
        }
    }

    /// Create a new private key from an existing key, encoded in a base36 string
    pub fn new_from_base36(s: &str) -> Option<Private> {
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

        Some(Private { private: buf })
    }

    /// Create a new private key from an existing keys (direct) buffer
    pub const fn new_from_buf(buf: &[u8; 32]) -> Option<Private> {
        Some(Private { private: *buf })
    }

    /// Get the keys raw buffer
    pub fn dump_buf(&self) -> &[u8; 32] {
        &self.private
    }

    pub fn dump_base36(&self) -> String {
        let big_int = BigUint::from_bytes_be(&self.private);
        big_int.to_str_radix(36)
    }

    /// Derive this private keys public key
    pub fn to_public(&self) -> Public {
        let signing_key = SigningKey::from_bytes(&self.private);
        Public::new_from_buf(signing_key.verifying_key().as_bytes())
    }
}

impl Deref for Private {
    type Target = [u8; 32];
    fn deref(&self) -> &Self::Target {
        &self.private
    }
}

impl fmt::Debug for Private {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Private: {}", self.dump_base36())
    }
}
