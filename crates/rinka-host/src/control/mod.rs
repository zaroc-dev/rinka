//! Control endpoint server.
//!
//! Each connection gets a reader thread. Requests are forwarded to the
//! presentation loop, which owns all wallpaper state, and the thread waits for
//! the reply before reading the next line.
//!
//! The protocol is the same newline-delimited JSON everywhere; only the
//! transport differs: a Unix socket (`unix.rs`) or a named pipe (`pipe.rs`).

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::json;

use crate::protocol::{Error, ErrorCode, PROTOCOL_VERSION, Reply, Request, Response, parse_line};

#[cfg(windows)]
mod pipe;
#[cfg(unix)]
mod unix;

#[cfg(windows)]
pub use pipe::{bind, default_endpoint};
#[cfg(unix)]
pub use unix::{bind, default_endpoint};

/// A bound endpoint that hands out one connection at a time.
pub trait Listener: Send + 'static {
    type Reader: Read + Send + 'static;
    type Writer: Write + Send + 'static;

    /// Blocks until a client connects. Errors are logged and retried.
    fn accept(&mut self) -> std::io::Result<(Self::Reader, Self::Writer)>;
}

/// A request awaiting a reply from the presentation loop.
pub struct Command {
    pub request: Request,
    pub reply: mpsc::Sender<Reply>,
    /// Signalled once the reply has been written to the client.
    pub delivered: Delivery,
}

/// See [`Command::delivered`].
pub struct Delivery(mpsc::Receiver<()>);

impl Delivery {
    /// Waits until the client has the reply, so the host can exit after a
    /// `shutdown` without cutting it off. A stalled client cannot hold the
    /// host for longer than a second.
    pub fn wait(&self) {
        let _ = self.0.recv_timeout(Duration::from_secs(1));
    }
}

/// Accepts connections forever, forwarding requests through `send`.
/// `send` returns false once the presentation loop has gone away.
pub fn serve<L, F>(mut listener: L, send: F)
where
    L: Listener,
    F: Fn(Command) -> bool + Clone + Send + 'static,
{
    thread::Builder::new()
        .name("rinka-control".into())
        .spawn(move || {
            let mut failures = 0u32;
            loop {
                match listener.accept() {
                    Ok((reader, writer)) => {
                        failures = 0;
                        let send = send.clone();
                        let _ = thread::Builder::new()
                            .name("rinka-client".into())
                            .spawn(move || handle_client(reader, writer, send));
                    }
                    Err(e) => {
                        log::warn!("control accept failed: {e}");
                        // Back off when the error persists instead of spinning.
                        failures += 1;
                        if failures > 3 {
                            thread::sleep(Duration::from_millis(100));
                        }
                    }
                }
            }
        })
        .expect("spawn control thread");
}

fn handle_client<F: Fn(Command) -> bool>(reader: impl Read, mut writer: impl Write, send: F) {
    let mut greeted = false;
    for line in BufReader::new(reader).lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let (id, request) = parse_line(&line);
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
                let (done_tx, done_rx) = mpsc::channel();
                if !send(Command {
                    request,
                    reply: tx,
                    delivered: Delivery(done_rx),
                }) {
                    break;
                }
                delivered = Some(done_tx);
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
        if let Some(done) = delivered {
            let _ = done.send(());
        }
    }
}
