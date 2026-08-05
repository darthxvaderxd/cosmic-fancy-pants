# Installing COSMIC Fancy Pants

Zones live inside the compositor, so using them means running a patched
`cosmic-comp` in place of the packaged one. The editor on its own does nothing:
stock `cosmic-comp` ignores the `zones` config key.

## Build dependencies

On Pop!_OS or Ubuntu:

```sh
sudo apt install build-essential pkg-config libudev-dev libgbm-dev libdrm-dev \
    libegl1-mesa-dev libinput-dev libseat-dev libdisplay-info-dev \
    libpixman-1-dev libxkbcommon-dev libwayland-dev libsystemd-dev
```

Every one of those backs a smithay backend the compositor enables, so a missing
package fails the build in a `*-sys` crate rather than at link time:

| Package | Needed by |
| --- | --- |
| `libudev-dev` | device enumeration, `backend_udev` |
| `libgbm-dev`, `libdrm-dev` | `backend_gbm`; `gbm.pc` requires `libdrm` |
| `libegl1-mesa-dev` | `backend_egl`, and the editor's EGL surface |
| `libinput-dev` | `backend_libinput` |
| `libseat-dev` | `backend_session_libseat` |
| `libdisplay-info-dev` | EDID parsing |
| `libpixman-1-dev` | `renderer_pixman`, the software fallback |
| `libxkbcommon-dev`, `libwayland-dev` | keymaps and the Wayland socket |
| `libsystemd-dev` | the default `systemd` feature; drop it with `--no-default-features` |

Ubuntu 24.04 ships `libdisplay-info` 0.1 against a 0.3 crate, which is fine —
the crate detects the installed version and selects its API at build time.

Rust comes from `rustup`, not apt: the toolchain is pinned in
`rust-toolchain.toml` and apt's `rustc` is older than the pin.

## Install

Both binaries go to `/usr/local/bin`, which precedes `/usr/bin` in the session
`PATH`. `cosmic-session` resolves `cosmic-comp` by name, so the patched build
shadows the packaged one without modifying or removing it — and an `apt upgrade`
of the `cosmic-comp` package cannot clobber it.

```sh
cargo build --release
cargo build --release --manifest-path cosmic-fancy-pants-editor/Cargo.toml

sudo install -Dm755 target/release/cosmic-comp /usr/local/bin/cosmic-comp
sudo install -Dm755 \
    cosmic-fancy-pants-editor/target/release/cosmic-fancy-pants-editor \
    /usr/local/bin/cosmic-fancy-pants-editor
sudo install -Dm644 \
    cosmic-fancy-pants-editor/data/dev.nilfactor.CosmicFancyPantsEditor.desktop \
    /usr/share/applications/dev.nilfactor.CosmicFancyPantsEditor.desktop
```

Then log out and back in. Verify what is running:

```sh
readlink -f /proc/$(pgrep -x cosmic-comp)/exe   # expect /usr/local/bin/cosmic-comp
```

## Uninstall

```sh
sudo rm -f /usr/local/bin/cosmic-comp \
           /usr/local/bin/cosmic-fancy-pants-editor \
           /usr/share/applications/dev.nilfactor.CosmicFancyPantsEditor.desktop
```

Log out and back in; the packaged `/usr/bin/cosmic-comp` takes over again.

## If the session will not start

A compositor that fails to start leaves you without a desktop, so know the way
out before installing. Switch to a TTY with **Ctrl+Alt+F3**, log in, and:

```sh
sudo rm /usr/local/bin/cosmic-comp
sudo systemctl restart display-manager
```

That is the whole recovery: the packaged compositor is untouched throughout.

## Using it

Zones are opt-in per monitor — nothing changes until a layout is assigned.

1. Open **Zone Editor** from the app library, or press **Super+Shift+`**.
2. Pick a template and shape it, then **Save**:
   - **Drag a boundary** to resize the zones either side of it.
   - **Click inside a zone** to split it at that point. It splits along the
     longer side, so a wide zone becomes two columns and a tall one two rows;
     **Shift+click** picks the other axis.
   - **Right-click a boundary** to delete it, merging the zones across it.
   - **Padding** sets the space between snapped windows.

   Editing a built-in template forks it to a custom layout rather than
   redefining the template everywhere it is used.
3. Hold **Shift** while dragging a window. The layout appears, the zone under
   the cursor highlights, and releasing snaps the window into it.

Hovering near the edge shared by two adjacent zones targets both, snapping the
window across their combined area.

Any modifier held to start the drag is ignored, so Super+drag — COSMIC's window
move gesture — works as well as dragging the title bar.

## Configuration

Settings live under the `zones` key of `com.system76.CosmicComp`:

```
~/.config/cosmic/com.system76.CosmicComp/v1/zones
```

The compositor watches this file, so edits apply without a restart.

| Key | Default | Meaning |
| --- | --- | --- |
| `enabled` | `true` | Master switch for zone snapping |
| `modifier` | `shift` | Modifier that arms zones during a drag |
| `spanning` | `true` | Allow a window to cover several adjacent zones |
| `adjacent_highlight_distance` | `16` | Pixels from a shared edge that activate both zones |
| `show_zone_numbers` | `true` | Draw zone numbers in the editor |
| `inactive_opacity` | `50` | Opacity of non-targeted zones in the drag overlay |
| `gap` | `None` | Space between snapped windows, in pixels. `None` follows COSMIC's global window gap |
| `remember_apps` | `false` | Re-open an app in the zone it last occupied |

A window records the gap it was snapped with, so changing `gap` affects new
snaps rather than re-laying out windows already sitting in zones.

### Keyboard shortcuts

Movement uses Ctrl+Alt, which COSMIC does not bind — every arrow shortcut it
ships uses Super — so nothing is shadowed. The workspace shortcuts are unbound,
because assignment has editor UI.

| Shortcut | Default | Action |
| --- | --- | --- |
| `open_editor` | **Super+Shift+`** | Launch the editor |
| `snap_next` / `snap_prev` | **Ctrl+Alt+→ / ←** | Move the focused window between zones |
| `grow_span` / `shrink_span` | **Ctrl+Alt+↑ / ↓** | Extend or contract across adjacent zones |
| `assign_to_workspace` | unbound | Pin the monitor's layout to the active workspace |
| `clear_workspace` | unbound | Drop that assignment, reverting to the monitor default |

Defaults only apply to a config that does not already have the key. An existing
config keeps whatever it has, including an explicit `None`.

A binding looks like:

```
snap_next: Some((
    modifiers: (ctrl: true, alt: true, shift: false, logo: false),
    key: "Right",
)),
```

`key` is an XKB keysym name — `Right`, `grave`, `w`.

## Known gaps

- Changing `gap` does not re-layout windows already snapped; they keep the
  spacing they were snapped with until moved again.
- Clearing a workspace assignment is shortcut-only; the editor can set one but
  not remove it.
