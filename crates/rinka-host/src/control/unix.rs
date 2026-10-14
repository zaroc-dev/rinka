//! Unix socket transport.

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

/// Default socket location: `$XDG_RUNTIME_DIR/rinka/host.sock`.
pub fn default_endpoint() -> anyhow::Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    Ok(PathBuf::from(runtime).join("rinka").join("host.sock"))
}

/// Binds the socket, refusing to start when another host already answers on
/// it and replacing a stale socket file otherwise.
pub fn bind(path: &Path) -> anyhow::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            bail!("another host is already listening on {}", path.display());
        }
        std::fs::remove_file(path).with_context(|| format!("removing stale {}", path.display()))?;
    }
    UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))
}

impl super::Listener for UnixListener {
    type Reader = UnixStream;
    type Writer = UnixStream;

    fn accept(&mut self) -> std::io::Result<(UnixStream, UnixStream)> {
        let (stream, _) = UnixListener::accept(self)?;
        let writer = stream.try_clone()?;
        Ok((stream, writer))
    }
}
