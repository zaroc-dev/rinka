//! Control socket server.
//!
//! Each connection gets a reader thread. Requests are forwarded to the
//! presentation loop, which owns all wallpaper state, and the thread waits for
//! the reply before reading the next line.
//!
//! The transport is a Unix socket on Linux and a named pipe on Windows; both
//! carry the same newline-delimited JSON.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use anyhow::{Context, bail};
use serde_json::json;

use crate::protocol::{Error, ErrorCode, PROTOCOL_VERSION, Reply, Request, Response, parse_line};

#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};

/// A request awaiting a reply from the presentation loop.
pub struct Command {
    pub request: Request,
    pub reply: mpsc::Sender<Reply>,
    /// The pipe thread acknowledges the reply after its client reads it.
    #[cfg(windows)]
    pub delivered: mpsc::Receiver<()>,
}

/// Default socket location: `$XDG_RUNTIME_DIR/rinka/host.sock`.
#[cfg(unix)]
pub fn default_socket_path() -> anyhow::Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    Ok(PathBuf::from(runtime).join("rinka").join("host.sock"))
}

/// Binds the socket, refusing to start when another host already answers on
/// it and replacing a stale socket file otherwise.
#[cfg(unix)]
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

/// Accepts connections forever, forwarding requests through `send`.
/// `send` returns false once the presentation loop has gone away.
#[cfg(unix)]
pub fn serve<F>(listener: UnixListener, send: F)
where
    F: Fn(Command) -> bool + Clone + Send + 'static,
{
    thread::Builder::new()
        .name("rinka-control".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let send = send.clone();
                        let _ =
                            thread::Builder::new()
                                .name("rinka-client".into())
                                .spawn(move || {
                                    if let Ok(writer) = stream.try_clone() {
                                        handle_client(stream, writer, send);
                                    }
                                });
                    }
                    Err(e) => log::warn!("control accept failed: {e}"),
                }
            }
        })
        .expect("spawn control thread");
}

/// Default pipe: `\\.\pipe\rinka-host-<user>`. Named pipes share one
/// machine-wide namespace, so the user name keeps users' hosts apart.
#[cfg(windows)]
pub fn default_socket_path() -> anyhow::Result<PathBuf> {
    let user = std::env::var("USERNAME").context("USERNAME is not set")?;
    Ok(PathBuf::from(format!(r"\\.\pipe\rinka-host-{user}")))
}

/// A named pipe instance waiting for its client.
#[cfg(windows)]
pub struct PipeListener {
    name: Vec<u16>,
    next: std::os::windows::io::OwnedHandle,
}

/// Creates the first pipe instance, refusing to start when another host
/// already owns the name.
#[cfg(windows)]
pub fn bind(path: &Path) -> anyhow::Result<PipeListener> {
    use std::os::windows::ffi::OsStrExt;
    let text = path.to_string_lossy();
    if !text.starts_with(r"\\.\pipe\") {
        bail!(r"{text} is not a named pipe path (\\.\pipe\...)");
    }
    let name: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let next = pipe::create(&name, true).map_err(|e| {
        if e.raw_os_error() == Some(windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED as i32) {
            anyhow::anyhow!("another host is already listening on {text}")
        } else {
            anyhow::anyhow!("creating {text}: {e}")
        }
    })?;
    Ok(PipeListener { name, next })
}

/// Accepts connections forever, forwarding requests through `send`.
/// `send` returns false once the presentation loop has gone away.
#[cfg(windows)]
pub fn serve<F>(listener: PipeListener, send: F)
where
    F: Fn(Command) -> bool + Clone + Send + 'static,
{
    thread::Builder::new()
        .name("rinka-control".into())
        .spawn(move || {
            let PipeListener { name, mut next } = listener;
            loop {
                if let Err(e) = pipe::accept(&next) {
                    log::warn!("control accept failed: {e}");
                    // Early disconnects leave ERROR_NO_DATA until reset.
                    pipe::disconnect(&next);
                    continue;
                }
                // Open the next instance before serving this one, so a new
                // client always finds a listening instance.
                let connected = match pipe::create(&name, false) {
                    Ok(fresh) => std::mem::replace(&mut next, fresh),
                    Err(e) => {
                        log::error!("creating the next pipe instance: {e}");
                        return;
                    }
                };
                let send = send.clone();
                let _ = thread::Builder::new()
                    .name("rinka-client".into())
                    .spawn(move || {
                        let file = std::fs::File::from(connected);
                        if let Ok(writer) = file.try_clone() {
                            handle_client(&file, pipe::Flushing(writer), send);
                        }
                    });
            }
        })
        .expect("spawn control thread");
}

#[cfg(windows)]
mod pipe {
    use std::io::Write;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

    use windows_sys::Win32::Foundation::{
        ERROR_PIPE_CONNECTED, GetLastError, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, FlushFileBuffers, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    const BUFFER: u32 = 64 * 1024;

    /// Creates a blocking byte-mode pipe instance. The default security
    /// descriptor lets only the creating user, administrators and SYSTEM
    /// write to it; remote clients are refused.
    pub fn create(name: &[u16], first: bool) -> std::io::Result<OwnedHandle> {
        let flags = PIPE_ACCESS_DUPLEX
            | if first {
                FILE_FLAG_FIRST_PIPE_INSTANCE
            } else {
                0
            };
        // SAFETY: `name` is NUL-terminated; the returned handle is checked
        // and owned by the result.
        let handle = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                flags,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                BUFFER,
                BUFFER,
                0,
                std::ptr::null(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: a valid handle that nothing else owns.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }

    /// Waits for a client to connect to `pipe`.
    pub fn accept(pipe: &OwnedHandle) -> std::io::Result<()> {
        // SAFETY: a valid pipe handle, synchronous (no OVERLAPPED).
        if unsafe { ConnectNamedPipe(pipe.as_raw_handle(), std::ptr::null_mut()) } != 0 {
            return Ok(());
        }
        // A client that connected between creation and this call.
        // SAFETY: reads the calling thread's last error.
        match unsafe { GetLastError() } {
            ERROR_PIPE_CONNECTED => Ok(()),
            _ => Err(std::io::Error::last_os_error()),
        }
    }

    pub fn disconnect(pipe: &OwnedHandle) {
        // SAFETY: a valid named pipe handle owned by the listener.
        unsafe { DisconnectNamedPipe(pipe.as_raw_handle()) };
    }

    /// Flushes each reply so a client reads it even if the host exits right
    /// after writing it (the `shutdown` reply).
    pub struct Flushing(pub std::fs::File);

    impl Write for Flushing {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.write(buf)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            // SAFETY: a valid pipe handle owned by `self.0`.
            if unsafe { FlushFileBuffers(self.0.as_raw_handle()) } == 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }
}

fn handle_client<F: Fn(Command) -> bool>(reader: impl Read, mut writer: impl Write, send: F) {
    let mut greeted = false;
    for line in BufReader::new(reader).lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let (id, request) = parse_line(&line);
        #[cfg(windows)]
        let mut delivered = None;
        let reply = match request {
            Err(e) => Err(e),
            Ok(Request::Hello { protocol, client }) => {
                if protocol == PROTOCOL_VERSION {
                    greeted = true;
                    log::info!(
                        "client connected: {}",
                        client.as_deref().unwrap_or("unnamed")
                    );
                    Ok(json!({
                        "protocol": PROTOCOL_VERSION,
                        "host_version": env!("CARGO_PKG_VERSION"),
                        "engine_version": rinka_engine::ENGINE_VERSION,
                        "platform": std::env::consts::OS,
                        "pid": std::process::id(),
                    }))
                } else {
                    Err(Error::new(
                        ErrorCode::UnsupportedProtocol,
                        format!("host speaks protocol {PROTOCOL_VERSION}, client sent {protocol}"),
                    ))
                }
            }
            Ok(_) if !greeted => Err(Error::new(ErrorCode::HelloRequired, "send hello first")),
            Ok(request) => {
                let (tx, rx) = mpsc::channel();
                #[cfg(windows)]
                let (done_tx, done_rx) = mpsc::channel();
                if !send(Command {
                    request,
                    reply: tx,
                    #[cfg(windows)]
                    delivered: done_rx,
                }) {
                    break;
                }
                #[cfg(windows)]
                {
                    delivered = Some(done_tx);
                }
                match rx.recv() {
                    Ok(reply) => reply,
                    Err(_) => break,
                }
            }
        };
        let mut out =
            serde_json::to_string(&Response::new(id, reply)).expect("response serializes");
        out.push('\n');
        if writer.write_all(out.as_bytes()).is_err() || writer.flush().is_err() {
            break;
        }
        #[cfg(windows)]
        if let Some(done) = delivered {
            let _ = done.send(());
        }
    }
}
