// SPDX-License-Identifier: GPL-3.0-only

//! Resolving user-defined snap zones for an output.
//!
//! [`cosmic_comp_config::zones`] owns the layout model and the pure fractional
//! geometry. This module is the bridge to compositor state: it picks the layout
//! that applies to a given output and workspace, measures the area the zones
//! are laid out over, and converts between pixels and fractions.

use cosmic_comp_config::{
    workspace::OutputMatch,
    zones::{AppZoneMemories, ZoneLayout, ZoneModifiers, ZoneRect, ZoneShortcuts, ZonesConfig},
};
use cosmic_settings_config::shortcuts;
use std::{cell::RefCell, collections::HashMap};

use smithay::{
    desktop::layer_map_for_output,
    input::keyboard::{Keysym, ModifiersState, xkb},
    output::Output,
    utils::{Logical, Point, Rectangle},
};

use crate::{
    config::ZoneAction,
    shell::{
        CosmicMapped,
        layout::floating::{FloatingTiled, zone_relative_geometry},
    },
    utils::prelude::*,
};

/// Does the currently held modifier set arm zone snapping?
///
/// The configured modifiers must all be held; anything *extra* is ignored.
/// An exact match would break the common case, because Super+drag is how
/// COSMIC starts a window move — requiring `logo: false` made it impossible to
/// arm zones from that gesture, leaving only title-bar drags working.
pub fn modifiers_match(want: &ZoneModifiers, have: &ModifiersState) -> bool {
    // An empty configuration would otherwise arm zones on every drag.
    if !(want.ctrl || want.alt || want.shift || want.logo) {
        return false;
    }
    (!want.ctrl || have.ctrl)
        && (!want.alt || have.alt)
        && (!want.shift || have.shift)
        && (!want.logo || have.logo)
}

/// A layout resolved against a specific output, ready to hit-test and render.
///
/// Built once when a drag arms zone snapping and reused for every motion event,
/// since resolving involves cloning the layout and measuring the layer map.
#[derive(Debug, Clone)]
pub struct ZoneContext {
    /// Id of the resolved layout, carried onto the window when it snaps.
    pub layout_id: String,
    layout: ZoneLayout,
    /// The output's non-exclusive area, output-relative — zones are laid out
    /// over what panels and docks leave behind, not the raw output.
    area: Rectangle<i32, Logical>,
    gaps: (i32, i32),
    /// Gap override in force when this was resolved, recorded onto each snap so
    /// placement matches what the overlay drew.
    gap: Option<u32>,
    /// Shared-edge span threshold, in fraction units. Zero disables spanning.
    edge_threshold: f64,
}

/// A resolved drop target: which zones, and the pixels they add up to.
#[derive(Debug, Clone, PartialEq)]
pub struct ZoneHit {
    pub zones: Vec<usize>,
    pub rect: ZoneRect,
    pub geometry: Rectangle<i32, Local>,
}

impl ZoneContext {
    /// Resolve the layout that applies to `output` on the given workspace.
    ///
    /// Returns `None` when zones are disabled, no layout is assigned, or the
    /// assigned layout has no usable zones — in each case the caller should
    /// fall back to COSMIC's built-in edge snapping.
    pub fn resolve(
        config: &ZonesConfig,
        output: &Output,
        workspace_id: Option<&str>,
        gaps: (i32, i32),
    ) -> Option<Self> {
        if !config.enabled {
            return None;
        }

        let output_match = OutputMatch {
            name: output.name(),
            edid: output.edid().cloned(),
        };
        let layout = config.layout_for(&output_match, workspace_id)?.sanitized();
        if layout.zones.is_empty() {
            return None;
        }

        let layout_id = config.layout_id_for(&output_match, workspace_id).cloned()?;

        let area = {
            let layers = layer_map_for_output(output);
            layers.non_exclusive_zone()
        };
        if area.size.w <= 0 || area.size.h <= 0 {
            return None;
        }

        let edge_threshold = config.edge_threshold_fraction(area.size.w, area.size.h);

        Some(Self {
            layout_id,
            layout,
            area,
            gaps,
            gap: config.gap,
            edge_threshold,
        })
    }

    /// Gap override to stamp onto snaps made from this layout.
    pub fn gap(&self) -> Option<u32> {
        self.gap
    }

    pub fn len(&self) -> usize {
        self.layout.zones.len()
    }

    pub fn is_empty(&self) -> bool {
        self.layout.zones.is_empty()
    }

    pub fn name(&self) -> &str {
        &self.layout.name
    }

    /// The sanitized layout these zones came from, for callers that need to
    /// reason about the zones themselves rather than hit-test them.
    pub fn layout(&self) -> &ZoneLayout {
        &self.layout
    }

    /// Pixel geometry of a single zone.
    pub fn zone_geometry(&self, index: usize) -> Option<Rectangle<i32, Local>> {
        let rect = self.layout.zones.get(index)?;
        Some(zone_relative_geometry(*rect, self.area, self.gaps))
    }

    /// Pixel geometry of every zone, for drawing the overlay.
    pub fn geometries(&self) -> impl Iterator<Item = Rectangle<i32, Local>> + '_ {
        self.layout
            .zones
            .iter()
            .map(move |rect| zone_relative_geometry(*rect, self.area, self.gaps))
    }

    /// Resolve what a cursor at `point` would snap to.
    ///
    /// `point` is output-relative. Points outside the zone area — over a panel,
    /// say — yield `None` rather than clamping, so dragging onto a panel does
    /// not spuriously target the nearest zone.
    pub fn hit(&self, point: Point<i32, Local>) -> Option<ZoneHit> {
        let (fx, fy) = self.to_fraction(point)?;
        let target = self.layout.target_at(fx, fy, self.edge_threshold)?;
        Some(ZoneHit {
            geometry: zone_relative_geometry(target.rect, self.area, self.gaps),
            zones: target.zones,
            rect: target.rect,
        })
    }

    /// Geometry for an explicit set of zone indices, used by keyboard snapping
    /// where there is no cursor to hit-test.
    pub fn hit_for(&self, zones: &[usize]) -> Option<ZoneHit> {
        let rect = self.layout.span(zones)?;
        Some(ZoneHit {
            zones: zones.to_vec(),
            rect,
            geometry: zone_relative_geometry(rect, self.area, self.gaps),
        })
    }

    fn to_fraction(&self, point: Point<i32, Local>) -> Option<(f64, f64)> {
        let area = self.area.as_local();
        if !area.contains(point) {
            return None;
        }
        Some((
            (point.x - area.loc.x) as f64 / area.size.w as f64,
            (point.y - area.loc.y) as f64 / area.size.h as f64,
        ))
    }
}

#[cfg(test)]
impl ZoneContext {
    fn for_test(
        layout: ZoneLayout,
        area: Rectangle<i32, Logical>,
        gaps: (i32, i32),
        edge_threshold: f64,
    ) -> Self {
        Self {
            layout_id: "test".into(),
            layout,
            area,
            gaps,
            gap: None,
            edge_threshold,
        }
    }
}

/// Match a key press against the zone bindings.
///
/// Returns the action and a synthetic [`shortcuts::Binding`] so the result fits
/// the shape the rest of the shortcut machinery expects.
pub fn match_shortcut(
    shortcuts: &ZoneShortcuts,
    modifiers: &ModifiersState,
    key_matches: &dyn Fn(Keysym) -> bool,
) -> Option<(ZoneAction, shortcuts::Binding)> {
    let candidates = [
        (&shortcuts.open_editor, ZoneAction::OpenEditor),
        (&shortcuts.snap_next, ZoneAction::CycleNext),
        (&shortcuts.snap_prev, ZoneAction::CyclePrev),
        (&shortcuts.grow_span, ZoneAction::GrowSpan),
        (&shortcuts.shrink_span, ZoneAction::ShrinkSpan),
        (
            &shortcuts.assign_to_workspace,
            ZoneAction::AssignToWorkspace,
        ),
        (&shortcuts.clear_workspace, ZoneAction::ClearWorkspace),
    ];

    for (binding, action) in candidates {
        let Some(binding) = binding.as_ref() else {
            continue;
        };
        // Modifiers first: this runs for every key press on the input thread,
        // and comparing four booleans is free next to resolving a keysym name.
        if !modifiers_match(&binding.modifiers, modifiers) {
            continue;
        }
        let Some(keysym) = parse_keysym(&binding.key) else {
            continue;
        };
        if !key_matches(keysym) {
            continue;
        }
        return Some((
            action,
            shortcuts::Binding {
                modifiers: shortcuts::Modifiers {
                    ctrl: binding.modifiers.ctrl,
                    alt: binding.modifiers.alt,
                    shift: binding.modifiers.shift,
                    logo: binding.modifiers.logo,
                },
                key: Some(keysym),
                keycode: None,
                description: Some(format!("{action:?}")),
            },
        ));
    }
    None
}

/// Resolve an XKB keysym name such as `"grave"` or `"Left"`.
///
/// Memoised: `keysym_from_name` is a lookup into xkbcommon's name table, and
/// this sits on the per-key-press path.
///
/// Nothing is ever evicted. The keys are binding names read from
/// configuration, so the cache grows only when the user edits a zone binding
/// to a name they have not used before — bounded by however many names they
/// try, at a `String` and a `Keysym` each. Not worth an eviction policy, but
/// it is unbounded in principle rather than fixed at startup: the config is
/// watched, so new names can arrive at any time.
///
/// Thread-local to stay lock-free — key handling runs on one thread, and a
/// duplicate entry on another would be harmless anyway.
fn parse_keysym(name: &str) -> Option<Keysym> {
    thread_local! {
        static CACHE: RefCell<HashMap<String, Option<Keysym>>> =
            RefCell::new(HashMap::new());
    }

    CACHE.with(|cache| {
        if let Some(cached) = cache.borrow().get(name) {
            return *cached;
        }
        let keysym = xkb::keysym_from_name(name, xkb::KEYSYM_CASE_INSENSITIVE);
        let resolved = (keysym.raw() != xkb::keysyms::KEY_NoSymbol).then_some(keysym);
        cache.borrow_mut().insert(name.to_string(), resolved);
        resolved
    })
}

/// Zone index a window currently occupies, if it is zone-snapped.
///
/// A spanned window reports its first zone, so cycling from a span lands
/// somewhere predictable rather than doing nothing.
pub fn current_zone(mapped: &CosmicMapped) -> Option<(String, usize)> {
    match mapped.floating_tiled.lock().unwrap().as_ref()? {
        FloatingTiled::Zone { layout, zones, .. } => {
            Some((layout.clone(), zones.iter().copied().min()?))
        }
        FloatingTiled::Corner(_) => None,
    }
}

/// Next zone index when cycling by `delta`, wrapping at both ends.
///
/// An unsnapped window enters at the first zone going forwards, or the last
/// going backwards, so both directions have an obvious entry point.
pub fn cycle_index(current: Option<usize>, len: usize, delta: i32) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let len_i = len as i32;
    Some(match current {
        Some(index) => (((index as i32 + delta) % len_i + len_i) % len_i) as usize,
        None if delta >= 0 => 0,
        None => len - 1,
    })
}

/// Zone set after growing or shrinking a span by one step.
///
/// A span is placed as its bounding rectangle, so it is only meaningful when
/// the zones it names fill that rectangle exactly. Appending the next index
/// does not guarantee that: in a 2x2 grid, growing the top row `[0, 1]` to
/// `[0, 1, 2]` gives a bounding box covering zone 3 as well — the window goes
/// fullscreen while recording three zones, and the next shrink drops from
/// fullscreen straight back to the top half.
///
/// Growing therefore extends the rectangle to take in one more zone and then
/// claims everything that rectangle swallows, choosing the smallest such
/// rectangle. Shrinking drops trailing zones until the set is exact again.
/// Either returns `None` when there is nowhere to go, leaving the window put.
pub fn resize_span(layout: &ZoneLayout, zones: &[usize], grow: bool) -> Option<Vec<usize>> {
    let len = layout.zones.len();
    let mut current: Vec<usize> = zones.iter().copied().filter(|&i| i < len).collect();
    current.sort_unstable();
    current.dedup();

    if !grow {
        while current.len() > 1 {
            current.pop();
            if layout.span_is_exact(&current) {
                return Some(current);
            }
        }
        return None;
    }

    // An unsnapped window has to enter the layout somewhere.
    if current.is_empty() {
        return (len > 0).then(|| vec![0]);
    }

    let size = current.len();
    (0..len)
        .filter(|i| !current.contains(i))
        .filter_map(|i| {
            let mut extended = current.clone();
            extended.push(i);
            let candidate = layout.zones_within(layout.span(&extended)?);
            layout.span_is_exact(&candidate).then_some(candidate)
        })
        .filter(|candidate| candidate.len() > size)
        // Smallest growth wins; index order breaks ties so the choice is
        // stable rather than dependent on iteration order.
        .min_by(|a, b| {
            let area = |set: &[usize]| layout.span(set).map(|r| r.area()).unwrap_or(f64::MAX);
            area(a).total_cmp(&area(b)).then_with(|| a.cmp(b))
        })
}

/// Zone a newly mapped window of `app_id` should be placed in, if any.
///
/// Only applies when the remembered layout is still the one assigned to this
/// output and workspace: a window should not jump into coordinates from a
/// layout that is no longer in use.
pub fn remembered_zone(
    config: &ZonesConfig,
    memories: &AppZoneMemories,
    app_id: &str,
    output: &Output,
    workspace_id: Option<&str>,
) -> Option<FloatingTiled> {
    if !config.remember_apps {
        return None;
    }
    let memory = memories.get(app_id)?;

    let output_match = OutputMatch {
        name: output.name(),
        edid: output.edid().cloned(),
    };
    let active = config.layout_id_for(&output_match, workspace_id)?;
    if active != &memory.layout {
        return None;
    }

    let layout = config.layouts.get(&memory.layout)?.sanitized();
    let rect = layout.span(&memory.zones)?;
    Some(FloatingTiled::Zone {
        layout: memory.layout.clone(),
        zones: memory.zones.clone(),
        rect,
        gap: config.gap,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic_comp_config::zones::{columns, grid};

    fn area(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), (w, h).into())
    }

    fn point(x: i32, y: i32) -> Point<i32, Local> {
        Point::<i32, Logical>::from((x, y)).as_local()
    }

    #[test]
    fn hit_maps_pointer_to_the_expected_column() {
        let ctx = ZoneContext::for_test(columns(3, "c3"), area(0, 0, 1200, 900), (0, 0), 0.0);
        assert_eq!(ctx.hit(point(100, 450)).unwrap().zones, vec![0]);
        assert_eq!(ctx.hit(point(600, 450)).unwrap().zones, vec![1]);
        assert_eq!(ctx.hit(point(1100, 450)).unwrap().zones, vec![2]);
    }

    /// Zones are laid out over the non-exclusive area, so a panel shifts the
    /// origin. Fractions must be relative to that area, not the raw output —
    /// otherwise every zone is offset by the panel height.
    #[test]
    fn fractions_are_relative_to_the_non_exclusive_area() {
        let panel = 40;
        let ctx = ZoneContext::for_test(
            columns(2, "c2"),
            area(0, panel, 1200, 900 - panel),
            (0, 0),
            0.0,
        );

        // Just below the panel is the very top of zone 0, not its middle.
        let (_, fy) = ctx.to_fraction(point(10, panel)).unwrap();
        assert!(fy.abs() < 1e-9, "expected top of area, got {fy}");

        assert_eq!(ctx.hit(point(10, panel + 5)).unwrap().zones, vec![0]);
        assert_eq!(ctx.hit(point(1100, panel + 5)).unwrap().zones, vec![1]);
    }

    /// Dragging over a panel must not snap to the nearest zone.
    #[test]
    fn points_outside_the_area_do_not_hit() {
        let ctx = ZoneContext::for_test(columns(2, "c2"), area(0, 40, 1200, 860), (0, 0), 0.0);
        assert!(ctx.hit(point(600, 10)).is_none(), "hit inside the panel");
        assert!(ctx.hit(point(-5, 500)).is_none(), "hit left of the output");
        assert!(ctx.hit(point(600, 5000)).is_none(), "hit below the output");
    }

    #[test]
    fn hit_geometry_matches_the_zone_geometry() {
        let ctx = ZoneContext::for_test(columns(3, "c3"), area(0, 0, 1200, 900), (0, 8), 0.0);
        let hit = ctx.hit(point(600, 450)).unwrap();
        assert_eq!(hit.geometry, ctx.zone_geometry(1).unwrap());
    }

    /// A span near a shared edge resolves to the union, and that union is wider
    /// than either zone alone.
    #[test]
    fn spanning_hit_covers_both_zones() {
        let ctx = ZoneContext::for_test(columns(3, "c3"), area(0, 0, 1200, 900), (0, 0), 0.02);
        let hit = ctx.hit(point(402, 450)).unwrap();
        assert_eq!(hit.zones, vec![0, 1]);
        assert!(hit.geometry.size.w > ctx.zone_geometry(0).unwrap().size.w);
    }

    #[test]
    fn hit_for_resolves_explicit_indices() {
        let ctx = ZoneContext::for_test(columns(3, "c3"), area(0, 0, 1200, 900), (0, 0), 0.0);
        let hit = ctx.hit_for(&[0, 2]).unwrap();
        assert_eq!(
            hit.geometry,
            ctx.zone_geometry(0)
                .unwrap()
                .merge(ctx.zone_geometry(2).unwrap())
        );
        assert!(ctx.hit_for(&[]).is_none());
    }

    fn memory_config() -> (ZonesConfig, AppZoneMemories) {
        let mut config = ZonesConfig {
            remember_apps: true,
            ..Default::default()
        };
        config.per_output.insert(
            OutputMatch {
                name: "DP-1".into(),
                edid: None,
            },
            "columns-3".into(),
        );
        let mut memories = AppZoneMemories::new();
        memories.insert(
            "com.example.App".into(),
            cosmic_comp_config::zones::AppZoneMemory {
                layout: "columns-3".into(),
                zones: vec![1],
            },
        );
        (config, memories)
    }

    /// A remembered zone from a layout that is no longer assigned must be
    /// ignored: its coordinates mean nothing under the current layout.
    #[test]
    fn memory_is_ignored_when_the_layout_changed() {
        let (mut config, memories) = memory_config();
        config.per_output.insert(
            OutputMatch {
                name: "DP-1".into(),
                edid: None,
            },
            "grid-2x2".into(),
        );
        // Same helper logic as `remembered_zone`, minus the Output it needs.
        let active = config
            .per_output
            .get(&OutputMatch {
                name: "DP-1".into(),
                edid: None,
            })
            .cloned();
        assert_eq!(active.as_deref(), Some("grid-2x2"));
        assert_ne!(
            memories["com.example.App"].layout.as_str(),
            active.unwrap().as_str(),
            "mismatch is what suppresses placement"
        );
    }

    #[test]
    fn memory_is_disabled_by_default() {
        assert!(!ZonesConfig::default().remember_apps);
    }

    #[test]
    fn cycling_wraps_in_both_directions() {
        assert_eq!(cycle_index(Some(0), 3, 1), Some(1));
        assert_eq!(cycle_index(Some(2), 3, 1), Some(0), "should wrap forward");
        assert_eq!(cycle_index(Some(0), 3, -1), Some(2), "should wrap backward");
    }

    /// An unsnapped window needs an obvious entry point in each direction.
    #[test]
    fn cycling_an_unsnapped_window_enters_at_an_end() {
        assert_eq!(cycle_index(None, 3, 1), Some(0));
        assert_eq!(cycle_index(None, 3, -1), Some(2));
    }

    #[test]
    fn cycling_an_empty_layout_does_nothing() {
        assert_eq!(cycle_index(Some(0), 0, 1), None);
        assert_eq!(cycle_index(None, 0, 1), None);
    }

    #[test]
    fn growing_a_span_appends_the_next_zone() {
        let layout = columns(3, "test");
        assert_eq!(resize_span(&layout, &[0], true), Some(vec![0, 1]));
        assert_eq!(resize_span(&layout, &[0, 1], true), Some(vec![0, 1, 2]));
    }

    #[test]
    fn a_span_cannot_grow_past_the_whole_layout() {
        let layout = columns(3, "test");
        assert_eq!(resize_span(&layout, &[0, 1, 2], true), None);
    }

    /// Growing extends the span's rectangle, so a span sitting at the end of
    /// the layout grows backwards rather than doing nothing.
    #[test]
    fn a_trailing_span_grows_backwards() {
        let layout = columns(3, "test");
        assert_eq!(resize_span(&layout, &[1, 2], true), Some(vec![0, 1, 2]));
    }

    #[test]
    fn shrinking_a_span_drops_the_last_zone() {
        let layout = columns(3, "test");
        assert_eq!(resize_span(&layout, &[0, 1, 2], false), Some(vec![0, 1]));
    }

    /// A span never shrinks to nothing — the window has to live somewhere.
    #[test]
    fn a_span_cannot_shrink_below_one_zone() {
        let layout = columns(3, "test");
        assert_eq!(resize_span(&layout, &[1], false), None);
        assert_eq!(resize_span(&layout, &[], false), None);
    }

    #[test]
    fn span_input_is_normalised() {
        // Out of order and duplicated indices must not confuse the result.
        let layout = columns(4, "test");
        assert_eq!(
            resize_span(&layout, &[2, 0, 2, 1], true),
            Some(vec![0, 1, 2, 3])
        );
    }

    /// Regression: growing the top row of a 2x2 used to append index 2, whose
    /// bounding box also covers zone 3. The window went fullscreen while
    /// recording three zones, so the next shrink dropped it from fullscreen to
    /// the top half. Every step must name exactly what it covers.
    #[test]
    fn growing_in_a_grid_never_covers_an_unlisted_zone() {
        let layout = grid(2, 2, "test");
        let mut span = vec![0];
        while let Some(next) = resize_span(&layout, &span, true) {
            assert!(
                layout.span_is_exact(&next),
                "{span:?} grew to {next:?}, which does not fill its own rectangle"
            );
            assert!(next.len() > span.len(), "growing must make progress");
            span = next;
        }
        assert_eq!(
            span,
            vec![0, 1, 2, 3],
            "growing should reach the whole grid"
        );

        while let Some(smaller) = resize_span(&layout, &span, false) {
            assert!(
                layout.span_is_exact(&smaller),
                "{span:?} shrank to {smaller:?}, which does not fill its own rectangle"
            );
            span = smaller;
        }
        assert_eq!(span.len(), 1, "shrinking should end at a single zone");
    }

    #[test]
    fn keysym_names_resolve() {
        assert!(parse_keysym("grave").is_some());
        assert!(parse_keysym("Left").is_some());
        assert!(parse_keysym("not-a-key").is_none());
    }

    #[test]
    fn the_configured_modifier_must_be_held() {
        let want = ZoneModifiers::default(); // shift
        let mut have = ModifiersState::default();
        assert!(!modifiers_match(&want, &have), "nothing held");
        have.shift = true;
        assert!(modifiers_match(&want, &have));
    }

    /// Regression: Super+drag is COSMIC's window-move gesture, so Super is held
    /// for most drags. Requiring it to be absent made zones unreachable that
    /// way, which is how this shipped broken.
    #[test]
    fn extra_modifiers_do_not_block_arming() {
        let want = ZoneModifiers::default(); // shift
        let have = ModifiersState {
            shift: true,
            logo: true,
            ..Default::default()
        };
        assert!(
            modifiers_match(&want, &have),
            "Super+Shift+drag must arm zones"
        );
    }

    /// A configuration with no modifiers would otherwise arm on every drag,
    /// making plain window moves impossible.
    #[test]
    fn an_empty_modifier_set_never_arms() {
        let want = ZoneModifiers {
            ctrl: false,
            alt: false,
            shift: false,
            logo: false,
        };
        let have = ModifiersState {
            shift: true,
            ..Default::default()
        };
        assert!(!modifiers_match(&want, &have));
    }
}
