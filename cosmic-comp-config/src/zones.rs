// SPDX-License-Identifier: GPL-3.0-only

//! Zone layouts for FancyZones-style window snapping.
//!
//! Zones are stored as fractions of the output's *non-exclusive* area (the
//! region left over after panels and docks reserve their space), never as
//! pixels. That keeps a layout valid across resolution changes, fractional
//! scaling, and monitor swaps, and it is the same shape the compositor's
//! existing `TiledCorners::relative_geometry` already produces.
//!
//! Everything in this module is pure geometry over `f64` fractions, with no
//! compositor types, so it is unit-testable on its own and shared with the
//! zone editor.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::workspace::OutputMatch;

/// Fractional tolerance used when comparing edges for adjacency.
const EPSILON: f64 = 1e-6;

/// A rectangle in fractional output coordinates: `0.0..=1.0` on both axes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ZoneRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl ZoneRect {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }

    pub fn area(&self) -> f64 {
        self.w * self.h
    }

    /// A zone is usable if it is finite, has positive extent, and lies within
    /// the unit square (allowing for float slop).
    pub fn is_valid(&self) -> bool {
        [self.x, self.y, self.w, self.h]
            .iter()
            .all(|v| v.is_finite())
            && self.w > EPSILON
            && self.h > EPSILON
            && self.x >= -EPSILON
            && self.y >= -EPSILON
            && self.right() <= 1.0 + EPSILON
            && self.bottom() <= 1.0 + EPSILON
    }

    /// Clamp into the unit square, preserving positive extent where possible.
    pub fn clamped(&self) -> Self {
        let x = self.x.clamp(0.0, 1.0);
        let y = self.y.clamp(0.0, 1.0);
        Self {
            x,
            y,
            w: self.w.clamp(0.0, 1.0 - x),
            h: self.h.clamp(0.0, 1.0 - y),
        }
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// Grow by `d` on every side. Used for shared-edge span detection: a
    /// neighbouring zone's inflated rect contains the cursor exactly when the
    /// cursor is within `d` of their shared edge.
    pub fn inflated(&self, d: f64) -> Self {
        Self {
            x: self.x - d,
            y: self.y - d,
            w: self.w + d * 2.0,
            h: self.h + d * 2.0,
        }
    }

    /// Smallest rectangle containing both. This is what a multi-zone span
    /// resolves to.
    pub fn union(&self, other: &Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Self {
            x,
            y,
            w: self.right().max(other.right()) - x,
            h: self.bottom().max(other.bottom()) - y,
        }
    }

    pub fn overlaps(&self, other: &Self) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }
}

/// The resolved drop target for a drag: one or more zones and the rectangle
/// they add up to.
#[derive(Debug, Clone, PartialEq)]
pub struct ZoneTarget {
    /// Indices into [`ZoneLayout::zones`], ascending.
    pub zones: Vec<usize>,
    pub rect: ZoneRect,
}

impl ZoneTarget {
    pub fn is_span(&self) -> bool {
        self.zones.len() > 1
    }
}

/// A named set of zones.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZoneLayout {
    pub name: String,
    pub zones: Vec<ZoneRect>,
}

impl ZoneLayout {
    pub fn new(name: impl Into<String>, zones: Vec<ZoneRect>) -> Self {
        Self {
            name: name.into(),
            zones,
        }
    }

    /// Drop zones that are degenerate or out of bounds, so a hand-edited or
    /// stale config can't poison hit-testing.
    pub fn sanitized(&self) -> Self {
        Self {
            name: self.name.clone(),
            zones: self
                .zones
                .iter()
                .copied()
                .filter(|z| z.is_valid())
                .collect(),
        }
    }

    /// Innermost zone containing the point.
    ///
    /// Zones may overlap (the editor's canvas mode allows it), so ties are
    /// broken by smallest area — the most specific zone wins, which is what a
    /// user pointing at a small zone stacked on a large one intends.
    pub fn hit_test(&self, x: f64, y: f64) -> Option<usize> {
        self.zones
            .iter()
            .enumerate()
            .filter(|(_, z)| z.contains(x, y))
            .min_by(|(_, a), (_, b)| {
                a.area()
                    .partial_cmp(&b.area())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
    }

    /// Bounding rectangle of a set of zone indices.
    pub fn span(&self, indices: &[usize]) -> Option<ZoneRect> {
        let mut iter = indices.iter().filter_map(|&i| self.zones.get(i));
        let first = *iter.next()?;
        Some(iter.fold(first, |acc, z| acc.union(z)))
    }

    /// Resolve what a drag at this point should snap to.
    ///
    /// With `edge_threshold > 0`, hovering near the edge shared by adjacent
    /// zones activates all of them and targets their bounding box — this is
    /// FancyZones' primary spanning gesture, and it needs no extra modifier.
    ///
    /// Overlapping zones suppress spanning: if the point is strictly inside
    /// more than one zone the layout is a canvas-style stack, where a bounding
    /// box would be surprising, so the smallest containing zone wins instead.
    pub fn target_at(&self, x: f64, y: f64, edge_threshold: f64) -> Option<ZoneTarget> {
        let strict: Vec<usize> = self
            .zones
            .iter()
            .enumerate()
            .filter(|(_, z)| z.contains(x, y))
            .map(|(i, _)| i)
            .collect();

        if strict.len() > 1 {
            let i = self.hit_test(x, y)?;
            return Some(ZoneTarget {
                zones: vec![i],
                rect: self.zones[i],
            });
        }

        if edge_threshold <= 0.0 {
            let i = *strict.first()?;
            return Some(ZoneTarget {
                zones: vec![i],
                rect: self.zones[i],
            });
        }

        let near: Vec<usize> = self
            .zones
            .iter()
            .enumerate()
            .filter(|(_, z)| z.inflated(edge_threshold).contains(x, y))
            .map(|(i, _)| i)
            .collect();

        let rect = self.span(&near)?;
        Some(ZoneTarget { zones: near, rect })
    }
}

/// Which modifier arms zone snapping during a drag.
///
/// Structurally identical to `cosmic_settings_config::shortcuts::Modifiers`,
/// but defined here so this crate — and therefore the editor, which depends on
/// it — does not have to pull in the settings-config tree for four booleans.
/// Converted at the compositor boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ZoneModifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
}

impl Default for ZoneModifiers {
    fn default() -> Self {
        Self {
            ctrl: false,
            alt: false,
            shift: true,
            logo: false,
        }
    }
}

/// A key binding owned by this feature.
///
/// Zone shortcuts deliberately do not go through
/// `cosmic_settings_config::shortcuts::Action`: that enum lives in an external
/// repo, so extending it would mean maintaining a second fork. These are
/// matched in the compositor ahead of the standard shortcut table instead.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ZoneBinding {
    pub modifiers: ZoneModifiers,
    /// XKB keysym name, e.g. `"grave"`, `"Left"`, `"1"`.
    pub key: String,
}

impl ZoneBinding {
    pub fn new(modifiers: ZoneModifiers, key: impl Into<String>) -> Self {
        Self {
            modifiers,
            key: key.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZoneShortcuts {
    pub open_editor: Option<ZoneBinding>,
    pub snap_next: Option<ZoneBinding>,
    pub snap_prev: Option<ZoneBinding>,
    pub grow_span: Option<ZoneBinding>,
    pub shrink_span: Option<ZoneBinding>,
}

impl Default for ZoneShortcuts {
    fn default() -> Self {
        let logo_shift = ZoneModifiers {
            logo: true,
            shift: true,
            ..Default::default()
        };
        Self {
            // Mirrors FancyZones' Win+Shift+` for the editor.
            open_editor: Some(ZoneBinding::new(logo_shift, "grave")),
            snap_next: None,
            snap_prev: None,
            grow_span: None,
            shrink_span: None,
        }
    }
}

/// Where a given app's windows were last snapped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppZoneMemory {
    pub layout: String,
    pub zones: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZonesConfig {
    pub enabled: bool,
    /// Modifier that arms zone snapping mid-drag. Default: shift.
    pub modifier: ZoneModifiers,
    /// Allow a window to span several adjacent zones.
    pub spanning: bool,
    /// Distance from a shared edge, in logical pixels, at which both adjacent
    /// zones activate. Converted to a fraction against the output before use.
    pub adjacent_highlight_distance: u32,
    pub show_zone_numbers: bool,
    /// Opacity of non-targeted zones in the drag overlay, 0-100.
    pub inactive_opacity: u8,
    /// Auto-place a new window into the zone its app last occupied.
    pub remember_apps: bool,
    /// Layout id -> layout.
    pub layouts: HashMap<String, ZoneLayout>,
    /// Monitor -> layout id.
    pub per_output: HashMap<OutputMatch, String>,
    /// Workspace id -> layout id. Takes precedence over `per_output`.
    pub per_workspace: HashMap<String, String>,
    /// App id -> last known zone.
    pub app_memory: HashMap<String, AppZoneMemory>,
    pub shortcuts: ZoneShortcuts,
}

impl Default for ZonesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            modifier: ZoneModifiers::default(),
            spanning: true,
            adjacent_highlight_distance: 16,
            show_zone_numbers: true,
            inactive_opacity: 50,
            remember_apps: false,
            layouts: default_layouts(),
            per_output: HashMap::new(),
            per_workspace: HashMap::new(),
            app_memory: HashMap::new(),
            shortcuts: ZoneShortcuts::default(),
        }
    }
}

impl ZonesConfig {
    /// Layout for an output/workspace pair. A per-workspace assignment wins
    /// over the monitor default.
    pub fn layout_for(
        &self,
        output: &OutputMatch,
        workspace_id: Option<&str>,
    ) -> Option<&ZoneLayout> {
        workspace_id
            .and_then(|id| self.per_workspace.get(id))
            .or_else(|| self.per_output.get(output))
            .and_then(|id| self.layouts.get(id))
    }

    /// `adjacent_highlight_distance` as a fraction of an output that is
    /// `width` x `height` logical pixels. Uses the smaller axis so the
    /// threshold feels the same horizontally and vertically.
    pub fn edge_threshold_fraction(&self, width: i32, height: i32) -> f64 {
        if !self.spanning {
            return 0.0;
        }
        let min_axis = width.min(height).max(1) as f64;
        (self.adjacent_highlight_distance as f64 / min_axis).clamp(0.0, 0.25)
    }
}

pub const DEFAULT_LAYOUT_ID: &str = "columns-3";

/// Built-in templates, mirroring the FancyZones starter set.
pub fn default_layouts() -> HashMap<String, ZoneLayout> {
    HashMap::from([
        ("columns-2".into(), columns(2, "Columns (2)")),
        ("columns-3".into(), columns(3, "Columns (3)")),
        ("rows-2".into(), rows(2, "Rows (2)")),
        ("grid-2x2".into(), grid(2, 2, "Grid (2x2)")),
        ("priority-grid".into(), priority_grid()),
        ("main-stack".into(), main_stack()),
    ])
}

pub fn columns(n: usize, name: &str) -> ZoneLayout {
    let w = 1.0 / n as f64;
    ZoneLayout::new(
        name,
        (0..n)
            .map(|i| ZoneRect::new(i as f64 * w, 0.0, w, 1.0))
            .collect(),
    )
}

pub fn rows(n: usize, name: &str) -> ZoneLayout {
    let h = 1.0 / n as f64;
    ZoneLayout::new(
        name,
        (0..n)
            .map(|i| ZoneRect::new(0.0, i as f64 * h, 1.0, h))
            .collect(),
    )
}

pub fn grid(cols: usize, rows: usize, name: &str) -> ZoneLayout {
    let w = 1.0 / cols as f64;
    let h = 1.0 / rows as f64;
    let mut zones = Vec::with_capacity(cols * rows);
    for r in 0..rows {
        for c in 0..cols {
            zones.push(ZoneRect::new(c as f64 * w, r as f64 * h, w, h));
        }
    }
    ZoneLayout::new(name, zones)
}

/// Narrow / wide / narrow — the layout most people actually want on a wide
/// monitor: reference material either side of the thing being worked on.
pub fn priority_grid() -> ZoneLayout {
    ZoneLayout::new(
        "Priority Grid",
        vec![
            ZoneRect::new(0.0, 0.0, 0.25, 1.0),
            ZoneRect::new(0.25, 0.0, 0.50, 1.0),
            ZoneRect::new(0.75, 0.0, 0.25, 1.0),
        ],
    )
}

/// One large primary zone with a stack of two beside it.
pub fn main_stack() -> ZoneLayout {
    ZoneLayout::new(
        "Main + Stack",
        vec![
            ZoneRect::new(0.0, 0.0, 0.6, 1.0),
            ZoneRect::new(0.6, 0.0, 0.4, 0.5),
            ZoneRect::new(0.6, 0.5, 0.4, 0.5),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    fn assert_rect(r: ZoneRect, x: f64, y: f64, w: f64, h: f64) {
        approx(r.x, x);
        approx(r.y, y);
        approx(r.w, w);
        approx(r.h, h);
    }

    #[test]
    fn templates_tile_the_unit_square() {
        for (id, layout) in default_layouts() {
            assert!(!layout.zones.is_empty(), "{id} has no zones");
            for z in &layout.zones {
                assert!(z.is_valid(), "{id} has an invalid zone: {z:?}");
            }
            let total: f64 = layout.zones.iter().map(|z| z.area()).sum();
            approx(total, 1.0);
        }
    }

    #[test]
    fn templates_do_not_overlap() {
        for (id, layout) in default_layouts() {
            for (i, a) in layout.zones.iter().enumerate() {
                for b in layout.zones.iter().skip(i + 1) {
                    assert!(!a.overlaps(b), "{id}: {a:?} overlaps {b:?}");
                }
            }
        }
    }

    #[test]
    fn hit_test_finds_the_right_column() {
        let l = columns(3, "c3");
        assert_eq!(l.hit_test(0.10, 0.5), Some(0));
        assert_eq!(l.hit_test(0.50, 0.5), Some(1));
        assert_eq!(l.hit_test(0.90, 0.5), Some(2));
    }

    #[test]
    fn hit_test_boundary_belongs_to_the_zone_on_the_right() {
        // Half-open intervals: a point exactly on a shared edge must land in
        // exactly one zone, never both and never neither.
        let l = columns(2, "c2");
        assert_eq!(l.hit_test(0.5, 0.5), Some(1));
        assert_eq!(l.hit_test(0.5 - 1e-12, 0.5), Some(0));
    }

    #[test]
    fn hit_test_outside_is_none() {
        let l = columns(2, "c2");
        assert_eq!(l.hit_test(-0.1, 0.5), None);
        assert_eq!(l.hit_test(1.5, 0.5), None);
        assert_eq!(l.hit_test(0.5, 1.0), None);
    }

    #[test]
    fn overlapping_zones_resolve_to_the_smallest() {
        let l = ZoneLayout::new(
            "stack",
            vec![
                ZoneRect::new(0.0, 0.0, 1.0, 1.0),
                ZoneRect::new(0.25, 0.25, 0.5, 0.5),
            ],
        );
        assert_eq!(l.hit_test(0.5, 0.5), Some(1));
        assert_eq!(l.hit_test(0.05, 0.05), Some(0));
    }

    #[test]
    fn span_is_the_bounding_box() {
        let l = grid(2, 2, "g");
        let r = l.span(&[0, 3]).unwrap();
        assert_rect(r, 0.0, 0.0, 1.0, 1.0);

        let r = l.span(&[0, 1]).unwrap();
        assert_rect(r, 0.0, 0.0, 1.0, 0.5);
    }

    #[test]
    fn span_of_unknown_index_is_ignored() {
        let l = columns(2, "c2");
        assert!(l.span(&[99]).is_none());
        assert_rect(l.span(&[0, 99]).unwrap(), 0.0, 0.0, 0.5, 1.0);
    }

    #[test]
    fn target_without_threshold_never_spans() {
        let l = columns(3, "c3");
        let t = l.target_at(1.0 / 3.0, 0.5, 0.0).unwrap();
        assert_eq!(t.zones, vec![1]);
        assert!(!t.is_span());
    }

    #[test]
    fn target_near_shared_edge_spans_both_neighbours() {
        let l = columns(3, "c3");
        // Just right of the 1/3 boundary, well within the threshold.
        let t = l.target_at(1.0 / 3.0 + 0.005, 0.5, 0.02).unwrap();
        assert_eq!(t.zones, vec![0, 1]);
        assert!(t.is_span());
        assert_rect(t.rect, 0.0, 0.0, 2.0 / 3.0, 1.0);
    }

    #[test]
    fn target_mid_zone_does_not_span() {
        // Regression guard: crossing zones during a drag must not accumulate.
        // Only proximity to a shared edge may span.
        let l = columns(3, "c3");
        let t = l.target_at(0.5, 0.5, 0.02).unwrap();
        assert_eq!(t.zones, vec![1]);
        assert!(!t.is_span());
    }

    #[test]
    fn target_at_grid_crossing_spans_four() {
        let l = grid(2, 2, "g");
        let t = l.target_at(0.5, 0.5, 0.02).unwrap();
        assert_eq!(t.zones, vec![0, 1, 2, 3]);
        assert_rect(t.rect, 0.0, 0.0, 1.0, 1.0);
    }

    #[test]
    fn target_outside_layout_is_none() {
        let l = columns(2, "c2");
        assert!(l.target_at(2.0, 0.5, 0.02).is_none());
    }

    #[test]
    fn overlapping_zones_suppress_spanning() {
        let l = ZoneLayout::new(
            "stack",
            vec![
                ZoneRect::new(0.0, 0.0, 1.0, 1.0),
                ZoneRect::new(0.25, 0.25, 0.5, 0.5),
            ],
        );
        let t = l.target_at(0.5, 0.5, 0.02).unwrap();
        assert_eq!(t.zones, vec![1]);
        assert!(!t.is_span());
    }

    #[test]
    fn sanitize_drops_degenerate_zones() {
        let l = ZoneLayout::new(
            "junk",
            vec![
                ZoneRect::new(0.0, 0.0, 0.5, 1.0),
                ZoneRect::new(0.0, 0.0, 0.0, 1.0), // zero width
                ZoneRect::new(0.5, 0.0, 2.0, 1.0), // out of bounds
                ZoneRect::new(f64::NAN, 0.0, 0.5, 1.0),
            ],
        );
        assert_eq!(l.sanitized().zones.len(), 1);
    }

    #[test]
    fn clamped_pulls_into_the_unit_square() {
        assert_rect(
            ZoneRect::new(-0.5, 0.5, 2.0, 2.0).clamped(),
            0.0,
            0.5,
            1.0,
            0.5,
        );
    }

    #[test]
    fn edge_threshold_scales_with_the_smaller_axis() {
        let cfg = ZonesConfig::default();
        approx(cfg.edge_threshold_fraction(3840, 2160), 16.0 / 2160.0);
        // Degenerate sizes must not divide by zero or go infinite.
        assert!(cfg.edge_threshold_fraction(0, 0).is_finite());
    }

    #[test]
    fn edge_threshold_is_zero_when_spanning_is_off() {
        let cfg = ZonesConfig {
            spanning: false,
            ..Default::default()
        };
        approx(cfg.edge_threshold_fraction(1920, 1080), 0.0);
    }

    #[test]
    fn per_workspace_layout_beats_per_output() {
        let output = OutputMatch {
            name: "DP-1".into(),
            edid: None,
        };
        let mut cfg = ZonesConfig::default();
        cfg.per_output.insert(output.clone(), "columns-2".into());
        cfg.per_workspace.insert("ws1".into(), "grid-2x2".into());

        assert_eq!(cfg.layout_for(&output, None).unwrap().name, "Columns (2)");
        assert_eq!(
            cfg.layout_for(&output, Some("ws1")).unwrap().name,
            "Grid (2x2)"
        );
        // An unmapped workspace falls back to the monitor default.
        assert_eq!(
            cfg.layout_for(&output, Some("other")).unwrap().name,
            "Columns (2)"
        );
    }

    #[test]
    fn unmapped_output_has_no_layout() {
        let cfg = ZonesConfig::default();
        let output = OutputMatch {
            name: "HDMI-1".into(),
            edid: None,
        };
        assert!(cfg.layout_for(&output, None).is_none());
    }

    #[test]
    fn default_layout_id_exists() {
        assert!(default_layouts().contains_key(DEFAULT_LAYOUT_ID));
    }

    #[test]
    fn config_survives_a_ron_round_trip() {
        let mut cfg = ZonesConfig::default();
        cfg.per_output.insert(
            OutputMatch {
                name: "DP-1".into(),
                edid: None,
            },
            "priority-grid".into(),
        );
        cfg.app_memory.insert(
            "com.example.App".into(),
            AppZoneMemory {
                layout: "priority-grid".into(),
                zones: vec![1],
            },
        );

        let encoded = ron::ser::to_string(&cfg).expect("serialize");
        let decoded: ZonesConfig = ron::from_str(&encoded).expect("deserialize");
        assert_eq!(cfg, decoded);
    }
}
