//! Control socket server.
//!
//! Each connection gets a reader thread. Requests are forwarded to the
//! presentation loop, which owns all wallpaper state, and the thread waits for
//! the reply before reading the next line.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use anyhow::{Context, bail};
use serde_json::json;

use crate::protocol::{Error, ErrorCode, PROTOCOL_VERSION, Reply, Request, Response, parse_line};

/// A request awaiting a reply from the presentation loop.
pub struct Command {
    pub request: Request,
    pub reply: mpsc::Sender<Reply>,
}

/// Default socket location: `$XDG_RUNTIME_DIR/rinka/host.sock`.
pub fn default_socket_path() -> anyhow::Result<PathBuf> {
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

/// Accepts connections forever, forwarding requests through `send`.
/// `send` returns false once the presentation loop has gone away.
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
                        let _ = thread::Builder::new()
                            .name("rinka-client".into())
                            .spawn(move || handle_client(stream, send));
                    }
                    Err(e) => log::warn!("control accept failed: {e}"),
                }
            }
        })
        .expect("spawn control thread");
}

fn handle_client<F: Fn(Command) -> bool>(stream: UnixStream, send: F) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut greeted = false;
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let (id, request) = parse_line(&line);
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
                if !send(Command { request, reply: tx }) {
                    break;
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
        if writer.write_all(out.as_bytes()).is_err() {
            break;
        }
    }
}
