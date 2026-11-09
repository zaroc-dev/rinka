// KWin script, loaded by rinka-host while it runs on Plasma.
//
// Rinka's wallpaper surfaces and Plasma's desktop window share KWin's
// desktop layer. A new surface lands above the desktop window whenever a
// window from another layer is active, and would then hide the desktop's
// icons and widgets. Raising the desktop windows after each Rinka surface
// appears keeps Rinka underneath.
function raiseDesktops() {
    for (const window of workspace.stackingOrder) {
        if (window.desktopWindow) {
            workspace.raiseWindow(window);
        }
    }
}

workspace.windowAdded.connect(window => {
    if (window.resourceClass === "rinka-host") {
        raiseDesktops();
    }
});
raiseDesktops();
