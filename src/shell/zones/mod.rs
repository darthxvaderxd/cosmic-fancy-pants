// SPDX-License-Identifier: GPL-3.0-only

//! Resolving user-defined snap zones for an output.
//!
//! [`cosmic_comp_config::zones`] owns the layout model and the pure fractional
//! geometry. This module is the bridge to compositor state: it picks the layout
//! that applies to a given output and workspace, measures the area the zones
//! are laid out over, and converts between pixels and fractions.

use cosmic_comp_config::{
    workspace::OutputMatch,
    zones::{ZoneLayout, ZoneModifiers, ZoneRect, ZonesConfig},
};
use smithay::{
    desktop::layer_map_for_output,
    input::keyboard::ModifiersState,
    output::Output,
    utils::{Logical, Point, Rectangle},
};

use crate::{shell::layout::floating::zone_relative_geometry, utils::prelude::*};

/// Does the currently held modifier set arm zone snapping?
///
/// Exact match, not a subset: holding Shift+Super with Shift configured should
/// not arm zones, since Super-drag has its own meaning.
pub fn modifiers_match(want: &ZoneModifiers, have: &ModifiersState) -> bool {
    want.ctrl == have.ctrl
        && want.alt == have.alt
        && want.shift == have.shift
        && want.logo == have.logo
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

        let layout_id = workspace_id
            .and_then(|id| config.per_workspace.get(id))
            .or_else(|| config.per_output.get(&output_match))
            .cloned()?;

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
            edge_threshold,
        })
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
            edge_threshold,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic_comp_config::zones::columns;

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

    #[test]
    fn modifiers_must_match_exactly() {
        let want = ZoneModifiers::default(); // shift
        let mut have = ModifiersState::default();
        have.shift = true;
        assert!(modifiers_match(&want, &have));

        // Shift+Super is a different gesture and must not arm zones.
        have.logo = true;
        assert!(!modifiers_match(&want, &have));
    }
}
