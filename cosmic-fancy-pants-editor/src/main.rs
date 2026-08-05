// SPDX-License-Identifier: GPL-3.0-only

//! Zone layout editor for COSMIC Fancy Pants.
//!
//! Opens one fullscreen layer-shell overlay per output, so zones are edited at
//! true size against the real desktop rather than in a scaled-down preview.
//! Saving writes `ZonesConfig` through cosmic-config, which the compositor
//! picks up live via its existing config watch — no restart.

mod overlay;
mod state;

use cosmic::app::Settings;

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,cosmic_fancy_pants_editor=info".into()),
        )
        .init();

    // No initial window: every surface this app creates is a layer shell
    // overlay bound to a specific output.
    cosmic::app::run::<state::Editor>(Settings::default().no_main_window(true), ())
}
