//! niri's IPC event stream, read for the overview backdrop.
//!
//! niri shows background layer surfaces in its overview backdrop when a
//! `place-within-backdrop` layer rule matches them. The backdrop of an
//! animated wallpaper is a still, refreshed each time the overview opens.

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use anyhow::Context;
use serde_json::Value;

/// niri's IPC socket, when the host runs under niri.
pub fn socket() -> Option<std::ffi::OsString> {
    std::env::var_os("NIRI_SOCKET")
}

/// Calls `opened` on a background thread each time the overview opens, until
/// it returns false or niri ends the stream.
pub fn watch_overview(
    socket: std::ffi::OsString,
    mut opened: impl FnMut() -> bool + Send + 'static,
) {
    let spawned = std::thread::Builder::new()
        .name("niri events".into())
        .spawn(move || {
            if let Err(e) = read_events(&socket, &mut opened) {
                log::warn!("niri event stream: {e:#}; overview backdrops stop refreshing");
            }
        });
    if let Err(e) = spawned {
        log::error!("starting the niri event thread: {e}");
    }
}

fn read_events(socket: &OsStr, opened: &mut dyn FnMut() -> bool) -> anyhow::Result<()> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connecting to {}", socket.to_string_lossy()))?;
    stream.write_all(b"\"EventStream\"\n")?;
    let mut lines = BufReader::new(stream).lines();
    let reply = lines.next().context("niri closed the socket")??;
    let reply: Value = serde_json::from_str(&reply).context("reading niri's reply")?;
    anyhow::ensure!(reply.get("Ok").is_some(), "niri refused: {reply}");
    for line in lines {
        let line = line?;
        // Most events are about windows and workspaces; skip parsing them.
        if line.contains("OverviewOpenedOrClosed") && overview_opened(&line)? && !opened() {
            break;
        }
    }
    Ok(())
}

fn overview_opened(line: &str) -> anyhow::Result<bool> {
    let event: Value = serde_json::from_str(line).context("reading a niri event")?;
    Ok(event
        .get("OverviewOpenedOrClosed")
        .and_then(|e| e.get("is_open"))
        .and_then(Value::as_bool)
        == Some(true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn calls_back_when_the_overview_opens() {
        let path = std::env::temp_dir().join(format!("rinka-niri-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(&stream).read_line(&mut request).unwrap();
            assert_eq!(request, "\"EventStream\"\n");
            let mut stream = stream;
            for line in [
                r#"{"Ok":"Handled"}"#,
                r#"{"OverviewOpenedOrClosed":{"is_open":false}}"#,
                r#"{"WindowFocusChanged":{"id":null}}"#,
                r#"{"OverviewOpenedOrClosed":{"is_open":true}}"#,
                r#"{"OverviewOpenedOrClosed":{"is_open":false}}"#,
                r#"{"OverviewOpenedOrClosed":{"is_open":true}}"#,
            ] {
                writeln!(stream, "{line}").unwrap();
            }
        });
        let mut opens = 0;
        read_events(path.as_os_str(), &mut || {
            opens += 1;
            true
        })
        .unwrap();
        server.join().unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(opens, 2);
    }

    #[test]
    fn a_refused_stream_is_an_error() {
        let path = std::env::temp_dir().join(format!("rinka-niri-err-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            writeln!(stream, r#"{{"Err":"no"}}"#).unwrap();
        });
        let result = read_events(path.as_os_str(), &mut || true);
        server.join().unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(result.is_err());
    }
}
