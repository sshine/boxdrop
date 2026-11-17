use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RecentSync {
    pub at_ms: i64,
    pub paths: u64,
    pub bytes_up: u64,
    pub bytes_down: u64,
    pub conflicts: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    pub state: String,
    pub progress: u8,
    pub conflicts: u64,
    pub paused: bool,
    pub bytes_up: u64,
    pub bytes_down: u64,
    pub last_sync_ms: i64,
    pub queue_pending: u64,
    pub queue_leased: u64,
    pub file_count: u64,
    pub root: String,
    pub recent: Vec<RecentSync>,
    pub error: Option<String>,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            state: "resting".to_owned(),
            progress: 0,
            conflicts: 0,
            paused: false,
            bytes_up: 0,
            bytes_down: 0,
            last_sync_ms: 0,
            queue_pending: 0,
            queue_leased: 0,
            file_count: 0,
            root: String::new(),
            recent: Vec::new(),
            error: None,
        }
    }
}
