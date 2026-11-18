---
name: nested-kwin-ui-test
description: >-
  Verify GUI behavior of this app end to end: launch it in a headless nested KWin
  (Plasma's compositor) with an isolated HOME, inject mouse and keyboard input, and
  take screenshots. Use after any UI change (clicks, double-clicks, keybindings, focus,
  scrolling, layout, rendering) instead of running the app on the user's desktop, and
  whenever input must be simulated: injecting into the real session can hit other windows.
---

# Nested KWin UI testing

`scripts/uitest` runs a private session and nothing touches the user's desktop:

- `kwin_wayland --virtual` with no output window and its own Wayland socket;
- a private `dbus-daemon`, so screenshots and D-Bus stay inside the session;
- `HOME`, `XDG_*_HOME` and `XDG_RUNTIME_DIR` under the sandbox dir
  (`$TMPDIR/<repo>-uitest`), so the app's config, state and logs go there too;
- input goes through KWin's `org_kde_kwin_fake_input`, never through uinput/ydotool.

## Workflow

```sh
U=.agents/skills/nested-kwin-ui-test/scripts/uitest
$U start                          # 1280x800; `start 1920 1080` for another size
H=$($U home)                      # build fixtures here, never in the real HOME
mkdir -p "$H/Music/Album A/CD1" && touch "$H/Music/Album A/01.flac"
$U app "$H/Music" "$H"            # cargo build, then launch in the background
$U shot                           # prints the PNG path; read it to see the screen
$U input move 180 162             # logical pixels, 1:1 with screenshot pixels
$U input click 2                  # double-click
$U input key down down enter      # named keys; combos like shift+tab
$U input back                     # mouse side button (BTN_SIDE)
$U shot
$U log                            # app stdout/stderr
$U stop                           # kills everything and deletes the sandbox
```

`uitest app` again restarts the app with new arguments and keeps the session.
`RUST_LOG=music_syncer=debug $U app …` passes the log level through.

## Getting reliable results

- Take a screenshot first and pick coordinates from it. KWin places the window
  itself; with the default size the 1000x600 window lands at about (140, 85).
- Confirm every step with the logs as well as pixels: the app writes its log
  file to `$H/.local/state/music-syncer/`, so you can match the state changes
  you expected.
- When a step "does nothing", check the fixture before blaming the app. Example:
  in a directory whose last entry is a file, `end` then `enter` correctly opens
  nothing.
- Leave about 0.3 s between steps that redraw (`sleep 0.3`). One `input key a b c`
  sends its keys back to back, which is fine for navigation but can race with
  async work.
- Clicking empty space inside a column focuses it without selecting a row; use it
  to set keyboard focus.
- Copy screenshots out of the sandbox before `stop` if they need to survive.

## Input reference

| Command | Effect |
|---|---|
| `move X Y` | absolute pointer position |
| `click [N]` | left click N times (2 = double-click) |
| `right`, `middle`, `back`, `forward` | single click of that button |
| `key KEY...` | each KEY tapped in turn; `ctrl+shift+tab` holds modifiers; letters, digits, `enter`, `esc`, `tab`, `space`, `backspace`, `delete`, arrows, `home`, `end`, `pageup`, `pagedown`, `f2`, `f5`, or a raw evdev code (2+ digits) |

There is no mouse-wheel command. KWin's fake input only sends continuous
`wl_pointer.axis` with no axis source, and gpui ignores those while its source is
`Wheel` (the default) because it waits for discrete steps
(`gpui_linux/src/linux/wayland/client.rs`). Scroll with keys (`pagedown`, `end`)
instead.

## Troubleshooting

- `kwin exited` / no socket: read `$TMPDIR/<repo>-uitest/kwin.log`.
- `app exited`: `uitest log`. Rendering needs the devShell's libraries, so the
  app always runs through `nix develop`; the devShell itself keeps the real HOME
  so flake inputs are not refetched.
- `session already running`: a previous run was not stopped; `uitest stop`.
- First `start` builds pywayland and the protocol XMLs from the `nixpkgs`
  registry entry; later starts hit the store.
