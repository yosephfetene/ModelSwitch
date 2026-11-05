//! Client API keys: generation, hashing, and display (ADR-012).
//!
//! A key is `ms_dev_` or `ms_live_` followed by 32 random bytes in base62
//! (43 characters). Only its SHA-256 hash and a short display prefix are
//! stored; the plaintext exists once, at creation, inside a
//! `SecretString` so it cannot be printed by accident.

use std::fmt;

use secrecy::SecretString;
use sha2::{Digest, Sha256};

/// Which environment a key is for. A label for people and secret
/// scanners: the server treats both alike, because each environment has
/// its own database and so its own keys (ADR-033).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum KeyEnvironment {
    #[default]
    Dev,
    Live,
}

impl KeyEnvironment {
    const ALL: [Self; 2] = [Self::Dev, Self::Live];

    pub fn prefix(self) -> &'static str {
        match self {
            Self::Dev => "ms_dev_",
            Self::Live => "ms_live_",
        }
    }
}

const BASE62: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
/// 62^43 > 2^256, so 43 base62 digits hold any 32-byte value.
const BODY_LEN: usize = 43;
/// Characters of the random body shown in listings, after the prefix.
const DISPLAY_BODY_LEN: usize = 4;
const NAME_MAX_CHARS: usize = 64;
const EXPIRY_DAYS_RANGE: std::ops::RangeInclusive<u32> = 1..=3650;

/// A freshly generated key. The only value that ever holds the plaintext.
#[derive(Debug)]
pub struct NewApiKey {
    plaintext: SecretString,
    prefix: String,
    hash: KeyHash,
}

impl NewApiKey {
    /// Generates a key from the operating system's secure random source.
    pub fn generate(environment: KeyEnvironment) -> Result<Self, getrandom::Error> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)?;
        Ok(Self::from_random_bytes(environment, bytes))
    }

    /// Builds the key from given bytes; deterministic, for tests.
    pub fn from_random_bytes(environment: KeyEnvironment, bytes: [u8; 32]) -> Self {
        let env_prefix = environment.prefix();
        let plaintext = format!("{env_prefix}{}", base62(bytes));
        let prefix = plaintext[..env_prefix.len() + DISPLAY_BODY_LEN].to_owned();
        let hash = KeyHash::of(&plaintext);
        Self {
            plaintext: SecretString::from(plaintext),
            prefix,
            hash,
        }
    }

    /// The full key. Expose it only to show it to the operator once.
    pub fn plaintext(&self) -> &SecretString {
        &self.plaintext
    }

    /// Display prefix such as `ms_dev_7Hq2`; safe to store and print.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    pub fn hash(&self) -> KeyHash {
        self.hash
    }
}

/// SHA-256 of a full key. A fast hash is enough because keys are
/// high-entropy, unlike passwords (ADR-012).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyHash([u8; 32]);

impl KeyHash {
    pub fn of(key: &str) -> Self {
        Self(Sha256::digest(key.as_bytes()).into())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Whether `token` has the exact shape of a key: an environment prefix
/// plus 43 base62 characters. Lets authentication reject junk without a
/// database query.
pub fn looks_like_key(token: &str) -> bool {
    KeyEnvironment::ALL.iter().any(|environment| {
        token
            .strip_prefix(environment.prefix())
            .is_some_and(|body| {
                body.len() == BODY_LEN && body.bytes().all(|b| b.is_ascii_alphanumeric())
            })
    })
}

/// Encodes 32 bytes as exactly 43 base62 digits (big-endian, zero-padded)
/// by repeated division of the byte array by 62.
fn base62(mut number: [u8; 32]) -> String {
    let mut digits = [0u8; BODY_LEN];
    for digit in digits.iter_mut().rev() {
        let mut remainder: u32 = 0;
        for byte in number.iter_mut() {
            let value = (remainder << 8) | u32::from(*byte);
            *byte = (value / 62) as u8;
            remainder = value % 62;
        }
        *digit = BASE62[remainder as usize];
    }
    String::from_utf8(digits.to_vec()).expect("base62 digits are ASCII")
}

/// Operator-chosen label for a key: 1--64 characters after trimming,
/// no control characters (so it is safe to print in listings and logs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyName(String);

impl KeyName {
    pub fn parse(name: &str) -> Result<Self, KeyNameError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(KeyNameError::Empty);
        }
        if name.chars().count() > NAME_MAX_CHARS {
            return Err(KeyNameError::TooLong);
        }
        if name.chars().any(char::is_control) {
            return Err(KeyNameError::ControlCharacter);
        }
        Ok(Self(name.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum KeyNameError {
    #[error("key name must not be empty")]
    Empty,
    #[error("key name must be at most 64 characters")]
    TooLong,
    #[error("key name must not contain control characters")]
    ControlCharacter,
}

/// How long a new key lasts: at least a day, at most about ten years.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpiryDays(u32);

impl ExpiryDays {
    pub fn parse(days: u32) -> Result<Self, ExpiryDaysError> {
        if EXPIRY_DAYS_RANGE.contains(&days) {
            Ok(Self(days))
        } else {
            Err(ExpiryDaysError)
        }
    }

    pub fn days(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("key expiry must be between 1 and 3650 days")]
pub struct ExpiryDaysError;

/// Derived from `revoked_at` and `expires_at`; never stored separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStatus {
    Active,
    Expired,
    Revoked,
}

impl fmt::Display for KeyStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `pad`, not `write_str`, so width flags like `{:<8}` apply.
        f.pad(match self {
            Self::Active => "active",
            Self::Expired => "expired",
            Self::Revoked => "revoked",
        })
    }
}

#[cfg(test)]
mod tests {
    use secrecy::ExposeSecret;

    use super::*;

    #[test]
    fn key_is_dev_prefix_plus_43_base62_characters() {
        let key = NewApiKey::generate(KeyEnvironment::Dev).unwrap();
        let plaintext = key.plaintext().expose_secret();

        let body = plaintext.strip_prefix("ms_dev_").expect("ms_dev_ prefix");
        assert_eq!(body.len(), 43);
        assert!(body.bytes().all(|b| b.is_ascii_alphanumeric()), "{body}");
    }

    #[test]
    fn generated_keys_differ() {
        let a = NewApiKey::generate(KeyEnvironment::Dev).unwrap();
        let b = NewApiKey::generate(KeyEnvironment::Dev).unwrap();

        assert_ne!(a.plaintext().expose_secret(), b.plaintext().expose_secret());
        assert_ne!(a.hash(), b.hash());
    }

    #[test]
    fn base62_handles_the_extremes() {
        assert_eq!(base62([0; 32]), "0".repeat(43));
        // 2^256 - 1 in base62, computed independently with .NET BigInteger.
        assert_eq!(
            base62([0xff; 32]),
            "yhjskwdA6OZ1AL1YmHWZWm8LLG7HjnuCA2j5rOw8Xp1"
        );
        assert_eq!(
            base62({
                let mut one = [0; 32];
                one[31] = 61;
                one
            }),
            format!("{}z", "0".repeat(42))
        );
    }

    #[test]
    fn prefix_is_dev_prefix_plus_four_characters() {
        let key = NewApiKey::from_random_bytes(KeyEnvironment::Dev, [0xff; 32]);

        assert_eq!(key.prefix(), "ms_dev_yhjs");
        assert!(key.plaintext().expose_secret().starts_with(key.prefix()));
    }

    #[test]
    fn live_keys_differ_from_dev_keys_only_in_their_prefix() {
        let dev = NewApiKey::from_random_bytes(KeyEnvironment::Dev, [0xff; 32]);
        let live = NewApiKey::from_random_bytes(KeyEnvironment::Live, [0xff; 32]);

        assert_eq!(live.prefix(), "ms_live_yhjs");
        assert_eq!(
            live.plaintext().expose_secret().strip_prefix("ms_live_"),
            dev.plaintext().expose_secret().strip_prefix("ms_dev_")
        );
        assert_ne!(live.hash(), dev.hash());
    }

    #[test]
    fn hash_is_sha256_of_the_full_key() {
        let key = NewApiKey::from_random_bytes(KeyEnvironment::Dev, [7; 32]);

        assert_eq!(key.hash(), KeyHash::of(key.plaintext().expose_secret()));
        // Known SHA-256 test vector, so the hash function itself is pinned.
        assert_eq!(KeyHash::of("abc").as_bytes()[..4], [0xba, 0x78, 0x16, 0xbf]);
    }

    #[test]
    fn debug_output_hides_the_plaintext() {
        let key = NewApiKey::generate(KeyEnvironment::Dev).unwrap();

        let debug = format!("{key:?}");
        assert!(!debug.contains(key.plaintext().expose_secret()), "{debug}");
    }

    #[test]
    fn only_well_formed_keys_look_like_keys() {
        let key = NewApiKey::generate(KeyEnvironment::Dev).unwrap();
        let valid = key.plaintext().expose_secret();
        assert!(looks_like_key(valid));

        let body = valid.strip_prefix("ms_dev_").unwrap();
        for token in [
            "",
            "hello",
            body,
            &format!("ms_test_{body}"),
            &format!("ms_live_{}", &body[1..]),
            &format!("ms_dev_ms_live_{body}"),
            &valid[..valid.len() - 1],
            &format!("{valid}x"),
            &format!("ms_dev_{}-", &body[1..]),
        ] {
            assert!(!looks_like_key(token), "{token:?}");
        }
    }

    #[test]
    fn both_environments_look_like_keys() {
        for environment in [KeyEnvironment::Dev, KeyEnvironment::Live] {
            let key = NewApiKey::generate(environment).unwrap();

            assert!(looks_like_key(key.plaintext().expose_secret()));
        }
    }

    #[test]
    fn expiry_days_are_validated() {
        for days in [1, 3650] {
            assert_eq!(ExpiryDays::parse(days).unwrap().days(), days);
        }
        for days in [0, 3651] {
            assert_eq!(ExpiryDays::parse(days), Err(ExpiryDaysError));
        }
    }

    #[test]
    fn status_respects_column_width() {
        assert_eq!(format!("{:<8}|", KeyStatus::Active), "active  |");
        assert_eq!(format!("{:<8}|", KeyStatus::Expired), "expired |");
        assert_eq!(format!("{:<8}|", KeyStatus::Revoked), "revoked |");
    }

    #[test]
    fn names_are_trimmed_and_validated() {
        assert_eq!(KeyName::parse("  laptop  ").unwrap().as_str(), "laptop");
        assert!(KeyName::parse(&"x".repeat(64)).is_ok());

        assert_eq!(KeyName::parse("   "), Err(KeyNameError::Empty));
        assert_eq!(KeyName::parse(&"x".repeat(65)), Err(KeyNameError::TooLong));
        assert_eq!(
            KeyName::parse("evil\nINFO forged"),
            Err(KeyNameError::ControlCharacter)
        );
    }
}
