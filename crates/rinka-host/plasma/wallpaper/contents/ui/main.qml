// Rinka draws wallpapers on its own background layer surfaces, below
// Plasma's desktop window. This plugin draws nothing and makes that window
// transparent, so the wallpaper shows through while icons, widgets and
// panels stay on top. Switching to another plugin restores the window's
// color.
import QtQuick
import QtQuick.Window
import org.kde.plasma.plasmoid
import org.kde.kirigami as Kirigami

WallpaperItem {
    id: root

    // A desktop window created opaque keeps telling KWin it is opaque (Qt
    // sets its opaque region once), so KWin draws nothing below it and the
    // desktop stays black until plasmashell restarts. That happens when this
    // plugin is chosen while the window is already showing; at startup the
    // window becomes transparent before it is shown.
    property bool restartNeeded: false

    Component.onCompleted: restartNeeded = root.Window.window !== null && root.Window.window.visible

    Binding {
        target: root.Window.window
        when: root.Window.window !== null
        property: "color"
        value: "transparent"
    }

    Text {
        visible: root.restartNeeded
        anchors.centerIn: parent
        width: Math.min(parent.width - Kirigami.Units.gridUnit * 4, Kirigami.Units.gridUnit * 30)
        horizontalAlignment: Text.AlignHCenter
        wrapMode: Text.Wrap
        color: "white"
        font.pointSize: Kirigami.Theme.defaultFont.pointSize * 1.2
        text: "Rinka's wallpaper appears here after Plasma restarts. Log out and back in, or run: systemctl --user restart plasma-plasmashell"
    }
}
