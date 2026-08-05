# COSMIC Fancy Pants — FancyZones for COSMIC / Pop!_OS

## Context

COSMIC ships two window paradigms — auto-tiling and floating with edge-snap to
halves/quarters — but nothing like Windows PowerToys FancyZones: user-authored
zone layouts that a window drops into on a modifier-drag.

**The central constraint that shapes this entire plan:** this cannot be an
external application. Wayland gives no client the ability to move or resize
another client's windows, and COSMIC's own `zcosmic_toplevel_manager_v1` exposes
only close / activate / maximize / minimize / fullscreen / move-to-workspace —
no geometry control. The drag grab, the hit-testing, and the overlay rendering
all live inside the compositor's pointer grab. So the zone logic must ship as a
modified `cosmic-comp`.

The good news from reading the source: cosmic-comp is already ~90% of the way
there structurally. `src/shell/grabs/moving.rs` already has a `SnappingZone`
enum that hit-tests the cursor during a drag, renders a translucent preview
rectangle with `IndicatorShader` + `BackdropShader`, and applies the result on
grab drop. Windows already carry a persistent `floating_tiled:
Arc<Mutex<Option<TiledCorners>>>` snap state that survives maximize, minimize,
and workspace moves. FancyZones is, structurally, the generalization of
`TiledCorners` from a fixed 8-variant enum into an arbitrary user-defined
rectangle. Almost all of the work is that one generalization plus an editor.

Target: the COSMIC 1.0 install on this machine (`cosmic-comp 1.0.0`, Pop!_OS
24.04). Repo is already GPL-3, matching upstream.

**Decisions taken:** fork with upstream remote · fullscreen layer-shell editor ·
overlay shows all zones with the hovered one highlighted · v1 includes zone
spanning, keyboard cycling, per-workspace layouts, and app→zone memory.

## Repo shape

Pull `pop-os/cosmic-comp` history into this repo, develop on a `fancy-zones`
branch, keep `upstream` as a remote for periodic rebase. Keeping the diff
rebase-able matters: it is the path to an eventual upstream PR.

```
cosmic-fancy-pants/
  src/…                       fork of cosmic-comp
    shell/zones/mod.rs        NEW — zone resolution + hit-testing engine
  cosmic-comp-config/
    src/zones.rs              NEW — shared data model (compositor + editor)
  cosmic-fancy-pants-editor/  NEW — workspace member, layer-shell editor app
```

`cosmic-comp-config` is already a workspace member and already the crate that
holds serde config types, so the zone model belongs there — it is the natural
shared dependency between compositor and editor.

## Data model — `cosmic-comp-config/src/zones.rs`

Zones are stored as **fractions of the output's non-exclusive area**, not
pixels. This is resolution-independent, survives monitor and scale changes, and
matches the shape of what `TiledCorners::relative_geometry` already returns.

```rust
pub struct ZoneRect { pub x: f64, pub y: f64, pub w: f64, pub h: f64 } // 0.0..=1.0
pub struct ZoneLayout { pub name: String, pub zones: Vec<ZoneRect> }

pub struct ZonesConfig {
    pub enabled: bool,
    pub modifier: ZoneModifiers,                   // default: shift
    pub spanning: bool,
    pub adjacent_highlight_distance: u32,          // shared-edge span threshold
    pub show_zone_numbers: bool,
    pub inactive_opacity: u8,                      // default 50, per FancyZones
    pub layouts: HashMap<String, ZoneLayout>,      // layout id -> layout
    pub per_output: HashMap<OutputMatch, String>,  // output -> layout id
    pub per_workspace: HashMap<String, String>,    // workspace id -> layout id (wins)
    pub app_memory: HashMap<String, (String, Vec<usize>)>, // app_id -> layout, zones
    pub remember_apps: bool,
    pub shortcuts: ZoneShortcuts,
}
```

Reuse `cosmic_comp_config::workspace::OutputMatch` (name + EDID) for monitor
identity — the same struct pinned workspaces already use to survive replugging.
Two details verified against the source:

- `OutputMatch` derives only `Debug, Clone, PartialEq, Eq, Serialize,
  Deserialize` (`cosmic-comp-config/src/workspace.rs:55`), so it cannot be a
  `HashMap` key as-is. Add a `Hash` derive — `String` and `EdidProduct` are both
  already `Hash`, so it is a one-word change and upstream-friendly. (RON handles
  struct map keys, unlike JSON, so serialization is fine.)
- `ZoneModifiers` is defined locally in `zones.rs` rather than reusing
  `cosmic_settings_config::shortcuts::Modifiers`, because `cosmic-comp-config`
  does not depend on `cosmic-settings-config` and the editor depends on this
  crate — pulling that in would bloat the editor's tree. It is the same four
  bools; convert at the compositor boundary.

Add `zones: ZonesConfig` as a field on `CosmicCompConfig`
(`cosmic-comp-config/src/lib.rs:69`, which derives `CosmicConfigEntry`). `cosmic-config` is a
per-key file store, so this automatically becomes a `zones` key under
`com.system76.CosmicComp/v1/` that the editor can write independently, and the
`ConfigWatchSource` already registered in `Config::load` (`src/config/mod.rs:171`)
picks up live edits — add a `"zones"` arm to `config_changed`
(`src/config/mod.rs:787`).

Ship built-in templates as defaults: 2/3 columns, 2×2 grid, priority grid
(narrow-wide-narrow), and focus (large centered + side stack).

## Compositor changes

### 1. Generalize the snap state (the keystone change)

`CosmicMapped::floating_tiled` (`src/shell/element/mod.rs:108`) becomes:

```rust
pub enum FloatingTiled {
    Corner(TiledCorners),
    Zone { layout: String, zones: Vec<usize>, rect: ZoneRect },
}
impl FloatingTiled {
    pub fn relative_geometry(&self, output_geometry: Rectangle<i32, Logical>,
                             gaps: (i32, i32)) -> Rectangle<i32, Local>;
}
```

Doing it this way — rather than adding a parallel zone field — is what makes the
rest cheap. Every existing consumer of the snap state keeps working unchanged
and gains zone support for free: unmaximize restore, minimize/unminimize restore,
and cross-workspace move all already round-trip this value
(`src/shell/workspace.rs:1027`, `:1102`, `:1188`; `FloatingRestoreData.was_snapped`
at `:307`).

Touch points: `Animation::geometry`'s `tiled_state` param
(`src/shell/layout/floating/mod.rs:134`), `FloatingLayout::map`'s read at `:307`
(already calls `relative_geometry`, so mapping works with no further change),
`snap_to_corner` at `:1672` → rename `snap_to`, `move_element`'s state machine at
`:1184` (arrow-key snapping stays `Corner`-only), and the `Option<TiledCorners>`
fields in `workspace.rs`.

### 2. Zone engine — `src/shell/zones/mod.rs` (new)

Pure geometry, no compositor state. This is where the unit tests go.

- Resolve the active layout for an `(Output, Workspace)` pair: per-workspace
  mapping wins, else per-output, else none.
- Fraction → pixels against `layer_map_for_output(output).non_exclusive_zone()`
  with theme gaps applied — the same inputs `SnappingZone::overlay_geometry`
  already uses, so zones respect panels and docks automatically.
- Hit-test `Point<i32, Local>` → `Option<usize>`.
- Span: bounding box over a set of zone indices.

### 3. Drag interception — `src/shell/grabs/moving.rs`

Add `zone_state: Option<ZoneDragState>` to `MoveGrabState` (`:56`), holding the
resolved zone rects, `hovered: Option<usize>`, and `span_anchor: Option<usize>`.
Zone mode and the existing `snapping_zone` edge-snap are mutually exclusive, so
plain drags behave exactly as they do today.

In `MoveGrab::update_location` (`:371`), inside the existing
`previous == ManagedLayer::Floating` block that currently computes
`snapping_zone` (`:470`): poll `seat.get_keyboard().unwrap().modifier_state()`
and compare against the configured modifier with `cosmic_modifiers_eq_smithay`
(`src/config/key_bindings.rs`). Polling modifiers this way is established
practice in this codebase (`src/config/mod.rs:831`, `src/xwayland.rs:421`).
When the modifier is held, hit-test instead of edge-snapping.

**Spanning** follows FancyZones' actual semantics, which are edge-proximity
based rather than anchor based:

1. *Primary — shared-edge hover.* When the cursor is within
   `adjacent_highlight_distance` of the edge shared by two adjacent zones, both
   activate and the target is their bounding box. This is the discoverable
   gesture and needs no extra key.
2. *Secondary — Ctrl accumulate.* Holding Ctrl while already in zone mode adds
   the hovered zone to the selected set, giving arbitrary multi-zone spans.

An earlier draft of this plan anchored the span at the zone where zone mode was
entered. That is wrong: it would turn every drag that merely crosses zones into
a span. Do not implement it that way.

**Overlapping zones:** the model permits arbitrary rects, so zones may overlap
(FancyZones' Canvas layouts do, and it has a dedicated setting for this). For
v1, hit-testing resolves ties by smallest area containing the point — the
intuitive "most specific zone wins".

### 4. Overlay rendering — `MoveGrabState::render` (`:70`)

Extend the existing `snapping_zone` render block at `:223`. When zone mode is
active, push a low-alpha `BackdropShader::element` for every zone, then the
target rect at higher alpha with an `IndicatorShader::element` border. The
shader-key type is `Key::Window(Usage, CosmicMappedKey)`
(`src/backend/render/mod.rs:138-152`), and each zone needs a distinct key — add
`Usage::ZoneIndicator(u8)` to the `Usage` enum.

### 5. Drop placement — `Drop for MoveGrab` (`:789`)

In the `ManagedLayer::Floating` arm, alongside the existing `snapping_zone`
branch at `:861`: when a zone target exists, set
`*window.floating_tiled.lock().unwrap() = Some(FloatingTiled::Zone { … })` and
apply the geometry via the generalized `snap_to`. Preserve the existing
`pre_drag_geometry` save/restore dance at `:865`/`:908` so that un-snapping
returns the window to where the user actually had it.

### 6. Keyboard cycling

`shortcuts::Action` lives in the external `cosmic-settings-config` crate, so
adding variants there would mean maintaining a *second* fork. Avoid that: define
bindings in `ZonesConfig::shortcuts` and match them in
`State::filter_keyboard_input` (`src/input/mod.rs:1622`) *before* the standard
shortcut loop at `:1915`, reusing `cosmic_modifiers_eq_smithay`. Actions:
snap-to-zone-N, cycle next/prev zone, grow/shrink span, open editor.

### 7. App→zone memory

On zone drop, record `app_id → (layout, zones)` (debounced config write). In
`FloatingLayout::map` (`:341`), when a window is first mapped and
`remember_apps` is set, look up its `app_id` and pre-set `floating_tiled` if the
remembered layout matches the active one. Gate behind a config toggle — silent
auto-placement is surprising when it is wrong.

### 8. Per-workspace layouts — known constraint

`Workspace.id` is `Option<String>` and is only populated for **pinned**
workspaces (`src/shell/workspace.rs:112`, `:402`; `to_pinned` at `:455`
`debug_assert!`s it is `Some`). So: lazily assign an id via
`random_workspace_id()` (`:79`) when a workspace is first given a zone layout,
and fall back to the per-output layout when there is no id. Worth confirming
this interacts sanely with dynamic workspace creation/destruction during
implementation.

## Editor — `cosmic-fancy-pants-editor`

libcosmic app opening one fullscreen layer surface per output, so you drag
splitters on the real screen at true size. `cosmic-workspaces-epoch` is the
working reference for this exact pattern:

```rust
cosmic::surface::action::simple_layer_shell(…, move || SctkLayerSurfaceSettings {
    layer: Layer::Top,
    anchor: Anchor::all(),          // cover the whole output
    size: Some((None, None)),       // fullscreen
    keyboard_interactivity: KeyboardInteractivity::Exclusive,
    namespace: "cosmic-fancy-pants-editor".into(),
    output: IcedOutput::Output(output.clone()),
    ..Default::default()
})
```

Enumerate outputs from `WaylandEvent::Output` and create a surface per output.
Canvas with draggable zone rectangles: drag edges to resize, drag interior to
move, split/merge, snap-to-grid, numeric entry, plus a template picker. Save
writes `ZonesConfig` through `cosmic_config`; the compositor's watcher applies
it live with no restart.

Ship a `.desktop` entry and bind it to a zone shortcut.

## Build order

1. Fork setup — merge upstream history, `fancy-zones` branch, confirm a clean
   `cargo build` before touching anything.
2. `zones.rs` data model + templates + serde round-trip tests.
3. Zone engine + `FloatingTiled` generalization. Compiles and passes tests with
   no behavior change yet — a good checkpoint.
4. Drag interception + overlay rendering.
5. Drop placement + restore semantics.
6. Editor app.
7. Keyboard cycling, per-workspace layouts, app memory.

## Verification

**Unit tests.** cosmic-comp has essentially no test infrastructure (one test, in
`src/backend/kms/device.rs`), so put the automated coverage in
`cosmic-comp-config` where the logic is pure: fraction→rect conversion under
various output sizes/scales/gaps, cursor hit-testing including boundaries and
gaps, span bounding boxes, and config serde round-trips. `cargo test -p
cosmic-comp-config`.

**Nested compositor — the main dev loop.** cosmic-comp runs nested inside the
running COSMIC session; `init_backend` falls back to winit when `WAYLAND_DISPLAY`
is set (`src/backend/mod.rs:26-39`), or force it with `COSMIC_BACKEND=winit`.
`cargo run` opens a compositor in a window; launch a client into it via its
`WAYLAND_DISPLAY` and exercise shift-drag without logging out. This covers
everything except real multi-monitor behavior.

**Full session.** `make && sudo make install` writes `/usr/bin/cosmic-comp`.
Back up the packaged binary first, and note that `apt upgrade` of the
`cosmic-comp` package will overwrite it — installing to `/usr/local/bin`
instead is the safer default, since `cosmic-session` resolves `cosmic-comp` via
PATH. Keep a TTY available to recover from a compositor that fails to start.

**Manual checklist:** shift-drag into each zone of each template; span across
adjacent zones; verify a zoned window survives maximize→restore,
minimize→restore, and workspace move; verify plain (unmodified) drag still does
today's half/quarter edge snapping; verify zones respect panel/dock exclusive
areas; verify live config reload while the editor saves; verify multi-monitor
with differing resolutions and fractional scaling.
