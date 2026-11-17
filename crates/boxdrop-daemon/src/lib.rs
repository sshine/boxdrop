//! The Boxdrop daemon and the watcher protocol.
//!
//! The watcher is deliberately not allowed to touch SQLite. It forwards events
//! to the daemon over a Unix socket; the daemon owns the durable queue. If the
//! watcher dies, the daemon keeps running and its periodic rescan repairs the
//! index.

pub mod daemon;
pub mod dbus;
pub mod status;
pub mod watch;

pub use daemon::{Config, run};
pub use status::{RecentSync, Status};
pub use watch::WatchMessage;
