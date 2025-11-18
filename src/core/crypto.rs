use argon2::{Argon2, Params};
use bincode::{Decode, Encode};
use ed25519_dalek::ed25519::Error;
use num_bigint::BigUint;
use std::ops::Deref;
use ed25519_dalek::{ed25519::signature::SignerMut};
use ed25519_dalek::{Signature as DalekSignature, VerifyingKey};
use ed25519_dalek::SigningKey;

use crate::core::keys::{Private, Public};

pub const MAGIC_BYTES: [u8; 10] = [205, 198, 59, 175, 94, 82, 224, 9, 114, 173];

pub fn argon2_hash(input: &[u8]) -> [u8; 32] { // WARNING: SLOW
  let params = Params::new(8 * 1024, 1, 2, Some(32)).unwrap();
  let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
  let mut hash = [0u8; 32];
  argon2.hash_password_into(input, &MAGIC_BYTES, &mut hash).unwrap();
  hash
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, std::hash::Hash)]
pub struct Hash(pub [u8; 32]);

impl Hash {
  pub fn new(data: &[u8]) -> Self {
    Hash(argon2_hash(data))
  }

  pub fn compare_with_data(&self, other_data: &[u8]) -> bool {
    let computed = argon2_hash(other_data);
    computed == self.0
  }

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
}

impl Deref for Hash {
  type Target = [u8; 32];
  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Copy)]
pub struct Signature(
  pub [u8; 64]
);

impl Signature {
  pub fn new_singature(private: &mut Private, data: &[u8]) -> Self {
    let mut key = SigningKey::from_bytes(private.dump_buf());
    let signature = key.sign(data);
    Signature(signature.to_bytes()) // [u8; 64]
  }

  pub fn new_from_buf(signature: &[u8; 64]) -> Self {
    Signature(signature.clone())
  }

  pub fn validate_with_public(&self, public: &Public, data: &[u8]) -> Result<bool, Error> {
    let key = VerifyingKey::from_bytes(public.dump_buf())?;
    Ok(key.verify_strict(data, &DalekSignature::from_bytes(&self.0)).is_ok())
  }

  pub fn validate_with_private(&self, public: &Private, data: &[u8]) -> Result<bool, Error> {
    let key = SigningKey::from_bytes(public.dump_buf());
    Ok(key.verify_strict(data, &DalekSignature::from_bytes(&self.0)).is_ok())
  }

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
}

impl Deref for Signature {
  type Target = [u8; 64]; // match the array size
  fn deref(&self) -> &Self::Target {
    &self.0
  }
}