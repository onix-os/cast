use std::{
    fmt,
    io::{self, Write},
    str::FromStr,
};

use chrono::{DateTime, Utc};
use derive_more::{Debug, Display, From, Into};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use thiserror::Error;
use tui::{Styled, pretty};

use crate::package;

/// Unique identifier for [`State`]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, From, Into, Display)]
#[debug("{_0:?}")]
pub struct Id(i32);

impl Id {
    /// Return the next sequential Id
    pub fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// Durable correlation identifier for one state transition.
///
/// The same value is written to the activation journal and, while a fresh
/// state is in flight, to the state database. Keeping one validated type on
/// both sides prevents recovery from relying on a free-form lookup key.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub(crate) struct TransitionId(String);

impl TransitionId {
    pub(crate) const TEXT_LENGTH: usize = 32;
    const RANDOM_BYTES: usize = Self::TEXT_LENGTH / 2;

    /// Generate one canonical transition identifier from the kernel CSPRNG.
    ///
    /// Recovery depends on this value being unguessable and collision-resistant;
    /// there is deliberately no time-, PID-, counter-, or userspace fallback.
    #[allow(dead_code)] // consumed by the activation-coordinator integration slice
    pub(crate) fn generate() -> io::Result<Self> {
        let mut random = [0_u8; Self::RANDOM_BYTES];
        let mut filled = 0;
        while filled < random.len() {
            // SAFETY: getrandom writes at most the supplied remaining length
            // into the live array and retains no pointer after returning.
            let result = unsafe {
                nix::libc::syscall(
                    nix::libc::SYS_getrandom,
                    random[filled..].as_mut_ptr(),
                    random.len() - filled,
                    0,
                )
            };
            if result == -1 {
                let source = io::Error::last_os_error();
                if source.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(source);
            }
            let read = usize::try_from(result)
                .map_err(|_| io::Error::other("getrandom returned a negative transition-ID length"))?;
            if read == 0 || read > random.len() - filled {
                return Err(io::Error::other("getrandom returned an invalid transition-ID length"));
            }
            filled += read;
        }

        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut encoded = [0_u8; Self::TEXT_LENGTH];
        for (index, byte) in random.into_iter().enumerate() {
            encoded[index * 2] = HEX[usize::from(byte >> 4)];
            encoded[index * 2 + 1] = HEX[usize::from(byte & 0x0f)];
        }
        let encoded = String::from_utf8(encoded.to_vec()).expect("lowercase hexadecimal is valid UTF-8");
        Self::parse(encoded).map_err(|_| io::Error::other("kernel randomness encoded to a noncanonical transition ID"))
    }

    pub(crate) fn parse(value: impl Into<String>) -> Result<Self, TransitionIdError> {
        let value = value.into();
        if value.len() != Self::TEXT_LENGTH
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(TransitionIdError);
        }
        Ok(Self(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TransitionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for TransitionId {
    type Err = TransitionIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl AsRef<str> for TransitionId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<'de> Deserialize<'de> for TransitionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(D::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("transition ID must be exactly 32 lowercase hexadecimal characters")]
pub(crate) struct TransitionIdError;

/// State types
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::EnumString)]
#[repr(u8)]
#[strum(serialize_all = "kebab-case")]
pub enum Kind {
    /// Automatically constructed state
    Transaction,
}

impl TryFrom<String> for Kind {
    type Error = strum::ParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    /// Unique identifier for this state
    pub id: Id,
    /// Quick summary for the state (optional)
    pub summary: Option<String>,
    /// Description for the state (optional)
    pub description: Option<String>,
    /// Selections in this state
    pub selections: Vec<Selection>,
    /// Creation timestamp
    pub created: DateTime<Utc>,
    /// Relevant type for this State
    pub kind: Kind,
}

/// The Selection records the presence of a package ID in a [`State`]
/// It also records whether it was selected as a transitive dependency,
/// along with an optional human-readable reason
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub package: package::Id,
    /// Marks whether the package was explicitly installed
    /// by the user, or if it's a "transitive" dependency
    pub explicit: bool,
    pub reason: Option<String>,
}

impl Selection {
    /// Construct a new explicit Selection to indicate user intent
    pub fn explicit(package: package::Id) -> Self {
        Self {
            package,
            explicit: true,
            reason: None,
        }
    }

    /// Construct a new transitive Selection to mark automatic installation
    pub fn transitive(package: package::Id) -> Self {
        Self {
            package,
            explicit: true,
            reason: None,
        }
    }

    /// Record a reason for the Selection entering the state
    pub fn reason(self, reason: impl ToString) -> Self {
        Self {
            reason: Some(reason.to_string()),
            ..self
        }
    }
}

/// Columnar display encapsulation for a [`State`]
pub struct ColumnDisplay<'a>(pub &'a State);

impl pretty::ColumnDisplay for ColumnDisplay<'_> {
    fn get_display_width(&self) -> usize {
        "State ".len() + self.0.id.to_string().len()
    }

    fn display_column(&self, writer: &mut impl Write, _col: pretty::Column, width: usize) {
        let _ = write!(writer, "State {}{:width$}", self.0.id.to_string().bold(), " ");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_TRANSITION_ID: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn transition_id_has_one_canonical_text_encoding() {
        let transition_id = TransitionId::parse(VALID_TRANSITION_ID).unwrap();
        assert_eq!(transition_id.as_str(), VALID_TRANSITION_ID);
        assert_eq!(transition_id.to_string(), VALID_TRANSITION_ID);
        assert_eq!(
            serde_json::to_string(&transition_id).unwrap(),
            format!("\"{VALID_TRANSITION_ID}\"")
        );
        assert_eq!(
            serde_json::from_str::<TransitionId>(&format!("\"{VALID_TRANSITION_ID}\"")).unwrap(),
            transition_id
        );
    }

    #[test]
    fn transition_id_rejects_noncanonical_lengths_and_characters() {
        for invalid in [
            "",
            "0123456789abcdef0123456789abcde",
            "0123456789abcdef0123456789abcdef0",
            "0123456789ABCDEF0123456789ABCDEF",
            "0123456789abcdef0123456789abcdeg",
            "0123456789abcdef0123456789abcde-",
        ] {
            assert_eq!(TransitionId::parse(invalid), Err(TransitionIdError));
            assert!(serde_json::from_str::<TransitionId>(&format!("\"{invalid}\"")).is_err());
        }
    }

    #[test]
    fn generated_transition_ids_are_canonical_and_distinct() {
        let mut generated = std::collections::BTreeSet::new();
        for _ in 0..64 {
            let transition_id = TransitionId::generate().unwrap();
            assert_eq!(transition_id.as_str().len(), TransitionId::TEXT_LENGTH);
            assert_eq!(TransitionId::parse(transition_id.to_string()).unwrap(), transition_id);
            assert!(
                generated.insert(transition_id),
                "kernel CSPRNG repeated a 128-bit transition ID"
            );
        }
    }
}
