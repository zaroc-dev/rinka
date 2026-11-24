import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

Kirigami.FormLayout {
    // Read by Plasma's wallpaper settings page.
    property var configDialog
    property var wallpaperConfiguration

    QQC2.Label {
        Layout.maximumWidth: Kirigami.Units.gridUnit * 24
        text: "Choose wallpapers for each display in the Rinka app. If Rinka is not running, this desktop shows black."
        wrapMode: Text.Wrap
    }
}
