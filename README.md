# COSMIC Fancy Pants

FancyZones-style window snapping for the COSMIC desktop.

Define your own zone layouts, then hold **Shift** while dragging a window: the
layout appears over the desktop, the zone under the cursor highlights, and
releasing snaps the window to fill it. Hovering near the edge shared by two
adjacent zones targets both, snapping the window across their combined area.

This is a fork of [pop-os/cosmic-comp](https://github.com/pop-os/cosmic-comp),
plus a standalone layout editor.

See [INSTALL.md](INSTALL.md) to install and configure it.

## Why this is a compositor fork

The obvious shape for this feature is a separate application, and that is not
possible. Wayland gives no client the ability to move or resize another
client's windows, and COSMIC's own `zcosmic_toplevel_manager_v1` protocol
exposes only close, activate, maximize, minimize, fullscreen and
move-to-workspace — no geometry control at all.

Everything the feature needs — intercepting the drag, hit-testing the cursor,
drawing the overlay, and placing the window — happens inside the compositor's
pointer grab. So the zone logic ships as a modified `cosmic-comp`.

The editor *is* a normal Wayland client, because all it does is write config.

## What changed

Around 2000 lines against upstream `d3ffa814`, most of it new files. The diff
stays small because COSMIC already had most of the machinery: `MoveGrab`
hit-tested edge-snap zones during a drag, rendered a translucent preview, and
applied it on release. Zones generalize that rather than duplicating it.

### The keystone change

Windows already carried a persistent snap state, `floating_tiled`, holding one
of eight fixed `TiledCorners` positions. That became:

```rust
pub enum FloatingTiled {
    Corner(TiledCorners),                                        // half and quarter snapping
    Zone { layout: String, zones: Vec<usize>, rect: ZoneRect },  // user-defined
}
```

Both resolve to a rectangle through the same method. Making zones a *peer* of
corner snapping rather than a parallel mechanism is what keeps the diff small:
every existing consumer of the snap state — unmaximize, unminimize, restore,
cross-workspace moves — handles zones without knowing they exist.

### New files

| Path | Purpose |
| --- | --- |
| `cosmic-comp-config/src/zones.rs` | Layout model and pure fractional geometry, shared with the editor |
| `src/shell/zones/mod.rs` | Bridge from that model to compositor state: layout resolution, pixel conversion, hit-testing |
| `cosmic-fancy-pants-editor/` | The editor, its own crate and workspace |

### Modified upstream files

| Path | Change |
| --- | --- |
| `src/shell/layout/floating/mod.rs` | `FloatingTiled`, fraction→pixel conversion, `snap_to` |
| `src/shell/grabs/moving.rs` | Modifier detection, zone hit-testing, overlay rendering, drop placement |
| `src/shell/mod.rs` | Cached zone config, app→zone memory on map |
| `src/shell/element/mod.rs`, `src/shell/workspace.rs` | Snap state type change |
| `src/input/mod.rs`, `src/input/actions.rs` | Zone shortcut matching and handling |
| `src/config/` | `ZoneAction`, config plumbing |
| `src/backend/render/mod.rs` | One shader-key variant for the overlay |

Zones are stored as **fractions of the output's non-exclusive area**, never
pixels, so a layout survives resolution changes, fractional scaling and monitor
swaps, and automatically respects panels and docks.

## Design decisions worth knowing

Some behaviour is deliberate and would otherwise look like a bug:

- **Spanning triggers on proximity to a shared edge**, not on where the drag
  started. Anchoring at the drag origin would turn every drag that merely
  crosses zones into a span.
- **Overlapping zones suppress spanning** and resolve to the smallest zone
  containing the cursor. A bounding box over stacked zones is never intended.
- **Editing a built-in template forks it** to a custom layout rather than
  editing in place, which would silently redefine that template for every
  monitor using it, with no undo.
- **App→zone memory is off by default**, and is ignored when the remembered
  layout is no longer the one assigned. Zone 1 of a three-column layout is a
  different region of the screen under a 2×2 grid.
- **Movement shortcuts default to Ctrl+Alt+arrows.** COSMIC binds every arrow
  shortcut it ships with Super, so Ctrl+Alt is free and nothing is shadowed; a
  test asserts these never claim Super.
- **Zone geometry matches `TiledCorners` exactly on even-sized outputs**,
  including odd gap values, so zone- and corner-snapped windows line up. It
  deliberately differs on odd-sized outputs, where upstream's integer
  truncation leaves a pixel unused against the far edge.
- **A snap carries the gap it was made with**, so placement uses the spacing the
  drag overlay previewed. Deriving it from the theme at placement time landed
  windows with different spacing than the user had just been shown.
- **Output lookup falls back to connector name.** Wayland does not expose EDID
  to clients, so the editor writes `edid: None` while the compositor's key
  carries the monitor's real EDID; an exact match silently disabled zones on any
  monitor that reports one.
- **Only the configured modifiers must be held**, not matched exactly. Super is
  how COSMIC starts a window move, so requiring it to be absent made zones
  unreachable from that gesture.

## Relationship to upstream

Development happens on `fancy-zones`, with upstream as a remote:

```sh
git remote add upstream https://github.com/pop-os/cosmic-comp.git
git fetch upstream && git rebase upstream/master
```

`git diff upstream/master...HEAD` is the complete feature, kept extractable as
a patch series should this ever be proposed upstream.

The editor is a **separate cargo workspace** with its own lockfile. Adding it as
a workspace member forced tokio, mio and libc upgrades into the compositor's
pinned dependency tree and failed to resolve at all. Decoupling them keeps
`Cargo.lock` byte-identical to upstream's; the two share only
`cosmic-comp-config`.

## Building

```sh
cargo build --release                                              # compositor
cargo build --release --manifest-path cosmic-fancy-pants-editor/Cargo.toml
cargo test && (cd cosmic-fancy-pants-editor && cargo test)
```

The compositor runs nested inside an existing session for development, which
avoids logging out to test a change:

```sh
COSMIC_BACKEND=winit cargo run
```

## Known gaps

- Workspace layouts are assigned by keyboard shortcut; the editor has no UI for
  it. The `ext-workspace` protocol does expose the ids this would need.
- Zone numbers are drawn in the editor but not in the drag overlay.
- Changing the gap does not re-layout windows already snapped, since each snap
  records the gap it was made with.

## License

GPL-3.0-only, matching upstream `cosmic-comp`.
