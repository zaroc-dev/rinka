# KDE Plasma

Measured 2026-11-09 with Plasma 6.8.2 and KWin 6.8.2 (NixOS), NVIDIA RTX
4070, Vulkan. Plasma ran headless (`kwin_wayland --virtual --no-lockscreen
plasmashell`) in an isolated home and session bus; screenshots came from
KWin's screenshot interface (`spectacle -b -n -f`). The recipe is in
`.agents/testing.md`. A real Plasma login has not been tested.

## Why Rinka was hidden

Plasma 6's desktop window (icons, widgets and Plasma's own wallpaper) is
itself a background layer-shell surface, namespace `desktop`, which KWin
treats as a desktop window. Rinka's surfaces share KWin's desktop layer with
it and stacked below it: with the host presenting (`presenting: true`), the
screen still showed Plasma's wallpaper.

Two more things were in the way:

- **Opaque desktop window.** Plasma creates the window black. Qt then sends
  a full-output opaque region for it once, and does not clear it when the
  color later becomes transparent (`QWaylandWindow::isOpaque` reads the
  requested format, which only gains alpha through `setColor` and only
  affects windows created afterwards). KWin draws nothing below an opaque
  region, so a transparent desktop chosen while Plasma runs stays black.
  The buffer itself is ARGB (`AR24`); only the region blocks.
- **Stacking on creation.** KWin puts a new window that is not activated
  under the active window, and above everything in its layer when the active
  window is in another layer (`Workspace::restackWindowUnderActive`). A
  Rinka surface created while an application is active therefore landed
  above the desktop window and hid the icons.

## What Rinka does

| Piece | What it does |
| --- | --- |
| Wallpaper plugin `dev.rinka.wallpaper` (`crates/rinka-host/plasma/wallpaper`) | Draws nothing and binds the desktop window's color to transparent. Restores the color when another plugin is chosen. Shows a restart note when chosen while its window is already showing. Installed to `share/plasma/wallpapers` by the Nix packages and the RPM; the host writes a user copy when no package provides it. |
| KWin script (`plasma/keep-below-desktop.js`) | Loaded by `rinka-host serve` over D-Bus while it runs (per control socket). After each `rinka-host` window appears it raises the desktop windows. Unloaded on shutdown. |
| `plasma_desktop {show_rinka?}` | Runs `plasma/desktops.js` in plasmashell: reports each desktop's plugin and screen geometry; `true` switches all desktops (every screen and activity) to the plugin and records the previous plugin in the containment's `[Rinka]` group; `false` restores it. After a switch the host restarts `plasma-plasmashell.service` when systemd runs plasmashell, otherwise replies `restart: "needed"`. |
| Editor | A library notice while Plasma covers a wallpaper that is presenting, and a "Plasma desktop" section in Settings with the state per display and both actions. |

## Measurements

| Check | Result |
| --- | --- |
| Before (Image plugin) | Teal `#1e6f73` assigned, presenting; screen showed Plasma's wallpaper. KWin stacking: `rinka-host` (layer 0) below `plasmashell` desktop (layer 0). |
| Plugin chosen at plasmashell start | Screen reads (30,111,115), the exact color. Panel and desktop icon draw on top. |
| Plugin chosen while running | Black; WAYLAND_DEBUG shows the desktop surface's `set_opaque_region(0,0,1280,720)` from creation and no later update. Hiding and showing the window recreated the layer surface on the same `wl_surface`, so the region stayed. After restarting plasmashell: (30,111,115). |
| Restart note | Shown on the black desktop in the live-switch case; not shown at startup (the window is not yet visible when the plugin loads). |
| Switch back | Image, then Plain Color as the previous plugin: each restored. A second switch to Rinka reports `restart: null`. |
| Stacking with an app active | Without the script: new `rinka-host` window above the desktop window. With it: below, also after the surface is recreated with Kate active. |
| Video | 1280×720 H.264 video plays (NVDEC) under the desktop icon and Kate. |
| Two outputs | Both show the wallpaper; Plasma's screen geometry matches the host's logical outputs (`[0,0,1920,1080]`, `[1920,0,1920,1080]`), which is how the editor maps desktops to outputs. |
| Fractional scale | Output at 1.5 (kscreen-doctor): host buffer 1920×1080 for 1280×720 logical, screen pixels (30,111,115). |
| Editor | Notice names both covered outputs; its button switched both desktops; Settings lists "Virtual-0 Rinka, Virtual-1 Rinka" and the restart note (no systemd in the test session). |

## Not working or not verified

- **Hidden pause.** KWin sends frame callbacks to every surface on an
  output, covered or not (`Item::framePainted`), so a wallpaper under a
  fullscreen window keeps playing: a video stayed at `hidden: false` under
  fullscreen Konsole. Only outputs KWin stops painting (powered off) can
  pause. A KWin script could report covered outputs to the host; not done.
- **Overview and desktop grid.** KWin's overview draws one desktop window as
  its background (`DesktopBackgroundItem`), now transparent, so the overview
  background is plain grey. Desktop thumbnails show the wallpaper.
- **Rinka not running** with the plugin selected: the desktop is black.
- **Not verified:** the systemd restart path (the test session's bus has no
  systemd), a real Plasma login, `XDG_CURRENT_DESKTOP` reaching the
  home-manager user service, the lock screen (Plasma's lock screen keeps its
  own wallpaper setting), desktop edit mode, multiple activities, physical hotplug, and the
  RPM's plugin path on Fedora KDE.
