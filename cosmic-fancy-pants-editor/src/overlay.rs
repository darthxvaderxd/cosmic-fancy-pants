// SPDX-License-Identifier: GPL-3.0-only

//! The fullscreen overlay drawn on each output: a toolbar plus the interactive
//! zone canvas.

use cosmic::{
    Element,
    iced::{Alignment, Length},
    widget,
};
use cosmic_comp_config::zones::ZonesConfig;

use crate::state::{Editor, Message};

pub fn view(app: &Editor, id: cosmic::iced::window::Id) -> Element<'_, Message> {
    let Some(output) = app.outputs.get(&id) else {
        return widget::text("").into();
    };
    let Some(layout) = app.layout_for(id) else {
        return widget::text("no layouts available").into();
    };

    let zones = widget::canvas(crate::zone_canvas::ZoneCanvas {
        zones: &layout.zones,
        surface_id: id,
        show_numbers: app.config.show_zone_numbers,
    })
    .width(Length::Fill)
    .height(Length::Fill);
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
