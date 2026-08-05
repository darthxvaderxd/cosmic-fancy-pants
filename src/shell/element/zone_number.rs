// SPDX-License-Identifier: GPL-3.0-only

//! The number badge drawn on each zone while a zone-snapping drag is active.
//!
//! Modelled on [`stack_hover`](super::stack_hover): a small [`IcedElement`]
//! created for the duration of a drag, positioned by the caller.

use crate::{
    backend::render::element::AsGlowRenderer,
    utils::iced::{IcedElement, IcedRenderElement, Program},
};

use calloop::LoopHandle;
use cosmic::{
    Apply,
    iced::{
        Alignment,
        core::{Background, Border, Color, Length},
    },
    theme,
    widget::{container, text},
};
use smithay::{
    backend::renderer::ImportMem,
    desktop::space::SpaceElement,
    output::Output,
    utils::{Logical, Physical, Point, Rectangle, Scale, Size},
};

/// Side length of the badge, in logical pixels.
const BADGE_SIZE: i32 = 48;

#[derive(Debug)]
pub struct ZoneNumber {
    elem: IcedElement<ZoneNumberInternal>,
}

impl ZoneNumber {
    pub fn new(
        evlh: LoopHandle<'static, crate::state::State>,
        number: usize,
        mut theme: cosmic::Theme,
    ) -> ZoneNumber {
        theme.transparent = theme.cosmic().frosted_system_interface;

        let elem = IcedElement::new(
            ZoneNumberInternal { number },
            Size::<i32, Logical>::new(BADGE_SIZE, BADGE_SIZE),
            evlh,
            theme,
        );

        ZoneNumber { elem }
    }

    /// Size of the badge, so callers can centre it within a zone.
    pub fn size(&self) -> Size<i32, Logical> {
        Size::from((BADGE_SIZE, BADGE_SIZE))
    }

    pub fn push_render_elements<R>(
        &self,
        renderer: &mut R,
        location: Point<i32, Physical>,
        scale: Scale<f64>,
        alpha: f32,
        push_above: &mut dyn FnMut(IcedRenderElement<R>),
    ) where
        R: AsGlowRenderer + ImportMem,
        R::TextureId: Send + Clone + 'static,
    {
        self.elem.push_render_elements(
            renderer,
            location,
            scale,
            alpha,
            self.elem
                .with_theme(|theme| theme.cosmic().radius_s())
                .map(|x| x.round() as u8),
            push_above,
            None,
        );
    }

    pub fn output_enter(&self, output: &Output) {
        self.elem
            .output_enter(output, Rectangle::default() /*unused*/);
    }

    pub fn output_leave(&self, output: &Output) {
        self.elem.output_leave(output);
    }
}

pub struct ZoneNumberInternal {
    number: usize,
}

impl Program for ZoneNumberInternal {
    type Message = ();

    fn view(&self) -> cosmic::Element<'_, Self::Message> {
        text::title2(format!("{}", self.number))
            .apply(container)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center)
            .width(Length::Fill)
            .height(Length::Fill)
            .class(theme::Container::custom(|theme| {
                let mut background = theme.cosmic().accent_color();
                if theme.transparent {
                    background.alpha = theme
                        .cosmic()
                        .alpha_map
                        .blurred_alpha(theme.cosmic().frosted);
                }

                container::Style {
                    snap: true,
                    icon_color: Some(Color::from(theme.cosmic().accent.on)),
                    text_color: Some(Color::from(theme.cosmic().accent.on)),
                    background: Some(Background::Color(background.into())),
                    border: Border {
                        radius: theme.cosmic().radius_s().into(),
                        width: 0.0,
                        color: Color::TRANSPARENT,
                    },
                    shadow: Default::default(),
                }
            }))
            .into()
    }
}
