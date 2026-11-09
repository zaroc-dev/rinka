//! KDE Plasma integration.
//!
//! Plasma's desktop (icons, widgets and its own wallpaper) is a background
//! layer-shell surface too, and KWin keeps it above Rinka's: Rinka's
//! wallpaper is drawn but hidden. Two pieces make it visible:
//!
//! - The `dev.rinka.wallpaper` Plasma wallpaper plugin (`plasma/wallpaper`)
//!   draws nothing and makes the desktop window transparent. Packages
//!   install it; [`desktop`] installs it for the user when none does and
//!   switches Plasma's desktops to it or back (the `plasma_desktop` request).
//! - A KWin script, loaded while the host runs ([`KeepBelowDesktop`]), keeps
//!   Rinka's surfaces below the desktop window. Without it a surface created
//!   while an application window is active stacks above the desktop and
//!   hides its icons.
//!
//! Both talk to the session bus; plasmashell and KWin own the state, so the
//! host keeps none. Switching to the plugin restarts plasmashell once (see
//! [`desktop`]).

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde_json::{Value, json};
use zbus::blocking::Connection;

use crate::protocol::{Error, ErrorCode, Reply};

const PLUGIN_ID: &str = "dev.rinka.wallpaper";

/// The wallpaper plugin package, relative to its directory.
const PLUGIN_FILES: [(&str, &str); 3] = [
    (
        "metadata.json",
        include_str!("../plasma/wallpaper/metadata.json"),
    ),
    (
        "contents/ui/main.qml",
        include_str!("../plasma/wallpaper/contents/ui/main.qml"),
    ),
    (
        "contents/ui/config.qml",
        include_str!("../plasma/wallpaper/contents/ui/config.qml"),
    ),
];

const KWIN_SCRIPT: &str = include_str!("../plasma/keep-below-desktop.js");
const DESKTOPS_SCRIPT: &str = include_str!("../plasma/desktops.js");

/// Whether this is a Plasma session (`XDG_CURRENT_DESKTOP` names KDE).
pub fn session() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .is_ok_and(|v| v.split(':').any(|d| d.eq_ignore_ascii_case("KDE")))
}

/// Reports which wallpaper plugin each Plasma desktop uses. `show_rinka`
/// true switches every desktop to Rinka's plugin, false restores the plugins
/// it replaced. Blocks on plasmashell; call it off the event loop.
///
/// Switching to the plugin while a desktop window is showing leaves that
/// window opaque to KWin (Qt sets its opaque region once, when the window
/// is created with Plasma's black background), so the desktop turns black
/// instead of showing Rinka. Plasma's own windows are only created
/// transparent at startup, so a switch restarts plasmashell when systemd
/// runs it; otherwise the reply says a restart is needed and the plugin
/// shows the same note.
pub fn desktop(show_rinka: Option<bool>) -> Reply {
    let unavailable = |e: anyhow::Error| Error::new(ErrorCode::Unavailable, format!("{e:#}"));
    let installed = if show_rinka == Some(true) {
        Some(ensure_plugin().map_err(unavailable)?)
    } else {
        find_plugin()
    };
    let conn = Connection::session()
        .context("connecting to the session bus")
        .map_err(unavailable)?;
    let mut result = run_desktops_script(&conn, show_rinka).map_err(unavailable)?;
    let switched = result["switched"].as_u64().unwrap_or(0);
    let restart = if switched == 0 {
        Value::Null
    } else {
        match restart_plasmashell(&conn) {
            Ok(()) => json!("restarted"),
            Err(e) => {
                log::info!("plasmashell not restarted: {e:#}");
                json!("needed")
            }
        }
    };
    Ok(json!({
        "plugin": PLUGIN_ID,
        "plugin_path": installed,
        "desktops": result["desktops"].take(),
        "restart": restart,
    }))
}

fn run_desktops_script(conn: &Connection, show_rinka: Option<bool>) -> anyhow::Result<Value> {
    let script = format!("const show = {};\n{DESKTOPS_SCRIPT}", json!(show_rinka));
    let output: String = conn
        .call_method(
            Some("org.kde.plasmashell"),
            "/PlasmaShell",
            Some("org.kde.PlasmaShell"),
            "evaluateScript",
            &(script,),
        )
        .context("asking plasmashell (is Plasma running?)")?
        .body()
        .deserialize()?;
    // Script errors come back as text in place of the printed JSON.
    serde_json::from_str(output.trim())
        .with_context(|| format!("plasmashell script failed: {}", output.trim()))
}

/// Restarts Plasma's systemd unit, if that unit is what runs plasmashell.
/// Starting it otherwise would run a second shell.
fn restart_plasmashell(conn: &Connection) -> anyhow::Result<()> {
    const UNIT: &str = "plasma-plasmashell.service";
    const SYSTEMD: Option<&str> = Some("org.freedesktop.systemd1");
    let manager = Some("org.freedesktop.systemd1.Manager");
    let path: zbus::zvariant::OwnedObjectPath = conn
        .call_method(
            SYSTEMD,
            "/org/freedesktop/systemd1",
            manager,
            "GetUnit",
            &(UNIT,),
        )
        .with_context(|| format!("{UNIT} is not loaded"))?
        .body()
        .deserialize()?;
    let state: zbus::zvariant::OwnedValue = conn
        .call_method(
            SYSTEMD,
            path.as_str(),
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.systemd1.Unit", "ActiveState"),
        )?
        .body()
        .deserialize()?;
    let state = String::try_from(state)?;
    anyhow::ensure!(state == "active", "{UNIT} is {state}");
    conn.call_method(
        SYSTEMD,
        "/org/freedesktop/systemd1",
        manager,
        "RestartUnit",
        &(UNIT, "replace"),
    )?;
    log::info!("restarted plasmashell to show Rinka's wallpaper plugin");
    Ok(())
}

/// The plugin's directory, if Plasma can find it.
fn find_plugin() -> Option<PathBuf> {
    std::iter::once(data_home())
        .chain(data_dirs())
        .map(|d| d.join("plasma/wallpapers").join(PLUGIN_ID))
        .find(|d| d.join("metadata.json").is_file())
}

/// Installs the plugin into the user's data directory unless a package
/// provides it. A user copy is rewritten every time, so it follows the host.
fn ensure_plugin() -> anyhow::Result<PathBuf> {
    if let Some(dir) = data_dirs()
        .map(|d| d.join("plasma/wallpapers").join(PLUGIN_ID))
        .find(|d| d.join("metadata.json").is_file())
    {
        return Ok(dir);
    }
    let dir = data_home().join("plasma/wallpapers").join(PLUGIN_ID);
    for (name, content) in PLUGIN_FILES {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().expect("plugin files have a parent"))
            .with_context(|| format!("creating {}", dir.display()))?;
        std::fs::write(&path, content).with_context(|| format!("writing {}", path.display()))?;
    }
    log::info!("installed the Plasma wallpaper plugin in {}", dir.display());
    Ok(dir)
}

fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
        })
}

fn data_dirs() -> impl Iterator<Item = PathBuf> {
    let dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.split(':')
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .collect::<Vec<_>>()
        .into_iter()
}

/// The KWin script that keeps Rinka's surfaces below Plasma's desktop
/// window, loaded for as long as the host runs.
pub struct KeepBelowDesktop {
    /// Per control socket, so a second host (a test one) does not unload
    /// the first one's script when it exits.
    name: String,
}

impl KeepBelowDesktop {
    /// Loads the script on a separate thread; failures are only logged, as
    /// the wallpaper still works while no application window is active.
    pub fn load(endpoint: &Path) -> Self {
        let stem = endpoint
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = format!("rinka-keep-below-desktop-{stem}");
        let path = endpoint.with_file_name(format!("{stem}.kwin.js"));
        let thread_name = name.clone();
        std::thread::Builder::new()
            .name("rinka-kwin".into())
            .spawn(move || match load_kwin_script(&path, &thread_name) {
                Ok(()) => log::info!("KWin script {thread_name} loaded"),
                Err(e) => log::warn!("KWin script: {e:#}; Rinka may cover desktop icons"),
            })
            .expect("spawn KWin script thread");
        Self { name }
    }

    /// Unloads the script, so KWin is left as it was.
    pub fn unload(&self) {
        let result = Connection::session().and_then(|conn| {
            conn.call_method(
                Some("org.kde.KWin"),
                "/Scripting",
                Some("org.kde.kwin.Scripting"),
                "unloadScript",
                &(self.name.as_str(),),
            )
        });
        if let Err(e) = result {
            log::warn!("unloading KWin script {}: {e}", self.name);
        }
    }
}

fn load_kwin_script(path: &Path, name: &str) -> anyhow::Result<()> {
    const KWIN: Option<&str> = Some("org.kde.KWin");
    const SCRIPTING: Option<&str> = Some("org.kde.kwin.Scripting");
    std::fs::write(path, KWIN_SCRIPT).with_context(|| format!("writing {}", path.display()))?;
    let file = path.to_str().context("script path is not UTF-8")?;
    let conn = Connection::session().context("connecting to the session bus")?;
    // A copy from an earlier host may still be loaded; replace it.
    conn.call_method(KWIN, "/Scripting", SCRIPTING, "unloadScript", &(name,))
        .context("asking KWin (is it running?)")?;
    let id: i32 = conn
        .call_method(KWIN, "/Scripting", SCRIPTING, "loadScript", &(file, name))?
        .body()
        .deserialize()?;
    anyhow::ensure!(id >= 0, "KWin refused to load {}", path.display());
    let script = format!("/Scripting/Script{id}");
    conn.call_method(
        KWIN,
        script.as_str(),
        Some("org.kde.kwin.Script"),
        "run",
        &(),
    )?;
    Ok(())
}
