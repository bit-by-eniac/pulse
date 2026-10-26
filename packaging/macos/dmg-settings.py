# dmgbuild settings for the Pulse disk image, used by scripts/build-macos.sh:
#   dmgbuild -s packaging/macos/dmg-settings.py -D app=<Pulse.app> -D volume_icon=<.icns> Pulse <out.dmg>
# dmgbuild writes the Finder window layout itself, so no Finder scripting is needed.
# Paths are relative to the repository root. The background is 640x400 points with
# the icons where icon_locations puts them (scripts/make-dmg-background.py).
import os.path

app = defines["app"]  # noqa: F821 (set by dmgbuild)

format = "UDZO"
files = [app]
symlinks = {"Applications": "/Applications"}
icon = defines["volume_icon"]  # noqa: F821
background = "packaging/macos/dmg-background.jpg"

# The height includes Finder's 32-point title bar.
window_rect = ((200, 140), (640, 432))
default_view = "icon-view"
icon_size = 128
text_size = 13
icon_locations = {os.path.basename(app): (170, 185), "Applications": (470, 185)}
