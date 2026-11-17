use serde::{Deserialize, Serialize};

/// What to do when two hosts changed the same path concurrently.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConflictPolicy {
    /// Keep the local file and preserve the remote one as a conflict copy.
    #[default]
    ConflictCopy,
    /// Newer mtime wins. The loser is still retained in history.
    LastWriteWins,
    /// Three-way merge text files; fall back to a conflict copy.
    MergeText,
    /// Stop and wait for an explicit resolution.
    Manual,
}

impl ConflictPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ConflictCopy => "conflict-copy",
            Self::LastWriteWins => "last-write-wins",
            Self::MergeText => "merge-text",
            Self::Manual => "manual",
        }
    }
}

impl std::str::FromStr for ConflictPolicy {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "conflict-copy" | "conflict" => Ok(Self::ConflictCopy),
            "last-write-wins" | "lww" => Ok(Self::LastWriteWins),
            "merge-text" | "merge" => Ok(Self::MergeText),
            "manual" => Ok(Self::Manual),
            other => Err(format!("unknown conflict policy: {other}")),
        }
    }
}

/// The outcome of a conflict, recorded so the UI and history can show it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resolution {
    LocalWins,
    RemoteWins,
    KeptBoth,
    Merged,
}

impl Resolution {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalWins => "local-wins",
            Self::RemoteWins => "remote-wins",
            Self::KeptBoth => "kept-both",
            Self::Merged => "merged",
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn parses_aliases() {
        assert_eq!(
            "lww".parse::<ConflictPolicy>().unwrap(),
            ConflictPolicy::LastWriteWins
        );
        assert_eq!(
            "conflict".parse::<ConflictPolicy>().unwrap(),
            ConflictPolicy::ConflictCopy
        );
        assert!("unknown".parse::<ConflictPolicy>().is_err());
    }
}
