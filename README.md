# HeroBar

A lightweight status bar for Wayland and X11, part of [HeroineOS](https://github.com/HeroineOS)
and built on [HeroUI](https://github.com/HeroineOS/HeroUI). It's a standalone program:
use it with HeroWM, or in your own sway/Hyprland/KDE rice.

- **Native Wayland panel** (wlr-layer-shell) on HeroWM, sway, Hyprland, KDE and other
  compositors that support it; a dock window with reserved space on X11. On GNOME, which
  has no layer-shell, it runs on XWayland.
- **Light:** 2.4 MB of its own memory (about 16 MB RSS with shared libraries), 0% CPU
  with a clock ticking every second, 1.6 MB binary. Measured on Wayland (sway).
- **Configured in TOML**, in the same style as the HeroWM compositor's config.
- **Taskbar** with pinned apps and open windows: from HeroWM's IPC, or the
  wlr-foreign-toplevel-management protocol (sway, Hyprland, labwc, river, Wayfire).
- **Icons** that follow the theme (battery level, Wi-Fi/wired, volume...), app icons
  from your icon theme.
- **Islands** (optional): each module on its own background, sharp, rounded or pill,
  with see-through gaps (Wayland).

## Install

Debian packages for amd64 and arm64 are attached to the
[releases](https://github.com/HeroineOS/HeroBar/releases) (built on Debian stable; they
also install on testing):

```sh
sudo apt install ./herobar_0.1.5-1_arm64.deb
herobar &
```

Start it from your compositor's autostart (e.g. `exec herobar` in sway). It applies
changes to `~/.config/hero/bar.toml` and the HeroUI theme within a second (it reloads
itself; a config with errors is reported and ignored until fixed). Appearance edits both.

## Modules

| Name | Shows | Placeholders |
|---|---|---|
| `clock` | date/time | strftime: `%H:%M`, `%a %d %b`... (`man 3 strftime`) |
| `cpu` | CPU usage since the last update | `{usage}` |
| `memory` | RAM in use | `{used}` `{total}` (GiB), `{percent}` |
| `battery` | battery level (hidden without a battery) | `{capacity}` `{status}` |
| `network` | interface of the default route | `{ifname}` `{state}` |
| `volume` | default output volume, via wpctl or pactl (hidden without audio) | `{volume}` |
| `taskbar` | pinned apps and open windows | (see below) |
| `custom/<name>` | fixed `text`, or the first line `exec` prints | |

Any module can have `interval = <seconds>`, `on-click` and `icon` (a built-in icon, an
icon theme name or an image path; `""` for none). Built-in modules have icons by
default; battery, network and volume change theirs with their state. A built-in module
with nothing to show (no battery, no audio) takes no space.

The taskbar shows pinned apps and open windows. Click to open or focus an app (again
to cycle through its windows), middle click to close a window.

```toml
[modules.taskbar]
show = "both"            # "running", "pinned" or "both"
style = "icons"          # one button per app; "icons-titles": one per window, with titles
pinned = ["foot", "firefox-esr"]   # .desktop file names
max-width = 600          # the most room it takes; buttons shrink to fit
fixed-width = false      # true: always max-width, so other modules never move
```

## Configuration

`~/.config/hero/bar.toml` (or `$XDG_CONFIG_HOME/hero/bar.toml`). Without one, the
built-in default is used. Start from it:

```sh
mkdir -p ~/.config/hero
herobar --print-default-config > ~/.config/hero/bar.toml
herobar --check     # validate after editing
```

The default config ([`res/bar.toml`](res/bar.toml)) is commented and shows every option.
In short:

```toml
[bar]
position = "top"          # or "bottom"
height = 34
reserve-space = true
modules-left = ["custom/menu"]
modules-center = ["clock"]
modules-right = ["cpu", "memory"]

[style]                   # overrides of the shared HeroUI theme
background = "#14141c"

[modules.clock]
format = "%a %d %b  %H:%M"

[modules."custom/menu"]
icon = "apps"
text = "HeroineOS"
on-click = { action = "run-command", arg = "heroappearance" }   # or on-click = "heroappearance"
```

Islands: `islands = true` and `island-style = "sharp" | "rounded" | "pill"` in `[bar]`.
The gaps are see-through on Wayland (through the fltk-sys fork); on X11 the islands sit
on the bar.

Colors and fonts default to the shared HeroUI theme (`~/.config/heroui/theme.conf`), so
the bar matches other HeroUI programs. Unknown keys and module names are errors, reported
with their line; at startup the bar then falls back to the default config instead of not
starting, and while running it keeps the last good config.

## Building

```sh
cargo build --release      # target/release/herobar
cargo deb                  # target/debian/herobar_*.deb (cargo install cargo-deb)
```

Needs Rust, CMake, a C++ compiler, and on Debian: `libx11-dev libxext-dev libxft-dev
libxinerama-dev libxcursor-dev libxrender-dev libxfixes-dev libpango1.0-dev libcairo2-dev
libgl-dev libwayland-dev wayland-protocols libxkbcommon-dev libdbus-1-dev`.

Cargo.toml patches `fltk-sys` with [HeroineOS/fltk-sys](https://github.com/HeroineOS/fltk-sys)
(fltk-sys 1.5.23 plus layer-shell, touch and transparent windows), because upstream FLTK
supports none of them on Wayland.

Targets: x86_64, aarch64, i686 and armv7 Linux (CI builds and tests x86_64 and aarch64).

## License

MIT OR Apache-2.0.
