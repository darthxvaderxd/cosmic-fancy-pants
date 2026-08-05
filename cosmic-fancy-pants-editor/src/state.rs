// SPDX-License-Identifier: GPL-3.0-only

//! Editor application state.

use std::collections::HashMap;

use cosmic::{
    app::{Core, Task},
    cctk::sctk::reexports::client::protocol::wl_output::WlOutput,
    iced::{
        Subscription,
        event::wayland::{Event as WaylandEvent, OutputEvent},
        platform_specific::shell::wayland::commands::layer_surface::{
            Anchor, KeyboardInteractivity, Layer,
        },
        runtime::platform_specific::wayland::layer_surface::{
            IcedOutput, SctkLayerSurfaceSettings,
        },
    },
    surface,
};
use cosmic_comp_config::{
    workspace::OutputMatch,
    zones::{DEFAULT_LAYOUT_ID, ZoneLayout, ZoneRect, ZonesConfig, is_default_layout_id},
};
use cosmic_config::{ConfigGet, ConfigSet};
use tracing::{error, info, warn};

use crate::overlay;

/// cosmic-config namespace the compositor reads its configuration from. Writing
/// the `zones` key here is what the compositor's watch picks up.
const COMP_CONFIG_ID: &str = "com.system76.CosmicComp";
const COMP_CONFIG_VERSION: u64 = 1;
const ZONES_KEY: &str = "zones";

/// An output we have opened an overlay on.
pub struct EditorOutput {
    /// Kept so removal can be matched by object identity: `OutputEvent::Removed`
    /// carries no `OutputInfo`, so there is no name to match on.
    pub wl_output: WlOutput,
    pub name: String,
    pub logical_size: (u32, u32),
    /// Layout id currently being edited on this output.
    pub layout_id: String,
}

impl EditorOutput {
    pub fn output_match(&self) -> OutputMatch {
        // EDID is not exposed to Wayland clients, so the editor matches on
        // connector name only. The compositor writes the richer match itself
        // when it has one; overwriting per-output entries by name is enough for
        // assignment, and a name-only match still survives a reboot.
        OutputMatch {
            name: self.name.clone(),
            edid: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    /// Wayland told us about an output appearing, changing, or going away.
    /// Boxed because `OutputEvent` carries a whole `OutputInfo`, which would
    /// otherwise set the size of every message in this enum.
    Output(Box<OutputEvent>, WlOutput, OutputInfoLite),
    /// Pick a different layout for the output showing this surface.
    SelectLayout(cosmic::iced::window::Id, String),
    /// A boundary drag produced a new set of zones for this surface.
    ZonesEdited(cosmic::iced::window::Id, Vec<ZoneRect>),
    /// Adjust the gap between snapped windows, in logical pixels.
    SetGap(Option<u32>),
    /// Choose whether Save assigns to the monitor or the active workspace.
    SetScope(Scope),
    Save,
    Cancel,
}

/// The subset of `OutputInfo` the editor needs, kept `Clone + Debug` so it can
/// ride along in a message.
#[derive(Debug, Clone, Default)]
pub struct OutputInfoLite {
    pub name: String,
    pub logical_size: (u32, u32),
}

/// What a Save applies the chosen layout to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Assign to the monitor, the default and always available.
    Output,
    /// Assign to the workspace that was active when the editor opened.
    Workspace,
}

/// What the compositor told us when it launched the editor.
///
/// Neither value is discoverable from Wayland: workspace ids are
/// compositor-internal, and there is no protocol for "the output the user was
/// on". Both are absent when the editor is started from the app library.
#[derive(Debug, Default, Clone)]
pub struct Launch {
    pub workspace: Option<String>,
    /// Connector name of the output the editor was opened from.
    pub output: Option<String>,
}

pub struct Editor {
    core: Core,
    /// Workspace the compositor was on when it launched us, if it told us.
    /// `None` means workspace assignment is unavailable.
    pub workspace: Option<String>,
    /// Output the editor was opened from, by connector name. Decides which
    /// overlay's layout a workspace assignment takes.
    opened_on: Option<String>,
    pub scope: Scope,
    /// Working copy. Edits mutate this; `Save` writes it out, `Cancel` drops it.
    pub config: ZonesConfig,
    pub outputs: HashMap<cosmic::iced::window::Id, EditorOutput>,
    /// Handle used to persist on save.
    config_handle: Option<cosmic_config::Config>,
}

impl Editor {
    /// Id of the layout currently shown on a surface, after falling back to
    /// the built-in default so the overlay always has something to draw.
    ///
    /// The picker matches on this rather than on the layout's display name:
    /// names are not unique, so two layouts sharing one would both render as
    /// selected, and when the fallback is in play the output's own `layout_id`
    /// points at a layout that no longer exists.
    pub fn layout_id_for(&self, id: cosmic::iced::window::Id) -> Option<&str> {
        let output = self.outputs.get(&id)?;
        if self.config.layouts.contains_key(&output.layout_id) {
            Some(output.layout_id.as_str())
        } else {
            self.config
                .layouts
                .contains_key(DEFAULT_LAYOUT_ID)
                .then_some(DEFAULT_LAYOUT_ID)
        }
    }

    /// Layout currently selected for a surface.
    pub fn layout_for(&self, id: cosmic::iced::window::Id) -> Option<&ZoneLayout> {
        self.config.layouts.get(self.layout_id_for(id)?)
    }

    fn load_config() -> (Option<cosmic_config::Config>, ZonesConfig) {
        match cosmic_config::Config::new(COMP_CONFIG_ID, COMP_CONFIG_VERSION) {
            Ok(handle) => {
                let config = handle.get::<ZonesConfig>(ZONES_KEY).unwrap_or_else(|err| {
                    // A missing key is the normal first-run case; anything else
                    // means an unreadable or malformed config, and starting from
                    // defaults beats refusing to open.
                    info!(?err, "no existing zone config, starting from defaults");
                    ZonesConfig::default()
                });
                (Some(handle), config)
            }
            Err(err) => {
                error!(?err, "failed to open cosmic-config; edits cannot be saved");
                (None, ZonesConfig::default())
            }
        }
    }

    /// Apply a boundary drag to the layout shown on `id`.
    ///
    /// Editing a built-in template in place would silently redefine it for
    /// every monitor using it, so the first edit forks it into a custom layout
    /// and repoints this output at the fork.
    fn apply_edit(&mut self, id: cosmic::iced::window::Id, zones: Vec<ZoneRect>) {
        let Some(output) = self.outputs.get(&id) else {
            return;
        };
        let current = output.layout_id.clone();

        let target = if is_default_layout_id(&current) {
            let forked = self.next_custom_id();
            let name = self
                .config
                .layouts
                .get(&current)
                .map(|l| format!("{} (edited)", l.name))
                .unwrap_or_else(|| "Custom".to_string());
            self.config
                .layouts
                .insert(forked.clone(), ZoneLayout { name, zones });
            if let Some(output) = self.outputs.get_mut(&id) {
                output.layout_id = forked.clone();
            }
            return;
        } else {
            current
        };

        if let Some(layout) = self.config.layouts.get_mut(&target) {
            layout.zones = zones;
        }
    }

    fn next_custom_id(&self) -> String {
        (1..)
            .map(|n| format!("custom-{n}"))
            .find(|id| !self.config.layouts.contains_key(id))
            .expect("an unused custom layout id always exists")
    }

    /// Overlay whose choice a workspace assignment takes.
    ///
    /// A workspace holds exactly one layout, so only one overlay can claim it:
    /// the one on the output the editor was opened from. When the compositor
    /// did not name that output — launched from the app library — the lowest
    /// connector name wins, because `HashMap` order is arbitrary and picking
    /// the "first" output would pin a different monitor's layout run to run.
    fn workspace_output(&self) -> Option<&EditorOutput> {
        let opened_on = self.opened_on.as_deref();
        self.outputs
            .values()
            .find(|output| opened_on.is_some_and(|name| name == output.name))
            .or_else(|| self.outputs.values().min_by_key(|output| &output.name))
    }

    fn save(&mut self) -> Task<Message> {
        let Some(handle) = self.config_handle.as_ref() else {
            error!("no config handle; refusing to discard edits silently");
            return Task::none();
        };

        // "Apply to workspace" means exactly one monitor's choice becomes the
        // workspace's, and that monitor's own default is deliberately left
        // alone — pinning it too would be the opposite of what the toggle says.
        // Every *other* overlay's picker is still a real choice the user made
        // about that monitor, though, so those are recorded either way; not
        // doing so is how a multi-monitor session used to lose them silently.
        let workspace_assignment = match (self.scope, self.workspace.clone()) {
            (Scope::Workspace, Some(workspace)) => self
                .workspace_output()
                .map(|output| (workspace, output.name.clone(), output.layout_id.clone())),
            _ => None,
        };
        let claimed_by_workspace = workspace_assignment
            .as_ref()
            .map(|(_, name, _)| name.clone());

        for output in self.outputs.values() {
            if claimed_by_workspace.as_deref() == Some(output.name.as_str()) {
                continue;
            }
            self.config
                .per_output
                .insert(output.output_match(), output.layout_id.clone());
        }

        if let Some((workspace, _, layout_id)) = workspace_assignment {
            self.config.per_workspace.insert(workspace, layout_id);
        }

        match handle.set(ZONES_KEY, &self.config) {
            Ok(()) => info!("saved zone configuration"),
            Err(err) => error!(?err, "failed to save zone configuration"),
        }

        self.close()
    }

    fn close(&mut self) -> Task<Message> {
        let tasks: Vec<_> = self
            .outputs
            .keys()
            .map(|id| surface::surface_task::<Message>(surface::action::destroy_layer_shell(*id)))
            .collect();
        self.outputs.clear();
        Task::batch(tasks).chain(cosmic::iced::exit())
    }

    fn open_overlay(&mut self, info: OutputInfoLite, wl_output: WlOutput) -> Task<Message> {
        let id = cosmic::iced::window::Id::unique();
        let layout_id = self
            .config
            .per_output
            .get(&OutputMatch {
                name: info.name.clone(),
                edid: None,
            })
            .cloned()
            .unwrap_or_else(|| DEFAULT_LAYOUT_ID.to_string());

        self.outputs.insert(
            id,
            EditorOutput {
                wl_output: wl_output.clone(),
                name: info.name.clone(),
                logical_size: info.logical_size,
                layout_id,
            },
        );

        surface::surface_task::<Message>(surface::action::app_layer_shell::<Self>(
            |_| surface::action::LiveSettings::default(),
            move |_| SctkLayerSurfaceSettings {
                id,
                // Overlay, not Top: the editor must cover panels and docks so
                // the whole output is editable, and it is explicitly modal.
                layer: Layer::Overlay,
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                anchor: Anchor::all(),
                // (None, None) with all anchors set means "fill the output".
                size: Some((None, None)),
                output: IcedOutput::Output(wl_output.clone()),
                namespace: "cosmic-fancy-pants-editor".into(),
                ..Default::default()
            },
            Some(Box::new(move |app: &Self| {
                overlay::view(app, id).map(cosmic::Action::App)
            })),
        ))
    }
}

impl cosmic::Application for Editor {
    type Executor = cosmic::executor::Default;
    type Flags = Launch;
    type Message = Message;

    const APP_ID: &'static str = "dev.nilfactor.CosmicFancyPantsEditor";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, launch: Self::Flags) -> (Self, Task<Self::Message>) {
        let (config_handle, config) = Self::load_config();
        (
            Self {
                core,
                workspace: launch.workspace,
                opened_on: launch.output,
                scope: Scope::Output,
                config,
                outputs: HashMap::new(),
                config_handle,
            },
            Task::none(),
        )
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        // Outputs arrive asynchronously, including ones plugged in while the
        // editor is open, so overlays are created from events rather than
        // enumerated once at startup.
        cosmic::iced::event::listen_with(|event, _, _id| match event {
            cosmic::iced::Event::PlatformSpecific(
                cosmic::iced::event::PlatformSpecific::Wayland(WaylandEvent::Output(
                    output_event,
                    wl_output,
                )),
            ) => {
                let info = match &output_event {
                    OutputEvent::Created(Some(info)) | OutputEvent::InfoUpdate(info) => {
                        OutputInfoLite {
                            name: info.name.clone().unwrap_or_default(),
                            logical_size: info
                                .logical_size
                                .map(|(w, h)| (w.max(0) as u32, h.max(0) as u32))
                                .unwrap_or_default(),
                        }
                    }
                    _ => OutputInfoLite::default(),
                };
                Some(Message::Output(Box::new(output_event), wl_output, info))
            }
            _ => None,
        })
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::Output(event, wl_output, info) => match *event {
                OutputEvent::Created(Some(_)) => {
                    if info.name.is_empty() {
                        warn!("output reported without a name; skipping overlay");
                        return Task::none();
                    }
                    if self.outputs.values().any(|o| o.wl_output == wl_output) {
                        return Task::none();
                    }
                    self.open_overlay(info, wl_output)
                }
                OutputEvent::Removed => {
                    // Matched by object identity: Removed carries no OutputInfo.
                    let removed: Vec<_> = self
                        .outputs
                        .iter()
                        .filter(|(_, o)| o.wl_output == wl_output)
                        .map(|(id, _)| *id)
                        .collect();
                    let tasks: Vec<_> = removed
                        .iter()
                        .map(|id| {
                            self.outputs.remove(id);
                            surface::surface_task::<Message>(surface::action::destroy_layer_shell(
                                *id,
                            ))
                        })
                        .collect();
                    Task::batch(tasks)
                }
                _ => Task::none(),
            },
            Message::SelectLayout(id, layout_id) => {
                if let Some(output) = self.outputs.get_mut(&id) {
                    output.layout_id = layout_id;
                }
                Task::none()
            }
            Message::ZonesEdited(id, zones) => {
                self.apply_edit(id, zones);
                Task::none()
            }
            Message::SetGap(gap) => {
                self.config.gap = gap;
                Task::none()
            }
            Message::SetScope(scope) => {
                self.scope = scope;
                Task::none()
            }
            Message::Save => self.save(),
            Message::Cancel => self.close(),
        }
    }

    /// Unused: every surface is a layer shell overlay with its own view, and
    /// the app runs with `no_main_window`.
    fn view(&self) -> cosmic::Element<'_, Self::Message> {
        cosmic::widget::text("").into()
    }
}
