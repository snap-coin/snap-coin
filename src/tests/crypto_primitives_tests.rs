use rand::Rng;

use crate::crypto::{Hash, Signature, keys::Private};

#[test]
fn test_hashing() {
    let data = b"Hello, world!";

    // Create a hash from data
    let hash = Hash::new(data);

    // Ensure that comparing with the same data returns true
    assert!(
        hash.compare_with_data(data),
        "Hash should match the original data"
    );

    // Ensure that comparing with different data returns false
    let other_data = b"Goodbye, world!";
    assert!(
        !hash.compare_with_data(other_data),
        "Hash should not match different data"
    );

    // Test base36 conversion roundtrip
    let base36 = hash.dump_base36();
    let hash_from_base36 = Hash::new_from_base36(&base36).expect("Should convert back from base36");
    assert_eq!(
        hash, hash_from_base36,
        "Hash should remain the same after base36 roundtrip"
    );
}

#[test]
fn test_signing() -> Result<(), anyhow::Error> {
    let mut private = Private::new_random();
    let public = private.to_public();

    let data = b"Some correct data";
    let data_bad = b"Some invalid data";
    let signature = Signature::new_signature(&mut private, data);
    let signature_bad = Signature::new_from_buf(&rand::rng().random());

    assert!(signature.validate_with_private(&private, data)?, "A valid signature validation was rejected (private verification)");
    assert!(signature.validate_with_public(&public, data)?, "A valid signature validation was rejected (public verification)");

    assert!(!signature.validate_with_private(&private, data_bad)?, "A valid signature validation was accepted (private verification, bad data)");
    assert!(!signature.validate_with_public(&public, data_bad)?, "A valid signature validation was accepted (public verification, bad data)");

    assert!(!signature_bad.validate_with_private(&private, data)?, "A invalid signature validation was accepted (private verification)");
    assert!(!signature_bad.validate_with_public(&public, data)?, "A invalid signature validation was accepted (public verification)");
    Ok(())
}