//! Ed25519 signing and verification for plugin modules.
//!
//! Modules are signed over their raw file bytes — the `.wat` text or the `.wasm`
//! binary exactly as it sits on disk — so the signature covers what is actually
//! about to be compiled, with no canonicalisation step for an attacker to
//! exploit. This is the mechanism `PluginHost::load_signed` uses, and the shape
//! a future `harness plugin sign` needs.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use harness_core::{HarnessError, Result};

/// Signing keys and public keys are 32 bytes; signatures are 64.
const KEY_LEN: usize = 32;
const SIGNATURE_LEN: usize = 64;

/// Produces a detached Ed25519 signature over `wasm`.
///
/// The signing key is raw seed material rather than a `SigningKey` so callers
/// can hand over a key they just read from disk or an environment variable
/// without this crate taking a dependency on key-file formats.
pub fn sign_module(wasm: &[u8], signing_key: &[u8; KEY_LEN]) -> Result<Vec<u8>> {
    let key = SigningKey::from_bytes(signing_key);
    // `try_sign` rather than the panicking `sign`: a signature that cannot be
    // produced is a normal error path for an operation that reads keys.
    let signature = key
        .try_sign(wasm)
        .map_err(|err| HarnessError::Sandbox(format!("failed to sign module: {err}")))?;
    Ok(signature.to_bytes().to_vec())
}

/// Verifies a detached Ed25519 signature over `wasm`.
///
/// Uses strict verification, which also rejects small-order (weak) public keys;
/// for module signing there is no compatibility reason to accept them.
pub fn verify_module(wasm: &[u8], signature: &[u8], public_key: &[u8]) -> Result<()> {
    let key_bytes: [u8; KEY_LEN] = public_key.try_into().map_err(|_| {
        HarnessError::Sandbox(format!(
            "ed25519 public key must be {KEY_LEN} bytes, got {}",
            public_key.len()
        ))
    })?;
    let verifying_key = VerifyingKey::from_bytes(&key_bytes)
        .map_err(|err| HarnessError::Sandbox(format!("invalid ed25519 public key: {err}")))?;

    let signature_bytes: [u8; SIGNATURE_LEN] = signature.try_into().map_err(|_| {
        HarnessError::Sandbox(format!(
            "ed25519 signature must be {SIGNATURE_LEN} bytes, got {}",
            signature.len()
        ))
    })?;

    verifying_key
        .verify_strict(wasm, &Signature::from_bytes(&signature_bytes))
        .map_err(|err| HarnessError::Sandbox(format!("module signature is not valid: {err}")))
}

/// The public key belonging to `signing_key`, for tests and for key tooling.
pub fn public_key_of(signing_key: &[u8; KEY_LEN]) -> [u8; KEY_LEN] {
    SigningKey::from_bytes(signing_key)
        .verifying_key()
        .to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; KEY_LEN] = [7u8; KEY_LEN];

    #[test]
    fn a_signature_verifies_against_the_matching_key() {
        let module = b"(module)";
        let signature = sign_module(module, &KEY).unwrap();

        assert_eq!(signature.len(), SIGNATURE_LEN);
        assert!(verify_module(module, &signature, &public_key_of(&KEY)).is_ok());
    }

    #[test]
    fn tampered_bytes_and_wrong_keys_are_refused() {
        let module = b"(module)";
        let signature = sign_module(module, &KEY).unwrap();
        let other_key = public_key_of(&[9u8; KEY_LEN]);

        assert!(verify_module(b"(module )", &signature, &public_key_of(&KEY)).is_err());
        assert!(verify_module(module, &signature, &other_key).is_err());
    }

    #[test]
    fn malformed_keys_and_signatures_are_reported_not_panicked_on() {
        let err = verify_module(b"(module)", &[0u8; SIGNATURE_LEN], &[0u8; 3]).unwrap_err();
        assert!(err.to_string().contains("32 bytes"), "{err}");

        let err = verify_module(b"(module)", &[0u8; 3], &public_key_of(&KEY)).unwrap_err();
        assert!(err.to_string().contains("64 bytes"), "{err}");
    }

    #[test]
    fn an_empty_signature_is_not_accepted_as_valid() {
        assert!(verify_module(b"(module)", &[0u8; SIGNATURE_LEN], &public_key_of(&KEY)).is_err());
    }
}
