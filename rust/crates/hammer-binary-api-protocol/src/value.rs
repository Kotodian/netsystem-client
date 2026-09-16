//! Fixed-size protocol string values.

use std::fmt;
use std::str::Utf8Error;

use serde::de::{Error as _, SeqAccess, Visitor};
use serde::ser::SerializeTuple;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct FixedString<const N: usize> {
    bytes: [u8; N],
}

impl<const N: usize> FixedString<N> {
    pub const fn empty() -> Self {
        Self { bytes: [0; N] }
    }

    pub fn from_str(value: &str) -> Self {
        let mut bytes = [0; N];
        let length = value.len().min(N.saturating_sub(1));
        bytes[..length].copy_from_slice(&value.as_bytes()[..length]);
        Self { bytes }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn as_str(&self) -> Result<&str, Utf8Error> {
        let end = self.bytes.iter().position(|byte| *byte == 0).unwrap_or(N);
        std::str::from_utf8(&self.bytes[..end])
    }

    pub fn into_string(self) -> String {
        let end = self.bytes.iter().position(|byte| *byte == 0).unwrap_or(N);
        String::from_utf8_lossy(&self.bytes[..end]).into_owned()
    }
}

impl<const N: usize> Default for FixedString<N> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<const N: usize> fmt::Debug for FixedString<N> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.as_str() {
            Ok(value) => formatter.debug_tuple("FixedString").field(&value).finish(),
            Err(_) => formatter
                .debug_tuple("FixedString")
                .field(&self.bytes)
                .finish(),
        }
    }
}

impl<const N: usize> Serialize for FixedString<N> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut tuple = serializer.serialize_tuple(N)?;
        for byte in &self.bytes {
            tuple.serialize_element(byte)?;
        }
        tuple.end()
    }
}

impl<'de, const N: usize> Deserialize<'de> for FixedString<N> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FixedStringVisitor<const N: usize>;

        impl<'de, const N: usize> Visitor<'de> for FixedStringVisitor<N> {
            type Value = FixedString<N>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "{N} fixed-string bytes")
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut bytes = [0; N];
                for (index, byte) in bytes.iter_mut().enumerate() {
                    *byte = sequence
                        .next_element()?
                        .ok_or_else(|| A::Error::invalid_length(index, &self))?;
                }
                Ok(FixedString { bytes })
            }
        }

        deserializer.deserialize_tuple(N, FixedStringVisitor)
    }
}
