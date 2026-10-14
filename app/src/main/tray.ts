// Tray icon: open the editor, pause wallpaper animation, quit. On Linux it
// is a StatusNotifierItem, shown by bars that host one.

import { Menu, nativeImage, Tray } from "electron";

export interface TrayActions {
  open(): void;
  setPaused(paused: boolean): void;
  quit(): void;
}

export interface TrayState {
  /** The host is running, so animation can be paused. */
  connected: boolean;
  paused: boolean;
}

export class AppTray {
  private readonly tray: Tray;

  constructor(
    iconPath: string,
    private readonly actions: TrayActions,
    state: TrayState,
  ) {
    let icon = nativeImage.createFromPath(iconPath);
    // The PNG is 256 px; tray hosts on Linux scale a large pixmap poorly.
    if (process.platform !== "win32") icon = icon.resize({ width: 32, height: 32 });
    this.tray = new Tray(icon);
    this.tray.setToolTip("Rinka");
    // Windows opens the menu on right click; a left click opens the editor.
    this.tray.on("click", () => actions.open());
    this.update(state);
  }

  update(state: TrayState): void {
    this.tray.setContextMenu(
      Menu.buildFromTemplate([
        { label: "Open Rinka", click: () => this.actions.open() },
        { type: "separator" },
        {
          label: "Pause animation",
          type: "checkbox",
          checked: state.paused,
          enabled: state.connected,
          click: (item) => this.actions.setPaused(item.checked),
        },
        { type: "separator" },
        { label: "Quit editor (wallpapers keep running)", click: () => this.actions.quit() },
      ]),
    );
  }

  destroy(): void {
    this.tray.destroy();
  }
}
