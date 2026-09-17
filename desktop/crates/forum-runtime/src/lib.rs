//! Local transport and producer durability; this crate never opens the core DB.
pub mod audio;
pub mod ipc;
pub mod outbox;
pub mod process_io;
pub mod recording;
pub use ipc::{Endpoint, RpcError, RpcRequest, RpcResult, RuntimeClient, UdsServer};
pub use outbox::{DurableProducer, DurableReceipt, PendingEvent, RunRecord, RuntimeConfig};

use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::Path,
};

pub(crate) fn private_directory(path: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(path.is_absolute(), "private directory must be absolute");
    if !path.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
    }
    let m = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        m.is_dir() && m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o777 == 0o700,
        "private directory must be owned, non-symlink and mode 0700"
    );
    Ok(())
}
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow::anyhow!("no parent"))?;
    let temporary = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

mod model_fingerprint;
pub use model_fingerprint::model_fingerprint;
