// Plasma desktop script, run through org.kde.PlasmaShell.evaluateScript.
// The host defines `show` before this: true switches every desktop (all
// screens and activities) to Rinka's wallpaper plugin and remembers the
// plugin it replaced, false switches back to that plugin, null only reports.
// The replaced plugin is kept in the containment's own `Rinka` group: written
// into a `Wallpaper` group, the key would also land in the configuration of
// the wallpaper still loaded.
// Prints `{switched, desktops}`: how many desktops changed to Rinka's plugin,
// and each desktop's screen, plugin and screen geometry.
const RINKA = "dev.rinka.wallpaper";
let switched = 0;

const list = desktops().map(desktop => {
    if (show === true && desktop.wallpaperPlugin !== RINKA) {
        switched += 1;
        const previous = desktop.wallpaperPlugin;
        desktop.wallpaperPlugin = RINKA;
        desktop.currentConfigGroup = ["Rinka"];
        desktop.writeConfig("PreviousPlugin", previous);
    } else if (show === false && desktop.wallpaperPlugin === RINKA) {
        desktop.currentConfigGroup = ["Rinka"];
        desktop.wallpaperPlugin = desktop.readConfig("PreviousPlugin", "") || "org.kde.image";
    }
    const geometry = desktop.screen >= 0 ? screenGeometry(desktop.screen) : null;
    return {
        screen: desktop.screen,
        plugin: desktop.wallpaperPlugin,
        geometry: geometry ? [geometry.x, geometry.y, geometry.width, geometry.height] : null,
    };
});
print(JSON.stringify({ switched, desktops: list }));
