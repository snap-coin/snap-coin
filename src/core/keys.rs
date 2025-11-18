use std::ops::Deref;

use bincode::{Decode, Encode};
use ed25519_dalek::SigningKey;
use num_bigint::{BigUint};
use rand::Rng;

#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq)]
pub struct Public {
  public: [u8; 32]
}

impl Public {
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

    Some(Public {
      public: buf,
    })
  }

  pub const fn new_from_buf(buf: &[u8; 32]) -> Self {    
    Public {
      public: *buf,
    }
  }

  pub fn dump_buf(&self) -> &[u8; 32] {
    &self.public
  }

  pub fn dump_base36(&self) -> String {
    let big_int = BigUint::from_bytes_be(&self.public);
    big_int.to_str_radix(36)
  }
}

impl Deref for Public {
  type Target = [u8; 32];
  fn deref(&self) -> &Self::Target {
    &self.public
  }
}

#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq)]
pub struct Private {
  private: [u8; 32]
}

impl Private {
  pub fn new_random() -> Self {
    Private { private: rand::rng().random() }
  }

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

    Some(Private {
      private: buf,
    })
  }

  pub const fn new_from_buf(buf: &[u8; 32]) -> Option<Private> {
    Some(Private {
      private: *buf,
    })
  }

  pub fn dump_buf(&self) -> &[u8; 32] {
    &self.private
  }

  pub fn dump_base36(&self) -> String {
    let big_int = BigUint::from_bytes_be(&self.private);
    big_int.to_str_radix(36)
  }

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