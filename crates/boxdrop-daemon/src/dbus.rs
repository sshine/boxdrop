use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use boxdrop_store::Store;
use zbus::interface;

use crate::status::Status;

pub const BUS_NAME: &str = "org.boxdrop.Daemon";
pub const OBJECT_PATH: &str = "/org/boxdrop/Daemon";
pub const INTERFACE: &str = "org.boxdrop.Daemon";

pub struct DbusDaemon {
    pub status: Arc<Mutex<Status>>,
    pub store: Arc<Store>,
    pub sync_now: Arc<AtomicBool>,
}

impl DbusDaemon {
    fn status(&self) -> std::sync::MutexGuard<'_, Status> {
        self.status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[interface(name = "org.boxdrop.Daemon")]
impl DbusDaemon {
    #[zbus(property)]
    fn state(&self) -> String {
        self.status().state.clone()
    }

    #[zbus(property)]
    fn progress(&self) -> u8 {
        self.status().progress
    }

    #[zbus(property)]
    fn conflicts(&self) -> u64 {
        self.status().conflicts
    }

    #[zbus(property)]
    fn paused(&self) -> bool {
        self.status().paused
    }

    #[zbus(property)]
    fn bytes_up(&self) -> u64 {
        self.status().bytes_up
    }

    #[zbus(property)]
    fn bytes_down(&self) -> u64 {
        self.status().bytes_down
    }

    #[zbus(property)]
    fn last_sync(&self) -> String {
        self.status().last_sync_ms.to_string()
    }

    #[zbus(property)]
    fn queue_pending(&self) -> u64 {
        self.status().queue_pending
    }

    #[zbus(property)]
    fn queue_leased(&self) -> u64 {
        self.status().queue_leased
    }

    #[zbus(property)]
    fn file_count(&self) -> u64 {
        self.status().file_count
    }

    #[zbus(property)]
    fn recent(&self) -> String {
        serde_json::to_string(&self.status().recent).unwrap_or_else(|_| "[]".to_owned())
    }

    #[zbus(property)]
    fn status_json(&self) -> String {
        serde_json::to_string(&*self.status()).unwrap_or_else(|_| "{}".to_owned())
    }

    #[zbus(property)]
    fn root(&self) -> String {
        self.store
            .list_roots()
            .ok()
            .and_then(|roots| {
                roots
                    .into_iter()
                    .next()
                    .map(|(_, path, _)| path.display().to_string())
            })
            .unwrap_or_default()
    }

    #[zbus(property)]
    fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_owned()
    }

    fn pause(&self) -> zbus::fdo::Result<()> {
        self.store
            .set_paused(true)
            .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
        self.status().paused = true;
        self.status().state = "paused".to_owned();
        Ok(())
    }

    fn resume(&self) -> zbus::fdo::Result<()> {
        self.store
            .set_paused(false)
            .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
        self.status().paused = false;
        self.status().state = "resting".to_owned();
        Ok(())
    }

    fn sync_now(&self) {
        self.sync_now.store(true, Ordering::Relaxed);
    }
}

pub async fn serve(daemon: DbusDaemon) -> anyhow::Result<()> {
    let _connection = zbus::connection::Builder::session()
        .name(BUS_NAME)
        .serve_at(OBJECT_PATH, daemon)
        .build()
        .await?;
    std::future::pending::<()>().await;
    Ok(())
}
