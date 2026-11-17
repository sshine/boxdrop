use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Error, Result};

/// A 32-byte BLAKE3 digest, rendered as hex in human-readable formats.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hash32([u8; 32]);

impl Hash32 {
    pub const ZERO: Self = Self([0u8; 32]);

    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn of(bytes: impl AsRef<[u8]>) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(bytes.as_ref());
        Self(*hasher.finalize().as_bytes())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }

    /// A short prefix for logs and conflict names.
    pub fn short(self) -> String {
        self.to_hex()[..12].to_owned()
    }

    pub fn is_zero(self) -> bool {
        self == Self::ZERO
    }
}

impl AsRef<[u8]> for Hash32 {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Display for Hash32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Hash32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl FromStr for Hash32 {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        let raw = hex::decode(value).map_err(|error| Error::InvalidHash(error.to_string()))?;
        let bytes: [u8; 32] = raw
            .try_into()
            .map_err(|_| Error::InvalidHash(value.to_owned()))?;
        Ok(Self(bytes))
    }
}

impl Serialize for Hash32 {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Hash32 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

pub fn hash_bytes(bytes: impl AsRef<[u8]>) -> Hash32 {
    Hash32::of(bytes)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn round_trips_hex() {
        let hash = Hash32::of(b"boxdrop");
        assert_eq!(hash.to_string().parse::<Hash32>().unwrap(), hash);
    }

    #[test]
    fn serializes_as_hex() {
        let hash = Hash32::of(b"boxdrop");
        let json = serde_json::to_string(&hash).unwrap();
        assert_eq!(json, format!("\"{}\"", hash.to_hex()));
    }
}
