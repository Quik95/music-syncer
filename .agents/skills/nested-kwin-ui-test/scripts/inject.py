"""Inject pointer/keyboard input into a KWin compositor via org_kde_kwin_fake_input.

Needs WAYLAND_DISPLAY pointing at a KWin started with KWIN_WAYLAND_NO_PERMISSION_CHECKS=1
and the generated protocol bindings directory as argv[1].

usage: inject.py PROTO_DIR COMMAND [ARGS...]
  move X Y          absolute pointer position in logical pixels
  click [N]         left click, N times in a row (2 = double-click)
  right | middle    single click of that button
  back | forward    mouse side buttons
  key KEY...        tap keys in sequence; KEY is a name (enter, ctrl+shift+tab) or evdev code
"""

import sys
import time

KEYS = {
    "esc": 1, "backspace": 14, "tab": 15, "enter": 28, "ctrl": 29, "shift": 42,
    "alt": 56, "space": 57, "f2": 60, "f5": 63, "home": 102, "up": 103,
    "pageup": 104, "left": 105, "right": 106, "end": 107, "down": 108,
    "pagedown": 109, "delete": 111, "super": 125,
}  # fmt: skip
KEYS.update({chr(c): code for c, code in zip(b"qwertyuiop", range(16, 26))})
KEYS.update({chr(c): code for c, code in zip(b"asdfghjkl", range(30, 39))})
KEYS.update({chr(c): code for c, code in zip(b"zxcvbnm", range(44, 51))})
KEYS.update({str(d): 2 + (d - 1) % 10 for d in range(10)})

BUTTONS = {"left": 0x110, "right": 0x111, "middle": 0x112, "back": 0x113, "forward": 0x114}
PRESSED, RELEASED = 1, 0


def key_code(name: str) -> int:
    return int(name) if name.isdigit() and len(name) > 1 else KEYS[name.lower()]


def main() -> None:
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    proto_dir, cmd, *args = sys.argv[1:]
    sys.path.insert(0, proto_dir)
    from fake_input import OrgKdeKwinFakeInput
    from pywayland.client import Display

    display = Display()
    display.connect()
    found = {}

    def on_global(registry, name, interface, version):
        if interface == "org_kde_kwin_fake_input":
            found["fi"] = registry.bind(name, OrgKdeKwinFakeInput, min(version, 4))

    registry = display.get_registry()
    registry.dispatcher["global"] = on_global
    display.roundtrip()
    if "fi" not in found:
        sys.exit("compositor does not expose org_kde_kwin_fake_input")
    fi = found["fi"]
    fi.authenticate("uitest", "automated UI test")

    def sync() -> None:
        # Separate events in time so clients see distinct frames; double-click
        # detection still works because the gap stays far below its threshold.
        display.roundtrip()
        time.sleep(0.05)

    def tap(button: int, count: int = 1) -> None:
        for _ in range(count):
            fi.button(button, PRESSED)
            sync()
            fi.button(button, RELEASED)
            sync()

    if cmd == "move":
        fi.pointer_motion_absolute(float(args[0]), float(args[1]))
        sync()
    elif cmd == "click":
        tap(BUTTONS["left"], int(args[0]) if args else 1)
    elif cmd in ("right", "middle", "back", "forward"):
        tap(BUTTONS[cmd])
    elif cmd == "key":
        for combo in args:
            codes = [key_code(part) for part in combo.split("+")]
            for code in codes:
                fi.keyboard_key(code, PRESSED)
                sync()
            for code in reversed(codes):
                fi.keyboard_key(code, RELEASED)
                sync()
    else:
        sys.exit(__doc__)
    display.disconnect()


if __name__ == "__main__":
    main()
