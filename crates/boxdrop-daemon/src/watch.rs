use serde::{Deserialize, Serialize};

/// The watcher-to-daemon protocol, one JSON object per line.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WatchMessage {
    /// `touched`, `removed` or `rescan`.
    pub kind: String,
    #[serde(default)]
    pub path: Option<String>,
}

impl WatchMessage {
    pub fn touched(path: impl Into<String>) -> Self {
        Self {
            kind: "touched".to_owned(),
            path: Some(path.into()),
        }
    }

    pub fn removed(path: impl Into<String>) -> Self {
        Self {
            kind: "removed".to_owned(),
            path: Some(path.into()),
        }
    }

    pub fn rescan() -> Self {
        Self {
            kind: "rescan".to_owned(),
            path: None,
        }
    }
}
