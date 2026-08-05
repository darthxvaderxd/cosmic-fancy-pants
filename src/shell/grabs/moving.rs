// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    backend::render::{
        BackdropShader, IndicatorShader, Key, Usage, cursor::CursorState, element::AsGlowRenderer,
    },
    shell::{
        CosmicMapped, CosmicSurface, Direction, ManagedLayer,
        element::zone_number::ZoneNumber,
        element::{CosmicMappedRenderElement, stack_hover::StackHover},
        focus::target::{KeyboardFocusTarget, PointerFocusTarget},
        layout::floating::{FloatingTiled, TiledCorners},
        zones::{self, ZoneContext, ZoneHit},
    },
    utils::prelude::*,
    wayland::protocols::toplevel_info::{toplevel_enter_output, toplevel_enter_workspace},
};

use calloop::LoopHandle;
use cosmic::theme::CosmicTheme;
use cosmic_comp_config::zones::AppZoneMemory;
use cosmic_config::ConfigSet;
use smallvec::SmallVec;
use smithay::{
    backend::{
        drm::DrmNode,
        input::ButtonState,
        renderer::{
            ImportAll, ImportMem,
            element::{RenderElement, utils::RescaleRenderElement},
        },
    },
    desktop::{WindowSurfaceType, layer_map_for_output, space::SpaceElement},
    input::{
        Seat,
        pointer::{
            AxisFrame, ButtonEvent, CursorIcon, GestureHoldBeginEvent, GestureHoldEndEvent,
            GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent,
            GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent,
            GrabStartData as PointerGrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
            RelativeMotionEvent,
        },
        touch::{self, GrabStartData as TouchGrabStartData, TouchGrab, TouchInnerHandle},
    },
    output::Output,
    utils::{IsAlive, Logical, Point, Rectangle, SERIAL_COUNTER, Scale},
};
use std::{
    collections::HashSet,
    sync::{Mutex, atomic::Ordering},
    time::Instant,
};
use tracing::warn;

use super::{GrabStartData, ReleaseMode};

pub type SeatMoveGrabState = Mutex<Option<MoveGrabState>>;

const RESCALE_ANIMATION_DURATION: f64 = 150.0;

pub struct MoveGrabState {
    window: CosmicMapped,
    window_offset: Point<i32, Logical>,
    indicator_thickness: u8,
    start: Instant,
    previous: ManagedLayer,
    snapping_zone: Option<SnappingZone>,
    zones: Option<ZoneDragState>,
    /// Output [`Self::zones`] was resolved for, recorded even when resolving
    /// produced nothing. Resolving to `None` is the *default* state — zones
    /// ship enabled but no layout is assigned until the user picks one — so
    /// without this marker a miss would be retried on every motion event.
    zones_output: Option<Output>,
    stacking_indicator: Option<(StackHover, Point<i32, Logical>)>,
    location: Point<f64, Logical>,
    cursor_output: Output,
}

/// Live state for a drag that has armed user-defined zone snapping.
///
/// The resolved [`ZoneContext`] is cached here rather than rebuilt per motion
/// event, since resolving clones the layout and measures the layer map.
pub struct ZoneDragState {
    /// Output the context was resolved against; a cursor crossing to another
    /// output invalidates it.
    output: Output,
    context: ZoneContext,
    target: Option<ZoneHit>,
    /// Whether the arming modifier is held right now. Releasing it disarms
    /// rather than discarding: the modifier gets tapped mid-drag, and the
    /// badges are real elements — rebuilding one per zone on every tap is
    /// churn for no visible benefit.
    armed: bool,
    /// Fill opacity for non-targeted zones, from `ZonesConfig::inactive_opacity`.
    inactive_alpha: f32,
    /// One badge per zone, built once when the drag arms rather than per frame.
    /// Empty when `show_zone_numbers` is off.
    numbers: Vec<ZoneNumber>,
}

impl ZoneDragState {
    /// Undo the `output_enter` each badge was created with.
    ///
    /// Entering an output is what puts an `IcedElement` on that output's
    /// element list; the leave has to be paired or the output keeps tracking
    /// elements that will never render again.
    fn output_leave(&self) {
        for number in &self.numbers {
            number.output_leave(&self.output);
        }
    }
}

impl MoveGrabState {
    #[profiling::function]
    pub fn render<R>(
        &self,
        renderer: &mut R,
        output: &Output,
        theme: &CosmicTheme,
        scanout_node: Option<DrmNode>,
        push: &mut dyn FnMut(CosmicMappedRenderElement<R>),
    ) where
        R: AsGlowRenderer + ImportAll + ImportMem,
        R::TextureId: Send + Clone + 'static,
        CosmicMappedRenderElement<R>: RenderElement<R>,
    {
        let scale = if self.previous == ManagedLayer::Tiling {
            0.6 + ((1.0
                - (Instant::now().duration_since(self.start).as_millis() as f64
                    / RESCALE_ANIMATION_DURATION)
                    .min(1.0))
                * 0.4)
        } else {
            1.0
        };
        let alpha = if &self.cursor_output == output {
            1.0
        } else {
            0.4
        };

        let mut window_geo = self.window.geometry();
        window_geo.loc += self.location.to_i32_round() + self.window_offset;
        if output
            .geometry()
            .as_logical()
            .intersection(window_geo)
            .is_none()
        {
            return;
        }

        let output_scale: Scale<f64> = output.current_scale().fractional_scale().into();
        let scaling_offset =
            self.window_offset - self.window_offset.to_f64().upscale(scale).to_i32_round();
        let render_location = self.location.to_i32_round() - output.geometry().loc.as_logical()
            + self.window_offset
            - scaling_offset;

        for (indicator, location) in self.stacking_indicator.iter() {
            indicator.push_render_elements(
                renderer,
                location.to_physical_precise_round(output_scale),
                output_scale,
                1.0,
                &mut |elem| push(elem.into()),
                None,
            );
        }

        self.window.push_popup_render_elements::<R>(
            renderer,
            (render_location - self.window.geometry().loc).to_physical_precise_round(output_scale),
            output_scale,
            alpha,
            scanout_node,
            push,
        );

        let active_window_hint = crate::theme::active_window_hint(theme);
        let radius = self
            .element()
            .corner_radius(window_geo.size, self.indicator_thickness);

        if self.indicator_thickness > 0 {
            push(
                IndicatorShader::focus_element(
                    renderer,
                    Key::Window(Usage::MoveGrabIndicator, self.window.key()),
                    Rectangle::new(
                        render_location,
                        self.window
                            .geometry()
                            .size
                            .to_f64()
                            .upscale(scale)
                            .to_i32_round(),
                    )
                    .as_local(),
                    self.indicator_thickness,
                    radius,
                    alpha,
                    output_scale.x,
                    [
                        active_window_hint.red,
                        active_window_hint.green,
                        active_window_hint.blue,
                    ],
                )
                .into(),
            )
        }

        let map_window_element = |elem| match elem {
            CosmicMappedRenderElement::Stack(stack) => {
                CosmicMappedRenderElement::GrabbedStack(RescaleRenderElement::from_element(
                    stack,
                    render_location
                        .to_physical_precise_round(output.current_scale().fractional_scale()),
                    scale,
                ))
            }
            CosmicMappedRenderElement::Window(window) => {
                CosmicMappedRenderElement::GrabbedWindow(RescaleRenderElement::from_element(
                    window,
                    render_location
                        .to_physical_precise_round(output.current_scale().fractional_scale()),
                    scale,
                ))
            }
            x => x,
        };

        let mut lower_elements = SmallVec::<[CosmicMappedRenderElement<R>; 4]>::new_const();
        self.window.push_render_elements(
            renderer,
            (render_location - self.window.geometry().loc).to_physical_precise_round(output_scale),
            None,
            output_scale,
            alpha,
            Some(false),
            scanout_node,
            &mut |elem| push(map_window_element(elem)),
            &mut |elem| lower_elements.push(map_window_element(elem)),
        );
        if let Some(shadow_element) = self.window.shadow_render_element(
            renderer,
            (render_location - self.window.geometry().loc).to_physical_precise_round(output_scale),
            None,
            output_scale,
            scale,
            alpha,
        ) {
            push(shadow_element);
        }
        for elem in lower_elements.into_iter() {
            push(elem);
        }

        let non_exclusive_geometry = {
            let layers = layer_map_for_output(output);
            layers.non_exclusive_zone()
        };

        let gaps = (theme.gaps.0 as i32, theme.gaps.1 as i32);
        let thickness = self.indicator_thickness.max(1);

        if let Some(t) = &self.snapping_zone
            && &self.cursor_output == output
        {
            let base_color = theme.palette.neutral_9;
            let overlay_geometry = t.overlay_geometry(non_exclusive_geometry, gaps);

            push(
                IndicatorShader::element(
                    renderer,
                    Key::Window(Usage::SnappingIndicator, self.window.key()),
                    overlay_geometry,
                    thickness,
                    [
                        theme.radius_s()[0] as u8,
                        theme.radius_s()[1] as u8,
                        theme.radius_s()[2] as u8,
                        theme.radius_s()[3] as u8,
                    ],
                    1.0,
                    output_scale.x,
                    [
                        active_window_hint.red,
                        active_window_hint.green,
                        active_window_hint.blue,
                    ],
                )
                .into(),
            );
            push(
                BackdropShader::element(
                    renderer,
                    Key::Window(Usage::SnappingIndicator, self.window.key()),
                    t.overlay_geometry(non_exclusive_geometry, gaps),
                    theme.radius_s()[0], // TODO: Fix once shaders support 4 corner radii customization
                    SNAP_OVERLAY_ALPHA,
                    [base_color.red, base_color.green, base_color.blue],
                )
                .into(),
            )
        }

        if let Some(zones) = &self.zones
            && zones.armed
            && &self.cursor_output == output
        {
            let base_color = theme.palette.neutral_9;
            let radii = [
                theme.radius_s()[0] as u8,
                theme.radius_s()[1] as u8,
                theme.radius_s()[2] as u8,
                theme.radius_s()[3] as u8,
            ];

            // Centre a number badge in each zone, so the keyboard shortcuts
            // have something to refer to.
            for (badge, geo) in zones.numbers.iter().zip(zones.context.geometries()) {
                let size = badge.size();
                let centre = Point::<i32, Logical>::from((
                    geo.loc.x + (geo.size.w - size.w) / 2,
                    geo.loc.y + (geo.size.h - size.h) / 2,
                ));
                badge.push_render_elements(
                    renderer,
                    centre.to_physical_precise_round(output_scale),
                    output_scale,
                    1.0,
                    &mut |elem| push(elem.into()),
                );
            }

            // The drop target, above the dim layer. This list is front-to-back
            // — the first element pushed is the topmost — so the target has to
            // be pushed *before* the backdrops it is meant to stand out from,
            // and its border before its own fill.
            if let Some(target) = zones.target.as_ref() {
                let key = Key::Window(Usage::ZoneIndicator(ZONE_TARGET_KEY), self.window.key());
                push(
                    IndicatorShader::element(
                        renderer,
                        key.clone(),
                        target.geometry,
                        thickness,
                        radii,
                        1.0,
                        output_scale.x,
                        [
                            active_window_hint.red,
                            active_window_hint.green,
                            active_window_hint.blue,
                        ],
                    )
                    .into(),
                );
                push(
                    BackdropShader::element(
                        renderer,
                        key,
                        target.geometry,
                        theme.radius_s()[0],
                        SNAP_OVERLAY_ALPHA,
                        [base_color.red, base_color.green, base_color.blue],
                    )
                    .into(),
                );
            }

            // Every other zone, dimmed, so the whole layout is legible the
            // moment the modifier goes down. Zones inside the target are
            // skipped: stacking the dim under the target fill would tint it,
            // and the point is that the target reads at exactly the alpha edge
            // snapping uses.
            let targeted = |idx: usize| {
                zones
                    .target
                    .as_ref()
                    .is_some_and(|target| target.zones.contains(&idx))
            };
            for (idx, geo) in zones.context.geometries().enumerate() {
                if targeted(idx) {
                    continue;
                }
                push(
                    BackdropShader::element(
                        renderer,
                        Key::Window(
                            Usage::ZoneIndicator(idx.min(ZONE_KEY_MAX) as u8),
                            self.window.key(),
                        ),
                        geo,
                        theme.radius_s()[0],
                        zones.inactive_alpha,
                        [base_color.red, base_color.green, base_color.blue],
                    )
                    .into(),
                );
            }
        }
    }

    pub fn element(&self) -> CosmicMapped {
        self.window.clone()
    }

    pub fn window(&self) -> CosmicSurface {
        self.window.active_window()
    }
}

struct NotSend<T>(pub T);
unsafe impl<T> Send for NotSend<T> {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnappingZone {
    Maximize,
    Top,
    TopLeft,
    Left,
    BottomLeft,
    Bottom,
    BottomRight,
    Right,
    TopRight,
}

/// Fill opacity of the highlighted drop target, shared by edge snapping and
/// zone snapping so the two look like the same mechanism.
const SNAP_OVERLAY_ALPHA: f32 = 0.4;
/// Shader cache keys are one byte, so zones past this share a key. Harmless:
/// the key only scopes a cache entry, and layouts this large are pathological.
const ZONE_KEY_MAX: usize = 254;
/// Reserved key for the drop-target overlay.
const ZONE_TARGET_KEY: u8 = 255;

const SNAP_RANGE: i32 = 32;
const SNAP_RANGE_MAXIMIZE: i32 = 22;
const SNAP_RANGE_TOP: i32 = 16;

impl SnappingZone {
    pub fn contains(
        &self,
        point: Point<i32, Local>,
        output_geometry: Rectangle<i32, Local>,
    ) -> bool {
        if !output_geometry.contains(point) {
            return false;
        }
        let top_zone_32 = point.y < output_geometry.loc.y + SNAP_RANGE_MAXIMIZE;
        let top_zone_56 = point.y < output_geometry.loc.y + SNAP_RANGE_MAXIMIZE + SNAP_RANGE_TOP;
        let left_zone = point.x < output_geometry.loc.x + SNAP_RANGE;
        let right_zone = point.x > output_geometry.loc.x + output_geometry.size.w - SNAP_RANGE;
        let bottom_zone = point.y > output_geometry.loc.y + output_geometry.size.h - SNAP_RANGE;
        let left_6th = point.x < output_geometry.loc.x + (output_geometry.size.w / 6);
        let right_6th = point.x > output_geometry.loc.x + (output_geometry.size.w * 5 / 6);
        let top_4th = point.y < output_geometry.loc.y + (output_geometry.size.h / 4);
        let bottom_4th = point.y > output_geometry.loc.y + (output_geometry.size.h * 3 / 4);
        match self {
            SnappingZone::Maximize => top_zone_32 && !left_6th && !right_6th,
            SnappingZone::Top => top_zone_56 && !top_zone_32 && !left_6th && !right_6th,
            SnappingZone::TopLeft => (top_zone_56 && left_6th) || (left_zone && top_4th),
            SnappingZone::Left => left_zone && !top_4th && !bottom_4th,
            SnappingZone::BottomLeft => (bottom_zone && left_6th) || (left_zone && bottom_4th),
            SnappingZone::Bottom => bottom_zone && !left_6th && !right_6th,
            SnappingZone::BottomRight => (bottom_zone && right_6th) || (right_zone && bottom_4th),
            SnappingZone::Right => right_zone && !top_4th && !bottom_4th,
            SnappingZone::TopRight => (top_zone_56 && right_6th) || (right_zone && top_4th),
        }
    }
    pub fn overlay_geometry(
        &self,
        non_exclusive_geometry: Rectangle<i32, Logical>,
        gaps: (i32, i32),
    ) -> Rectangle<i32, Local> {
        match self {
            SnappingZone::Maximize => non_exclusive_geometry.as_local(),
            SnappingZone::Top => TiledCorners::Top.relative_geometry(non_exclusive_geometry, gaps),
            SnappingZone::TopLeft => {
                TiledCorners::TopLeft.relative_geometry(non_exclusive_geometry, gaps)
            }
            SnappingZone::Left => {
                TiledCorners::Left.relative_geometry(non_exclusive_geometry, gaps)
            }
            SnappingZone::BottomLeft => {
                TiledCorners::BottomLeft.relative_geometry(non_exclusive_geometry, gaps)
            }
            SnappingZone::Bottom => {
                TiledCorners::Bottom.relative_geometry(non_exclusive_geometry, gaps)
            }
            SnappingZone::BottomRight => {
                TiledCorners::BottomRight.relative_geometry(non_exclusive_geometry, gaps)
            }
            SnappingZone::Right => {
                TiledCorners::Right.relative_geometry(non_exclusive_geometry, gaps)
            }
            SnappingZone::TopRight => {
                TiledCorners::TopRight.relative_geometry(non_exclusive_geometry, gaps)
            }
        }
    }
}

pub struct MoveGrab {
    window: CosmicMapped,
    start_data: GrabStartData,
    seat: Seat<State>,
    cursor_output: Output,
    window_outputs: HashSet<Output>,
    previous: ManagedLayer,
    release: ReleaseMode,
    edge_snap_threshold: f64,
    // SAFETY: This is only used on drop which will always be on the main thread
    evlh: NotSend<LoopHandle<'static, State>>,
}

impl MoveGrab {
    fn update_location(&mut self, state: &mut State, location: Point<f64, Logical>) {
        let mut shell = state.common.shell.write();

        let Some(current_output) = shell
            .outputs()
            .find(|output| {
                output
                    .geometry()
                    .as_logical()
                    .overlaps_or_touches(Rectangle::new(location.to_i32_floor(), (0, 0).into()))
            })
            .cloned()
        else {
            return;
        };
        if self.cursor_output != current_output {
            shell
                .workspaces
                .active_mut(&self.cursor_output)
                .unwrap()
                .tiling_layer
                .cleanup_drag();
            self.cursor_output = current_output.clone();
        }

        let mut borrow = self
            .seat
            .user_data()
            .get::<SeatMoveGrabState>()
            .map(|s| s.lock().unwrap());
        if let Some(grab_state) = borrow.as_mut().and_then(|s| s.as_mut()) {
            grab_state.location = location;
            grab_state.cursor_output = self.cursor_output.clone();

            let mut window_geo = self.window.geometry();
            window_geo.loc += location.to_i32_round() + grab_state.window_offset;

            if matches!(self.previous, ManagedLayer::Floating | ManagedLayer::Sticky) {
                let loc = grab_state.window_offset.to_f64() + grab_state.location;
                let size = window_geo.size.to_f64();
                let output_geom = self.cursor_output.geometry().to_f64().as_logical();
                let output_loc = output_geom.loc;
                let output_size = output_geom.size;

                grab_state.location.x = if (loc.x - output_loc.x).abs() < self.edge_snap_threshold {
                    output_loc.x - grab_state.window_offset.x as f64
                } else if ((loc.x + size.w) - (output_loc.x + output_size.w)).abs()
                    < self.edge_snap_threshold
                {
                    output_loc.x + output_size.w - grab_state.window_offset.x as f64 - size.w
                } else {
                    grab_state.location.x
                };
                grab_state.location.y = if (loc.y - output_loc.y).abs() < self.edge_snap_threshold {
                    output_loc.y - grab_state.window_offset.y as f64
                } else if ((loc.y + size.h) - (output_loc.y + output_size.h)).abs()
                    < self.edge_snap_threshold
                {
                    output_loc.y + output_size.h - grab_state.window_offset.y as f64 - size.h
                } else {
                    grab_state.location.y
                };
            }

            for output in shell.outputs() {
                if let Some(overlap) = output.geometry().as_logical().intersection(window_geo) {
                    if self.window_outputs.insert(output.clone()) {
                        self.window.output_enter(output, overlap);
                        if let Some(indicator) =
                            grab_state.stacking_indicator.as_ref().map(|x| &x.0)
                        {
                            indicator.output_enter(output);
                        }
                    }
                } else if self.window_outputs.remove(output) {
                    self.window.output_leave(output);
                    if let Some(indicator) = grab_state.stacking_indicator.as_ref().map(|x| &x.0) {
                        indicator.output_leave(output);
                    }
                }
            }

            let indicator_location = shell.stacking_indicator(&current_output, self.previous);
            if indicator_location.is_some() != grab_state.stacking_indicator.is_some() {
                grab_state.stacking_indicator = indicator_location.map(|geo| {
                    let size = geo.size.as_logical();
                    let element = StackHover::new(
                        state.common.event_loop_handle.clone(),
                        size,
                        state.common.theme.clone(),
                    );
                    for output in &self.window_outputs {
                        element.output_enter(output);
                    }
                    (element, geo.loc.as_logical())
                });
            }

            // Check for overlapping with zones
            if grab_state.previous == ManagedLayer::Floating {
                let zones_config = &state.common.config.cosmic_conf.zones;
                let zone_mode = zones_config.enabled
                    && self.seat.get_keyboard().is_some_and(|keyboard| {
                        zones::modifiers_match(&zones_config.modifier, &keyboard.modifier_state())
                    });

                if zone_mode {
                    // User-defined zones replace edge snapping outright while
                    // the modifier is held, rather than competing with it.
                    grab_state.snapping_zone = None;

                    // Keyed on the output rather than on `zones` being `Some`,
                    // so a layout that resolves to nothing is remembered as a
                    // miss instead of being re-resolved — and re-missed — on
                    // every pointer event.
                    if grab_state.zones_output.as_ref() != Some(&current_output) {
                        if let Some(previous) = grab_state.zones.take() {
                            previous.output_leave();
                        }
                        grab_state.zones_output = Some(current_output.clone());

                        let gaps = {
                            let gaps = state.common.theme.cosmic().gaps;
                            zones_config.gaps_or((gaps.0 as i32, gaps.1 as i32))
                        };
                        let workspace_id = shell
                            .active_space(&current_output)
                            .and_then(|workspace| workspace.id.clone());

                        grab_state.zones = ZoneContext::resolve(
                            zones_config,
                            &current_output,
                            workspace_id.as_deref(),
                            gaps,
                        )
                        .map(|context| {
                            let numbers = if zones_config.show_zone_numbers {
                                (1..=context.len())
                                    .map(|n| {
                                        let number = ZoneNumber::new(
                                            state.common.event_loop_handle.clone(),
                                            n,
                                            state.common.theme.clone(),
                                        );
                                        number.output_enter(&current_output);
                                        number
                                    })
                                    .collect()
                            } else {
                                Vec::new()
                            };
                            ZoneDragState {
                                output: current_output.clone(),
                                context,
                                target: None,
                                armed: true,
                                inactive_alpha: (zones_config.inactive_opacity.min(100) as f32
                                    / 100.0)
                                    * SNAP_OVERLAY_ALPHA,
                                numbers,
                            }
                        });
                    }

                    if let Some(zones) = grab_state.zones.as_mut() {
                        zones.armed = true;
                        let point = location
                            .as_global()
                            .to_local(&current_output)
                            .to_i32_floor();
                        zones.target = zones.context.hit(point);
                    }

                    drop(borrow);
                    return;
                }

                // Disarm rather than discard, so tapping the modifier off and
                // on again does not rebuild every badge. Clearing the target is
                // what stops the drop from snapping to a zone.
                if let Some(zones) = grab_state.zones.as_mut() {
                    zones.armed = false;
                    zones.target = None;
                }
                let output_geometry = current_output.geometry().to_local(&current_output);
                grab_state.snapping_zone = [
                    SnappingZone::Maximize,
                    SnappingZone::Top,
                    SnappingZone::TopLeft,
                    SnappingZone::Left,
                    SnappingZone::BottomLeft,
                    SnappingZone::Bottom,
                    SnappingZone::BottomRight,
                    SnappingZone::Right,
                    SnappingZone::TopRight,
                ]
                .iter()
                .find(|&x| {
                    x.contains(
                        location
                            .as_global()
                            .to_local(&current_output)
                            .to_i32_floor(),
                        output_geometry,
                    )
                })
                .cloned();
            }
        }
        drop(borrow);
    }
}

impl PointerGrab<State> for MoveGrab {
    fn motion(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(PointerFocusTarget, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        self.update_location(state, event.location);

        // While the grab is active, no client has pointer focus
        handle.motion(state, None, event);
        if !self.window.alive() {
            handle.unset_grab(self, state, event.serial, event.time, true);
        }
    }

    fn relative_motion(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(PointerFocusTarget, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        // While the grab is active, no client has pointer focus
        handle.relative_motion(state, None, event);
    }

    fn button(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &ButtonEvent,
    ) {
        handle.button(state, event);
        match self.release {
            ReleaseMode::NoMouseButtons => {
                if handle.current_pressed().is_empty() {
                    handle.unset_grab(self, state, event.serial, event.time, true);
                }
            }
            ReleaseMode::Click => {
                if event.state == ButtonState::Pressed {
                    handle.unset_grab(self, state, event.serial, event.time, true);
                }
            }
        }
    }

    fn axis(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        details: AxisFrame,
    ) {
        handle.axis(state, details);
    }

    fn frame(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>) {
        handle.frame(data)
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event)
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event)
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event)
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event)
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event)
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event)
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event)
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event)
    }

    fn start_data(&self) -> &PointerGrabStartData<State> {
        match &self.start_data {
            GrabStartData::Pointer(start_data) => start_data,
            _ => unreachable!(),
        }
    }

    fn unset(&mut self, _data: &mut State) {}
}

impl TouchGrab<State> for MoveGrab {
    fn down(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        _focus: Option<(PointerFocusTarget, Point<f64, Logical>)>,
        event: &touch::DownEvent,
    ) {
        handle.down(data, None, event)
    }

    fn up(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        event: &touch::UpEvent,
    ) {
        if event.slot == <Self as TouchGrab<State>>::start_data(self).slot {
            handle.unset_grab(self, data);
        }

        handle.up(data, event);
    }

    fn motion(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        _focus: Option<(PointerFocusTarget, Point<f64, Logical>)>,
        event: &touch::MotionEvent,
    ) {
        if event.slot == <Self as TouchGrab<State>>::start_data(self).slot {
            self.update_location(data, event.location);
        }

        handle.motion(data, None, event);
    }

    fn frame(&mut self, data: &mut State, handle: &mut TouchInnerHandle<'_, State>) {
        handle.frame(data)
    }

    fn cancel(&mut self, data: &mut State, handle: &mut TouchInnerHandle<'_, State>) {
        handle.unset_grab(self, data);
    }

    fn shape(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        event: &touch::ShapeEvent,
    ) {
        handle.shape(data, event)
    }

    fn orientation(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        event: &touch::OrientationEvent,
    ) {
        handle.orientation(data, event)
    }

    fn start_data(&self) -> &TouchGrabStartData<State> {
        match &self.start_data {
            GrabStartData::Touch(start_data) => start_data,
            _ => unreachable!(),
        }
    }

    fn unset(&mut self, _data: &mut State) {}
}

impl MoveGrab {
    pub fn new(
        start_data: GrabStartData,
        window: CosmicMapped,
        seat: &Seat<State>,
        initial_window_location: Point<i32, Global>,
        cursor_output: Output,
        indicator_thickness: u8,
        edge_snap_threshold: f64,
        previous_layer: ManagedLayer,
        release: ReleaseMode,
        evlh: LoopHandle<'static, State>,
    ) -> MoveGrab {
        // false-positive: `Output`s hash is based on it's inner ptr
        #[allow(clippy::mutable_key_type)]
        let mut outputs = HashSet::new();
        outputs.insert(cursor_output.clone());
        window.output_enter(&cursor_output, window.geometry()); // not accurate but...
        window.moved_since_mapped.store(true, Ordering::SeqCst);

        let grab_state = MoveGrabState {
            window: window.clone(),
            window_offset: (initial_window_location
                - start_data.location().as_global().to_i32_round())
            .as_logical(),
            indicator_thickness,
            start: Instant::now(),
            stacking_indicator: None,
            snapping_zone: None,
            zones: None,
            zones_output: None,
            previous: previous_layer,
            location: start_data.location(),
            cursor_output: cursor_output.clone(),
        };

        *seat
            .user_data()
            .get::<SeatMoveGrabState>()
            .unwrap()
            .lock()
            .unwrap() = Some(grab_state);

        {
            let cursor_state = seat.user_data().get::<CursorState>().unwrap();
            cursor_state.lock().unwrap().set_shape(CursorIcon::Grabbing);
        }

        MoveGrab {
            window,
            start_data,
            seat: seat.clone(),
            cursor_output,
            window_outputs: outputs,
            previous: previous_layer,
            release,
            edge_snap_threshold,
            evlh: NotSend(evlh),
        }
    }

    pub fn is_tiling_grab(&self) -> bool {
        self.previous == ManagedLayer::Tiling
    }

    pub fn is_touch_grab(&self) -> bool {
        match self.start_data {
            GrabStartData::Touch(_) => true,
            GrabStartData::Pointer(_) => false,
        }
    }
}

impl Drop for MoveGrab {
    fn drop(&mut self) {
        // No more buttons are pressed, release the grab.
        let output = self.cursor_output.clone();
        let seat = self.seat.clone();
        // false-positive: `Output`s hash is based on it's inner ptr
        #[allow(clippy::mutable_key_type)]
        let window_outputs = self.window_outputs.drain().collect::<HashSet<_>>();
        let previous = self.previous;
        let window = self.window.clone();
        let is_touch_grab = matches!(self.start_data, GrabStartData::Touch(_));
        let cursor_output = self.cursor_output.clone();

        let _ = self.evlh.0.insert_idle(move |state| {
            // Recorded during the drop and persisted after the shell lock is
            // released, since saving touches config rather than shell state.
            let mut zone_memory: Option<(String, String, Vec<usize>)> = None;

            let position: Option<(CosmicMapped, Point<i32, Global>)> = if let Some(grab_state) =
                seat.user_data()
                    .get::<SeatMoveGrabState>()
                    .and_then(|s| s.lock().unwrap().take())
            {
                // Pairs the `output_enter` each zone badge was created with.
                // The badges are about to be dropped either way, but the output
                // keeps its own list of entered elements.
                if let Some(zones) = grab_state.zones.as_ref() {
                    zones.output_leave();
                }

                if grab_state.window.alive() {
                    let window_location =
                        (grab_state.location.to_i32_round() + grab_state.window_offset).as_global();
                    let mut shell = state.common.shell.write();

                    let workspace_handle = shell.active_space(&output).unwrap().handle;
                    for old_output in window_outputs.iter().filter(|o| *o != &output) {
                        grab_state.window.output_leave(old_output);
                    }

                    for (window, _) in grab_state.window.windows() {
                        toplevel_enter_output(&window, &output);
                        if previous != ManagedLayer::Sticky {
                            toplevel_enter_workspace(&window, &workspace_handle);
                        }
                    }

                    match previous {
                        ManagedLayer::Sticky => {
                            grab_state.window.set_geometry(Rectangle::new(
                                window_location,
                                grab_state.window.geometry().size.as_global(),
                            ));
                            let set = shell.workspaces.sets.get_mut(&output).unwrap();
                            let (window, location) = set
                                .sticky_layer
                                .drop_window(grab_state.window, window_location.to_local(&output));

                            Some((window, location.to_global(&output)))
                        }
                        ManagedLayer::Tiling
                            if shell.active_space(&output).unwrap().tiling_enabled =>
                        {
                            let (window, location) = shell
                                .active_space_mut(&output)
                                .unwrap()
                                .tiling_layer
                                .drop_window(grab_state.window);
                            Some((window, location.to_global(&output)))
                        }
                        _ => {
                            grab_state.window.set_geometry(Rectangle::new(
                                window_location,
                                grab_state.window.geometry().size.as_global(),
                            ));
                            let theme = shell.theme.clone();
                            let workspace = shell.active_space_mut(&output).unwrap();
                            let (window, location) = workspace.floating_layer.drop_window(
                                grab_state.window,
                                window_location.to_local(&workspace.output),
                            );

                            // A zone target wins over edge snapping: the two are
                            // mutually exclusive during the drag, and the user
                            // was looking at the zone overlay when they let go.
                            if matches!(previous, ManagedLayer::Floating)
                                && let Some(zones) = grab_state.zones.as_ref()
                                && let Some(hit) = zones.target.as_ref()
                            {
                                // As below: `last_geometry` holds the pre-drag
                                // geometry (set in FloatingLayout::unmap), and
                                // unsnapping restores that size. Preserve it
                                // across the snap.
                                let pre_drag_geometry = *window.last_geometry.lock().unwrap();

                                workspace.floating_layer.snap_to(
                                    &window,
                                    &FloatingTiled::Zone {
                                        layout: zones.context.layout_id.clone(),
                                        zones: hit.zones.clone(),
                                        rect: hit.rect,
                                        gap: zones.context.gap(),
                                    },
                                );

                                if let Some(geo) = pre_drag_geometry {
                                    *window.last_geometry.lock().unwrap() = Some(geo);
                                }

                                zone_memory = Some((
                                    window.active_window().app_id(),
                                    zones.context.layout_id.clone(),
                                    hit.zones.clone(),
                                ));
                            } else if matches!(previous, ManagedLayer::Floating)
                                && let Some(sz) = grab_state.snapping_zone
                            {
                                // `last_geometry` was set to the pre-drag geometry(in FloatingLayout::unmap).
                                // Snapshot it here and restore it after so "restore-to-floating" goes back to where the user had the window.
                                let pre_drag_geometry = *window.last_geometry.lock().unwrap();

                                if sz == SnappingZone::Maximize {
                                    shell.maximize_toggle(
                                        &window,
                                        &seat,
                                        &state.common.event_loop_handle,
                                    );
                                    if let Some(geo) = pre_drag_geometry
                                        && let Some(state) =
                                            window.maximized_state.lock().unwrap().as_mut()
                                    {
                                        state.original_geometry = geo;
                                    }
                                } else {
                                    let directions = match sz {
                                        SnappingZone::Maximize => vec![],
                                        SnappingZone::Top => vec![Direction::Up],
                                        SnappingZone::TopLeft => {
                                            vec![Direction::Up, Direction::Left]
                                        }
                                        SnappingZone::Left => vec![Direction::Left],
                                        SnappingZone::BottomLeft => {
                                            vec![Direction::Down, Direction::Left]
                                        }
                                        SnappingZone::Bottom => vec![Direction::Down],
                                        SnappingZone::BottomRight => {
                                            vec![Direction::Down, Direction::Right]
                                        }
                                        SnappingZone::Right => vec![Direction::Right],
                                        SnappingZone::TopRight => {
                                            vec![Direction::Up, Direction::Right]
                                        }
                                    };
                                    for direction in directions {
                                        workspace.floating_layer.move_element(
                                            direction,
                                            &seat,
                                            ManagedLayer::Floating,
                                            &theme,
                                            &window,
                                        );
                                    }
                                    if let Some(geo) = pre_drag_geometry {
                                        *window.last_geometry.lock().unwrap() = Some(geo);
                                    }
                                }
                            }
                            Some((window, location.to_global(&output)))
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            };

            if let Some((app_id, layout, zones)) = zone_memory {
                remember_app_zone(state, app_id, layout, zones);
            }

            let mut shell = state.common.shell.write();
            shell
                .workspaces
                .active_mut(&cursor_output)
                .unwrap()
                .tiling_layer
                .cleanup_drag();
            shell.set_overview_mode(None, state.common.event_loop_handle.clone());
            drop(shell);

            {
                let cursor_state = seat.user_data().get::<CursorState>().unwrap();
                cursor_state.lock().unwrap().unset_shape();
            }

            if let Some((mapped, position)) = position {
                let serial = SERIAL_COUNTER.next_serial();
                if !is_touch_grab {
                    let pointer = seat.get_pointer().unwrap();
                    let current_location = pointer.current_location();

                    if let Some((target, offset)) = mapped.focus_under(
                        current_location - position.as_logical().to_f64(),
                        WindowSurfaceType::ALL,
                        &seat,
                    ) {
                        pointer.motion(
                            state,
                            Some((
                                target,
                                position.as_logical().to_f64() - window.geometry().loc.to_f64()
                                    + offset,
                            )),
                            &MotionEvent {
                                location: pointer.current_location(),
                                serial,
                                time: state.common.clock.now().as_millis(),
                            },
                        );
                    }
                }
                Shell::set_focus(
                    state,
                    Some(&KeyboardFocusTarget::from(mapped)),
                    &seat,
                    Some(serial),
                    false,
                )
            }
        });
    }
}

/// Persist where an app was last snapped, for `remember_apps`.
///
/// Writes through cosmic-config so the setting survives a restart, and updates
/// the in-memory copy so it takes effect without waiting for the config watch
/// to fire.
///
/// This fires on every zone drop-snap, which is why it has its own key: written
/// into the `zones` blob it would rewrite the user's layouts from whatever
/// snapshot this process last read, and an editor save landing in between would
/// be silently undone.
fn remember_app_zone(state: &mut State, app_id: String, layout: String, zones: Vec<usize>) {
    if app_id.is_empty() || !state.common.config.cosmic_conf.zones.remember_apps {
        return;
    }

    let memory = AppZoneMemory { layout, zones };
    let memories = &mut state.common.config.cosmic_conf.zone_app_memory;
    if memories.get(&app_id) == Some(&memory) {
        return;
    }
    memories.insert(app_id, memory);

    let snapshot = memories.clone();
    if let Err(err) = state
        .common
        .config
        .cosmic_helper
        .set("zone_app_memory", &snapshot)
    {
        warn!(?err, "failed to persist app zone memory");
    }
    state.common.shell.write().set_zone_app_memory(snapshot);
}
