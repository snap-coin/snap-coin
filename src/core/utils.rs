use num_bigint::BigUint;

pub fn clamp_f(x: f64, minv: f64, maxv: f64) -> f64 {
  x.max(minv).min(maxv)
}

pub fn clamp_i(x: i64, minv: i64, maxv: i64) -> i64 {
  x.max(minv).min(maxv)
}

pub fn max_256_bui() -> BigUint {
  BigUint::from_bytes_be(&[0xFFu8; 32])
}

pub fn max_256_buf() -> [u8; 32] {
  [0xFFu8; 32]
}