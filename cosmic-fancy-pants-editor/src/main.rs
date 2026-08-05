// SPDX-License-Identifier: GPL-3.0-only

//! Zone layout editor for COSMIC Fancy Pants.
//!
//! Opens one fullscreen layer-shell overlay per output, so zones are edited at
//! true size against the real desktop rather than in a scaled-down preview.
//! Saving writes `ZonesConfig` through cosmic-config, which the compositor
//! picks up live via its existing config watch — no restart.

mod overlay;
mod state;
mod zone_canvas;

use cosmic::app::Settings;

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,cosmic_fancy_pants_editor=info".into()),
        )
        .init();

    // Both of these are compositor knowledge with no Wayland equivalent:
    // workspace ids are internal, and nothing tells a client which output the
    // user was looking at. Absent — launched from the app library — the editor
    // offers per-monitor assignment only.
    let launch = state::Launch {
        workspace: flag("--workspace"),
        output: flag("--output"),
    };

    // No initial window: every surface this app creates is a layer shell
    // overlay bound to a specific output.
    cosmic::app::run::<state::Editor>(Settings::default().no_main_window(true), launch)
}

/// Value following `name` on the command line, if it is there and non-empty.
fn flag(name: &str) -> Option<String> {
    std::env::args()
        .skip_while(|arg| arg != name)
        .nth(1)
        .filter(|value| !value.is_empty())
}
