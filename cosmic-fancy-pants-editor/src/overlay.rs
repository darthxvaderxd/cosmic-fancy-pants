// SPDX-License-Identifier: GPL-3.0-only

//! The fullscreen overlay drawn on each output.
//!
//! Zones are laid out with a proportional grid rather than absolute pixels, so
//! the drawing stays correct whatever size the compositor gives the surface.

use cosmic::{
    Element,
    iced::{Alignment, Length},
    widget,
};
use cosmic_comp_config::zones::{ZoneRect, ZonesConfig};

use crate::state::{Editor, Message};

/// Row of zone rectangles positioned by fraction.
///
/// iced has no absolute positioning primitive here, so zones are placed by
/// nesting proportional spacers: a column of rows, each row a sequence of
/// horizontal spacers and zone tiles sized by `FillPortion`. Fractions are
/// scaled to integer portions, which is exact for the built-in templates and
/// close enough visually for hand-authored ones.
const PORTION_SCALE: f32 = 10_000.0;

fn portion(fraction: f64) -> u16 {
    ((fraction as f32 * PORTION_SCALE).round() as i32).clamp(1, u16::MAX as i32) as u16
}

pub fn view(app: &Editor, id: cosmic::iced::window::Id) -> Element<'_, Message> {
    let Some(output) = app.outputs.get(&id) else {
        return widget::text("").into();
    };
    let Some(layout) = app.layout_for(id) else {
        return widget::text("no layouts available").into();
    };

    let zones = zone_canvas(&layout.zones);
    let toolbar = toolbar(
        id,
        &output.name,
        output.logical_size,
        &layout.name,
        &app.config,
    );

    widget::container(
        widget::Column::new()
            .push(toolbar)
            .push(
                widget::container(zones)
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .spacing(0),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .class(cosmic::theme::Container::Background)
    .into()
}

/// Draw the zones as proportional tiles.
///
/// Rows are derived by splitting on distinct y-bands. This renders any layout
/// whose zones form rows — which covers every built-in template — and falls
/// back to a single row for irregular layouts rather than drawing nothing.
fn zone_canvas(zones: &[ZoneRect]) -> Element<'_, Message> {
    let mut bands: Vec<(f64, f64)> = Vec::new();
    for zone in zones {
        let band = (zone.y, zone.bottom());
        if !bands
            .iter()
            .any(|(t, b)| (t - band.0).abs() < 1e-6 && (b - band.1).abs() < 1e-6)
        {
            bands.push(band);
        }
    }
    bands.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut column = widget::Column::new()
        .width(Length::Fill)
        .height(Length::Fill);
    let mut drawn = 0usize;

    for (top, bottom) in &bands {
        let mut row_zones: Vec<(usize, &ZoneRect)> = zones
            .iter()
            .enumerate()
            .filter(|(_, z)| (z.y - top).abs() < 1e-6 && (z.bottom() - bottom).abs() < 1e-6)
            .collect();
        row_zones
            .sort_by(|(_, a), (_, b)| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));

        let mut row = widget::Row::new().width(Length::Fill).height(Length::Fill);
        let mut cursor = 0.0f64;
        for (index, zone) in row_zones {
            // Gap left of this zone, if the layout does not start at the edge.
            if zone.x - cursor > 1e-6 {
                row = row.push(
                    widget::space::horizontal()
                        .width(Length::FillPortion(portion(zone.x - cursor))),
                );
            }
            row = row.push(zone_tile(index, *zone));
            cursor = zone.right();
            drawn += 1;
        }
        if 1.0 - cursor > 1e-6 {
            row = row.push(
                widget::space::horizontal().width(Length::FillPortion(portion(1.0 - cursor))),
            );
        }

        column =
            column.push(widget::container(row).height(Length::FillPortion(portion(bottom - top))));
    }

    if drawn != zones.len() {
        // Irregular layout: at least show the count so the state is not a
        // silently blank screen.
        return widget::container(widget::text(format!(
            "{} zones (irregular layout — visual editing not yet supported)",
            zones.len()
        )))
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into();
    }

    column.into()
}

fn zone_tile<'a>(index: usize, _zone: ZoneRect) -> Element<'a, Message> {
    widget::container(
        widget::text(format!("{}", index + 1))
            .size(32)
            .align_x(Alignment::Center),
    )
    .center_x(Length::Fill)
    .center_y(Length::Fill)
    .width(Length::Fill)
    .height(Length::Fill)
    .padding(8)
    .class(cosmic::theme::Container::Card)
    .into()
}

fn toolbar<'a>(
    id: cosmic::iced::window::Id,
    output_name: &'a str,
    logical_size: (u32, u32),
    layout_name: &'a str,
    config: &'a ZonesConfig,
) -> Element<'a, Message> {
    // Stable ordering: HashMap iteration order would reshuffle the picker on
    // every redraw.
    let mut ids: Vec<&String> = config.layouts.keys().collect();
    ids.sort();

    let mut picker = widget::Row::new().spacing(8).align_y(Alignment::Center);
    for layout_id in ids {
        let name = config
            .layouts
            .get(layout_id)
            .map(|l| l.name.as_str())
            .unwrap_or(layout_id.as_str());
        let selected = name == layout_name;
        let mut button = widget::button::standard(name);
        if !selected {
            button = button.on_press(Message::SelectLayout(id, layout_id.clone()));
        }
        picker = picker.push(button);
    }

    widget::container(
        widget::Row::new()
            .push(widget::text::heading(format!(
                "Zones — {output_name} ({}x{})",
                logical_size.0, logical_size.1
            )))
            .push(widget::space::horizontal())
            .push(picker)
            .push(widget::space::horizontal().width(Length::Fixed(16.0)))
            .push(widget::button::standard("Cancel").on_press(Message::Cancel))
            .push(widget::button::suggested("Save").on_press(Message::Save))
            .spacing(8)
            .align_y(Alignment::Center),
    )
    .padding(12)
    .width(Length::Fill)
    .class(cosmic::theme::Container::Primary)
    .into()
}
