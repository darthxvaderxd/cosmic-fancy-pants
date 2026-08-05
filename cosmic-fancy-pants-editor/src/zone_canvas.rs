// SPDX-License-Identifier: GPL-3.0-only

//! Interactive zone canvas.
//!
//! Draws the layout at true size and lets shared boundaries be dragged. Zones
//! are edited as a grid: moving a divider resizes the zones on both sides of
//! it, so the layout stays tiled rather than developing gaps or overlaps.

use cosmic::{
    iced::{
        Color, Point, Rectangle, Renderer, Size, mouse,
        widget::canvas::{self, Event, Frame, Geometry, Path, Stroke, Text},
    },
    theme::Theme,
};
use cosmic_comp_config::zones::ZoneRect;

use crate::state::Message;

/// How close, in pixels, the cursor must be to a boundary to grab it.
const GRAB_PX: f32 = 8.0;
/// Smallest a zone may be dragged to, as a fraction. Stops a drag from
/// collapsing a zone to nothing and making it unrecoverable.
const MIN_FRACTION: f64 = 0.05;
/// Fractional tolerance for treating two edges as the same boundary.
const EPS: f64 = 1e-6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Vertical,
    Horizontal,
}

/// A boundary shared by zones on either side of it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Divider {
    pub axis: Axis,
    pub position: f64,
}

/// An in-progress boundary drag.
///
/// The zones are snapshotted at grab time and every frame is derived from that
/// snapshot. Advancing the boundary incrementally against live app state races:
/// an edit only reaches `self.zones` after a round trip through `update` and a
/// view rebuild, so a second motion event arriving first would look for the
/// boundary at its new position in the old zones, match nothing, and republish
/// the layout unchanged — silently undoing the drag.
#[derive(Debug, Clone)]
struct Drag {
    /// Boundary as it was when grabbed, matching `origin`.
    divider: Divider,
    /// Zones as they were when grabbed.
    origin: Vec<ZoneRect>,
    /// Latest position, for drawing the handle.
    current: f64,
}

#[derive(Debug, Default)]
pub struct CanvasState {
    dragging: Option<Drag>,
    /// Tracked from keyboard events; mouse events do not carry modifiers.
    modifiers: cosmic::iced::keyboard::Modifiers,
}

pub struct ZoneCanvas<'a> {
    pub zones: &'a [ZoneRect],
    pub surface_id: cosmic::iced::window::Id,
    pub show_numbers: bool,
    /// Gap between snapped windows, drawn so the setting is visible here.
    pub gap: f32,
}

/// Interior boundaries of a layout.
///
/// Only boundaries with zones on *both* sides are returned: the outer edges of
/// the screen are not draggable, and a one-sided edge would tear the tiling.
pub fn dividers(zones: &[ZoneRect]) -> Vec<Divider> {
    let mut found: Vec<Divider> = Vec::new();

    let mut consider = |axis: Axis, position: f64| {
        if position <= EPS || position >= 1.0 - EPS {
            return;
        }
        let (before, after) = match axis {
            Axis::Vertical => (
                zones.iter().any(|z| (z.right() - position).abs() < EPS),
                zones.iter().any(|z| (z.x - position).abs() < EPS),
            ),
            Axis::Horizontal => (
                zones.iter().any(|z| (z.bottom() - position).abs() < EPS),
                zones.iter().any(|z| (z.y - position).abs() < EPS),
            ),
        };
        if before
            && after
            && !found
                .iter()
                .any(|d| d.axis == axis && (d.position - position).abs() < EPS)
        {
            found.push(Divider { axis, position });
        }
    };

    for zone in zones {
        consider(Axis::Vertical, zone.x);
        consider(Axis::Vertical, zone.right());
        consider(Axis::Horizontal, zone.y);
        consider(Axis::Horizontal, zone.bottom());
    }
    found
}

/// Move a boundary, resizing the zones on both sides.
///
/// The target is clamped so the dragged boundary stays on screen and its own
/// two zones keep a usable size — a drag past the edge pins rather than
/// collapsing. Clamping cannot protect zones beyond the adjacent pair, though,
/// so a move that would invert one of those returns `None` and the caller keeps
/// the previous layout.
pub fn move_divider(zones: &[ZoneRect], divider: Divider, to: f64) -> Option<Vec<ZoneRect>> {
    let to = to.clamp(MIN_FRACTION, 1.0 - MIN_FRACTION);
    let mut out = zones.to_vec();

    for zone in &mut out {
        match divider.axis {
            Axis::Vertical => {
                if (zone.right() - divider.position).abs() < EPS {
                    zone.w = to - zone.x;
                } else if (zone.x - divider.position).abs() < EPS {
                    let right = zone.right();
                    zone.x = to;
                    zone.w = right - to;
                }
            }
            Axis::Horizontal => {
                if (zone.bottom() - divider.position).abs() < EPS {
                    zone.h = to - zone.y;
                } else if (zone.y - divider.position).abs() < EPS {
                    let bottom = zone.bottom();
                    zone.y = to;
                    zone.h = bottom - to;
                }
            }
        }
    }

    if out.iter().any(|z| z.w < MIN_FRACTION || z.h < MIN_FRACTION) {
        return None;
    }
    Some(out)
}

/// Split a zone in two at `at`, measured along `axis`.
///
/// `axis` names the orientation of the new boundary, matching [`Divider`]:
/// a vertical split produces side-by-side zones. Returns `None` if either half
/// would be unusably small, so a stray click cannot shave off a sliver.
pub fn split_zone(zones: &[ZoneRect], index: usize, at: f64, axis: Axis) -> Option<Vec<ZoneRect>> {
    let zone = *zones.get(index)?;

    let (first, second) = match axis {
        Axis::Vertical => {
            if at - zone.x < MIN_FRACTION || zone.right() - at < MIN_FRACTION {
                return None;
            }
            (
                ZoneRect::new(zone.x, zone.y, at - zone.x, zone.h),
                ZoneRect::new(at, zone.y, zone.right() - at, zone.h),
            )
        }
        Axis::Horizontal => {
            if at - zone.y < MIN_FRACTION || zone.bottom() - at < MIN_FRACTION {
                return None;
            }
            (
                ZoneRect::new(zone.x, zone.y, zone.w, at - zone.y),
                ZoneRect::new(zone.x, at, zone.w, zone.bottom() - at),
            )
        }
    };

    let mut out = zones.to_vec();
    out[index] = first;
    // Inserted next to its sibling so zone numbers stay in reading order.
    out.insert(index + 1, second);
    Some(out)
}

/// Remove a boundary, merging the zones on either side of it.
///
/// Zones are paired across the boundary only when they line up exactly on the
/// perpendicular axis, so merging a grid column joins each row's pair and
/// leaves a ragged layout untouched rather than producing overlaps. Returns
/// `None` when nothing could be paired.
pub fn merge_at(zones: &[ZoneRect], divider: Divider) -> Option<Vec<ZoneRect>> {
    let mut out = zones.to_vec();
    let mut merged_any = false;
    let mut removed: Vec<usize> = Vec::new();

    for i in 0..zones.len() {
        if removed.contains(&i) {
            continue;
        }
        let a = zones[i];
        let ends_here = match divider.axis {
            Axis::Vertical => (a.right() - divider.position).abs() < EPS,
            Axis::Horizontal => (a.bottom() - divider.position).abs() < EPS,
        };
        if !ends_here {
            continue;
        }

        let partner = (0..zones.len()).find(|&j| {
            if j == i || removed.contains(&j) {
                return false;
            }
            let b = zones[j];
            match divider.axis {
                Axis::Vertical => {
                    (b.x - divider.position).abs() < EPS
                        && (b.y - a.y).abs() < EPS
                        && (b.h - a.h).abs() < EPS
                }
                Axis::Horizontal => {
                    (b.y - divider.position).abs() < EPS
                        && (b.x - a.x).abs() < EPS
                        && (b.w - a.w).abs() < EPS
                }
            }
        });

        if let Some(j) = partner {
            out[i] = a.union(&zones[j]);
            removed.push(j);
            merged_any = true;
        }
    }

    if !merged_any {
        return None;
    }
    removed.sort_unstable_by(|a, b| b.cmp(a));
    for index in removed {
        out.remove(index);
    }
    Some(out)
}

fn to_screen(zone: &ZoneRect, bounds: Rectangle) -> Rectangle {
    Rectangle {
        x: bounds.x + zone.x as f32 * bounds.width,
        y: bounds.y + zone.y as f32 * bounds.height,
        width: zone.w as f32 * bounds.width,
        height: zone.h as f32 * bounds.height,
    }
}

/// Nearest grabbable boundary to the cursor, within [`GRAB_PX`].
fn divider_at(zones: &[ZoneRect], bounds: Rectangle, cursor: Point) -> Option<Divider> {
    dividers(zones)
        .into_iter()
        .map(|divider| {
            let distance = match divider.axis {
                Axis::Vertical => {
                    (bounds.x + divider.position as f32 * bounds.width - cursor.x).abs()
                }
                Axis::Horizontal => {
                    (bounds.y + divider.position as f32 * bounds.height - cursor.y).abs()
                }
            };
            (divider, distance)
        })
        .filter(|(_, distance)| *distance <= GRAB_PX)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(divider, _)| divider)
}

/// Cursor position as a fraction of the canvas.
fn to_fraction(bounds: Rectangle, cursor: Point) -> (f64, f64) {
    (
        ((cursor.x - bounds.x) / bounds.width.max(1.0)) as f64,
        ((cursor.y - bounds.y) / bounds.height.max(1.0)) as f64,
    )
}

/// Index of the zone containing a fractional point, smallest first so a zone
/// stacked on a larger one still wins.
fn index_at(zones: &[ZoneRect], x: f64, y: f64) -> Option<usize> {
    zones
        .iter()
        .enumerate()
        .filter(|(_, z)| z.contains(x, y))
        .min_by(|(_, a), (_, b)| a.area().total_cmp(&b.area()))
        .map(|(i, _)| i)
}

fn cursor_fraction(divider: Divider, bounds: Rectangle, cursor: Point) -> f64 {
    match divider.axis {
        Axis::Vertical => ((cursor.x - bounds.x) / bounds.width.max(1.0)) as f64,
        Axis::Horizontal => ((cursor.y - bounds.y) / bounds.height.max(1.0)) as f64,
    }
}

impl canvas::Program<Message, Theme, Renderer> for ZoneCanvas<'_> {
    type State = CanvasState;

    fn update(
        &self,
        state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let position = cursor.position_in(bounds)?;
        // `position_in` is bounds-relative; boundary maths works in the same
        // space as `draw`, which uses absolute bounds.
        let absolute = Point::new(bounds.x + position.x, bounds.y + position.y);

        match event {
            Event::Keyboard(cosmic::iced::keyboard::Event::ModifiersChanged(modifiers)) => {
                state.modifiers = *modifiers;
                None
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                // On a boundary: drag it. Anywhere else: split the zone there.
                if let Some(divider) = divider_at(self.zones, bounds, absolute) {
                    state.dragging = Some(Drag {
                        divider,
                        origin: self.zones.to_vec(),
                        current: divider.position,
                    });
                    return Some(canvas::Action::capture());
                }

                let (fx, fy) = to_fraction(bounds, absolute);
                let index = index_at(self.zones, fx, fy)?;
                let zone = self.zones[index];
                // Split along the longer side by default, which is what makes
                // a wide zone become two columns; Shift picks the other axis.
                let mut axis = if zone.w >= zone.h {
                    Axis::Vertical
                } else {
                    Axis::Horizontal
                };
                if state.modifiers.shift() {
                    axis = match axis {
                        Axis::Vertical => Axis::Horizontal,
                        Axis::Horizontal => Axis::Vertical,
                    };
                }
                let at = match axis {
                    Axis::Vertical => fx,
                    Axis::Horizontal => fy,
                };
                let updated = split_zone(self.zones, index, at, axis)?;
                Some(canvas::Action::publish(Message::ZonesEdited(
                    self.surface_id,
                    updated,
                )))
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) => {
                // Right-click a boundary to delete it, merging across.
                let divider = divider_at(self.zones, bounds, absolute)?;
                let updated = merge_at(self.zones, divider)?;
                Some(canvas::Action::publish(Message::ZonesEdited(
                    self.surface_id,
                    updated,
                )))
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let drag = state.dragging.as_mut()?;
                let to = cursor_fraction(drag.divider, bounds, absolute);
                // Preview locally and redraw; publishing per motion event would
                // rebuild the whole widget tree and re-render a fullscreen
                // canvas hundreds of times per drag, which is what made
                // dragging feel sluggish.
                if move_divider(&drag.origin, drag.divider, to).is_some() {
                    drag.current = to.clamp(MIN_FRACTION, 1.0 - MIN_FRACTION);
                }
                Some(canvas::Action::request_redraw())
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                // Commit once, on release.
                let drag = state.dragging.take()?;
                let updated = move_divider(&drag.origin, drag.divider, drag.current)?;
                Some(canvas::Action::publish(Message::ZonesEdited(
                    self.surface_id,
                    updated,
                )))
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<Geometry<Renderer>> {
        let cosmic = theme.cosmic();
        let mut frame = Frame::new(renderer, bounds.size());

        let accent = cosmic.accent_color();
        let accent = Color::from_rgb(accent.red, accent.green, accent.blue);
        let neutral = cosmic.palette.neutral_5;
        let fill = Color::from_rgba(neutral.red, neutral.green, neutral.blue, 0.35);

        // Zones hold still during a drag; only the guide line moves, and the
        // layout reflows once on release. Less visually noisy than reflowing
        // continuously, and it keeps the zone geometry static so the per-frame
        // cost is a single line rather than the whole scene.
        let zones: &[ZoneRect] = self.zones;

        let hovered = cursor
            .position_in(bounds)
            .map(|p| Point::new(bounds.x + p.x, bounds.y + p.y))
            .and_then(|p| divider_at(zones, bounds, p));
        let active = state
            .dragging
            .as_ref()
            .map(|drag| Divider {
                position: drag.current,
                ..drag.divider
            })
            .or(hovered);

        for (index, zone) in zones.iter().enumerate() {
            // Frame coordinates are local to the canvas, so draw at the origin.
            let rect = to_screen(
                zone,
                Rectangle {
                    x: 0.0,
                    y: 0.0,
                    ..bounds
                },
            );
            // Same rule the compositor uses: a full gap against an output edge,
            // half a gap against an interior edge, so neighbours share one
            // gutter. Drawing it here makes the padding setting visible.
            let gap = |at_edge: bool| if at_edge { self.gap } else { self.gap / 2.0 };
            let left = gap(zone.x <= EPS);
            let top = gap(zone.y <= EPS);
            let right = gap(zone.right() >= 1.0 - EPS);
            let bottom = gap(zone.bottom() >= 1.0 - EPS);
            let inset = Rectangle {
                x: rect.x + left,
                y: rect.y + top,
                width: (rect.width - left - right).max(1.0),
                height: (rect.height - top - bottom).max(1.0),
            };
            let path = Path::rectangle(
                Point::new(inset.x, inset.y),
                Size::new(inset.width, inset.height),
            );
            frame.fill(&path, fill);
            frame.stroke(&path, Stroke::default().with_color(accent).with_width(2.0));

            if self.show_numbers {
                frame.fill_text(Text {
                    content: format!("{}", index + 1),
                    position: Point::new(inset.x + inset.width / 2.0, inset.y + inset.height / 2.0),
                    color: accent,
                    size: 28.0.into(),
                    align_x: cosmic::iced::alignment::Horizontal::Center.into(),
                    align_y: cosmic::iced::alignment::Vertical::Center.into(),
                    ..Text::default()
                });
            }
        }

        // Emphasise the boundary under the cursor so it is discoverably draggable.
        if let Some(divider) = active {
            let path = match divider.axis {
                Axis::Vertical => {
                    let x = divider.position as f32 * bounds.width;
                    Path::rectangle(Point::new(x - 2.0, 0.0), Size::new(4.0, bounds.height))
                }
                Axis::Horizontal => {
                    let y = divider.position as f32 * bounds.height;
                    Path::rectangle(Point::new(0.0, y - 2.0), Size::new(bounds.width, 4.0))
                }
            };
            frame.fill(&path, accent);
        }

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        let divider = state.dragging.as_ref().map(|d| d.divider).or_else(|| {
            cursor
                .position_in(bounds)
                .map(|p| Point::new(bounds.x + p.x, bounds.y + p.y))
                .and_then(|p| divider_at(self.zones, bounds, p))
        });
        match divider.map(|d| d.axis) {
            Some(Axis::Vertical) => mouse::Interaction::ResizingHorizontally,
            Some(Axis::Horizontal) => mouse::Interaction::ResizingVertically,
            None => mouse::Interaction::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn columns(n: usize) -> Vec<ZoneRect> {
        let w = 1.0 / n as f64;
        (0..n)
            .map(|i| ZoneRect::new(i as f64 * w, 0.0, w, 1.0))
            .collect()
    }

    #[test]
    fn interior_boundaries_are_dividers() {
        let found = dividers(&columns(3));
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|d| d.axis == Axis::Vertical));
    }

    /// The screen edges are not boundaries; dragging them would tear the layout
    /// away from the output.
    #[test]
    fn screen_edges_are_not_dividers() {
        let found = dividers(&columns(2));
        assert_eq!(found.len(), 1);
        assert!((found[0].position - 0.5).abs() < 1e-9);
    }

    #[test]
    fn moving_a_divider_resizes_both_sides() {
        let zones = columns(2);
        let moved = move_divider(&zones, dividers(&zones)[0], 0.7).unwrap();
        assert!((moved[0].w - 0.7).abs() < 1e-9, "{:?}", moved[0]);
        assert!((moved[1].x - 0.7).abs() < 1e-9);
        assert!((moved[1].w - 0.3).abs() < 1e-9);
    }

    /// The layout must stay tiled: no gaps, no overlaps, still covering 1.0.
    #[test]
    fn moving_a_divider_keeps_the_layout_tiled() {
        let zones = columns(3);
        let moved = move_divider(&zones, dividers(&zones)[0], 0.2).unwrap();
        let total: f64 = moved.iter().map(|z| z.w * z.h).sum();
        assert!((total - 1.0).abs() < 1e-9, "total area {total}");
        for pair in moved.windows(2) {
            assert!((pair[0].right() - pair[1].x).abs() < 1e-9, "gap or overlap");
        }
    }

    /// Dragging past the edge pins the boundary at the minimum rather than
    /// collapsing the zone or refusing the move, so the drag stays smooth.
    #[test]
    fn dragging_past_the_edge_clamps() {
        let zones = columns(2);
        let divider = dividers(&zones)[0];

        let squashed = move_divider(&zones, divider, 0.001).unwrap();
        assert!((squashed[0].w - MIN_FRACTION).abs() < 1e-9, "{squashed:?}");
        assert!(squashed.iter().all(|z| z.w >= MIN_FRACTION));

        let stretched = move_divider(&zones, divider, 0.999).unwrap();
        assert!(
            stretched.iter().all(|z| z.w >= MIN_FRACTION),
            "{stretched:?}"
        );
    }

    /// A move that would invert a zone on the far side of a neighbouring
    /// boundary is refused outright — clamping the dragged edge cannot protect
    /// zones that are not adjacent to it.
    #[test]
    fn a_move_that_would_invert_a_neighbour_is_refused() {
        let zones = columns(3);
        let first = dividers(&zones)
            .into_iter()
            .min_by(|a, b| a.position.total_cmp(&b.position))
            .unwrap();
        // Dragging the first boundary past the second would give the middle
        // zone a negative width.
        assert!(move_divider(&zones, first, 0.9).is_none());
    }

    /// Regression: a drag must be absolute against its grab-time snapshot.
    ///
    /// Motion events outrun the app-state round trip, so several arrive against
    /// the same zones. Deriving each move from the *previous result* instead
    /// finds no boundary at the stale position, silently returns the layout
    /// unchanged, and the drag appears to snap back on release.
    #[test]
    fn moves_are_absolute_against_the_grab_snapshot() {
        let zones = columns(2);
        let divider = dividers(&zones)[0];

        let first = move_divider(&zones, divider, 0.7).unwrap();
        let second = move_divider(&zones, divider, 0.35).unwrap();
        assert!((second[0].w - 0.35).abs() < 1e-9, "{second:?}");

        // The broken form: same divider, but based on already-moved zones.
        let stale = move_divider(&first, divider, 0.35).unwrap();
        assert_eq!(
            stale, first,
            "a stale base silently no-ops, which is the bug this guards"
        );
    }

    fn tiles_completely(zones: &[ZoneRect]) -> bool {
        let area: f64 = zones.iter().map(|z| z.w * z.h).sum();
        (area - 1.0).abs() < 1e-9
    }

    #[test]
    fn splitting_produces_two_zones_that_still_tile() {
        let zones = columns(2);
        let split = split_zone(&zones, 0, 0.2, Axis::Vertical).unwrap();
        assert_eq!(split.len(), 3);
        assert!(tiles_completely(&split), "{split:?}");
        assert!((split[0].w - 0.2).abs() < 1e-9);
        assert!((split[1].x - 0.2).abs() < 1e-9);
        assert!((split[1].w - 0.3).abs() < 1e-9);
    }

    /// The new zone is inserted beside its sibling so the numbering the user
    /// sees stays in reading order rather than jumping to the end.
    #[test]
    fn a_split_zone_keeps_its_neighbours_in_order() {
        let zones = columns(3);
        let split = split_zone(&zones, 0, 0.15, Axis::Vertical).unwrap();
        let xs: Vec<f64> = split.iter().map(|z| z.x).collect();
        let mut sorted = xs.clone();
        sorted.sort_by(|a, b| a.total_cmp(b));
        assert_eq!(xs, sorted, "zones should stay left-to-right");
    }

    #[test]
    fn splitting_horizontally_stacks_the_halves() {
        let zones = vec![ZoneRect::new(0.0, 0.0, 1.0, 1.0)];
        let split = split_zone(&zones, 0, 0.25, Axis::Horizontal).unwrap();
        assert!((split[0].h - 0.25).abs() < 1e-9);
        assert!((split[1].y - 0.25).abs() < 1e-9);
        assert!(tiles_completely(&split));
    }

    /// A stray click near an edge must not shave off an unusable sliver.
    #[test]
    fn splitting_too_close_to_an_edge_is_refused() {
        let zones = vec![ZoneRect::new(0.0, 0.0, 1.0, 1.0)];
        assert!(split_zone(&zones, 0, 0.001, Axis::Vertical).is_none());
        assert!(split_zone(&zones, 0, 0.999, Axis::Vertical).is_none());
    }

    #[test]
    fn merging_removes_a_boundary_and_still_tiles() {
        let zones = columns(3);
        let divider = dividers(&zones)[0];
        let merged = merge_at(&zones, divider).unwrap();
        assert_eq!(merged.len(), 2);
        assert!(tiles_completely(&merged), "{merged:?}");
    }

    /// Deleting a boundary in a grid should join every pair along it, not just
    /// the first, or the layout would be left with a hole.
    #[test]
    fn merging_a_grid_column_joins_every_row() {
        let zones = vec![
            ZoneRect::new(0.0, 0.0, 0.5, 0.5),
            ZoneRect::new(0.5, 0.0, 0.5, 0.5),
            ZoneRect::new(0.0, 0.5, 0.5, 0.5),
            ZoneRect::new(0.5, 0.5, 0.5, 0.5),
        ];
        let vertical = dividers(&zones)
            .into_iter()
            .find(|d| d.axis == Axis::Vertical)
            .unwrap();
        let merged = merge_at(&zones, vertical).unwrap();
        assert_eq!(merged.len(), 2, "both rows should merge: {merged:?}");
        assert!(tiles_completely(&merged));
    }

    /// Zones that do not line up across the boundary cannot merge into a
    /// rectangle, so they are left alone rather than overlapped.
    #[test]
    fn merging_ragged_zones_is_refused() {
        let zones = vec![
            ZoneRect::new(0.0, 0.0, 0.5, 1.0),
            ZoneRect::new(0.5, 0.0, 0.5, 0.5),
            ZoneRect::new(0.5, 0.5, 0.5, 0.5),
        ];
        // The left zone spans full height; neither right zone matches it.
        let divider = Divider {
            axis: Axis::Vertical,
            position: 0.5,
        };
        assert!(merge_at(&zones, divider).is_none());
    }

    #[test]
    fn horizontal_dividers_are_found_in_stacked_layouts() {
        let zones = vec![
            ZoneRect::new(0.0, 0.0, 1.0, 0.4),
            ZoneRect::new(0.0, 0.4, 1.0, 0.6),
        ];
        let found = dividers(&zones);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].axis, Axis::Horizontal);

        let moved = move_divider(&zones, found[0], 0.25).unwrap();
        assert!((moved[0].h - 0.25).abs() < 1e-9);
        assert!((moved[1].y - 0.25).abs() < 1e-9);
        assert!((moved[1].h - 0.75).abs() < 1e-9);
    }

    /// A grid shares one divider across several rows; dragging it must move
    /// every zone on that boundary, not just the first.
    #[test]
    fn a_shared_divider_moves_every_adjacent_zone() {
        let zones = vec![
            ZoneRect::new(0.0, 0.0, 0.5, 0.5),
            ZoneRect::new(0.5, 0.0, 0.5, 0.5),
            ZoneRect::new(0.0, 0.5, 0.5, 0.5),
            ZoneRect::new(0.5, 0.5, 0.5, 0.5),
        ];
        let vertical = dividers(&zones)
            .into_iter()
            .find(|d| d.axis == Axis::Vertical)
            .unwrap();
        let moved = move_divider(&zones, vertical, 0.3).unwrap();
        assert!((moved[0].w - 0.3).abs() < 1e-9);
        assert!((moved[2].w - 0.3).abs() < 1e-9, "second row did not move");
        assert!((moved[1].x - 0.3).abs() < 1e-9);
        assert!((moved[3].x - 0.3).abs() < 1e-9, "second row did not move");
    }
}
