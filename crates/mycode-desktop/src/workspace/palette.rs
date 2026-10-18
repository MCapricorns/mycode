//! Opens, closes, and runs the global command palette.
//!
//! The palette is a layer over the desk. Choosing a row uses the same
//! handlers as the sidebar, the model picker, the composer `/` menu, and the
//! settings navigation.

use gpui_kit::component::command::CommandState;
use gpui_kit::{AppContext as _, Context, Entity, Focusable as _, Window};

use crate::view_model::{FrontendAction, MainView, PaletteCommand, palette_catalog, palette_item};
use crate::workspace::Workspace;

impl Workspace {
    pub(crate) fn command_palette_entity(&self) -> Option<Entity<CommandState>> {
        self.command_palette.clone()
    }

    /// Whether this frame should move focus into the palette search field.
    pub(crate) fn take_palette_focus(&mut self) -> bool {
        let pending = self.palette_focus_pending && self.vm().command_palette_open;
        if pending {
            self.palette_focus_pending = false;
        }
        pending
    }

    pub(crate) fn toggle_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_command_palette(!self.vm().command_palette_open, window, cx);
    }

    pub(crate) fn set_command_palette(
        &mut self,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.vm().command_palette_open == open {
            return;
        }
        if open {
            self.project_picker = None;
            let palette = self.ensure_command_palette(window, cx);
            palette.update(cx, |state, cx| state.set_query("", window, cx));
            self.palette_focus_pending = true;
        } else {
            self.palette_focus_pending = false;
        }
        self.apply_action(FrontendAction::CommandPaletteSet(open), cx);
        if !open {
            self.restore_focus_after_palette(window, cx);
        }
    }

    /// Runs the row at the catalog coordinate the command widget confirmed.
    pub(crate) fn activate_palette(
        &mut self,
        section: usize,
        row: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let catalog = palette_catalog(self.vm());
        let command = palette_item(&catalog, section, row).map(|item| item.command.clone());
        if let Some(command) = command {
            self.run_palette_command(command, cx);
        }
        self.set_command_palette(false, window, cx);
    }

    fn ensure_command_palette(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<CommandState> {
        if let Some(palette) = self.command_palette.clone() {
            return palette;
        }
        let palette = cx.new(|cx| CommandState::new(window, cx));
        self.command_palette = Some(palette.clone());
        palette
    }

    fn run_palette_command(&mut self, command: PaletteCommand, cx: &mut Context<Self>) {
        match command {
            PaletteCommand::NewSession => {
                self.on_show_main_view(MainView::Chat, cx);
                if !crate::view_model::has_open_folder(self.vm()) {
                    self.push_toast(
                        crate::i18n::t("Open a folder to get started.", "打开一个目录即可开始。"),
                        crate::workspace::ToastKind::Info,
                        cx,
                    );
                } else {
                    self.on_new_session(cx);
                }
            }
            PaletteCommand::OpenSession(session_id) => {
                self.on_show_main_view(MainView::Chat, cx);
                self.on_open_session(&session_id, cx);
            }
            PaletteCommand::SelectModel { provider, model } => {
                self.on_select_model_on(&provider, &model, cx);
            }
            PaletteCommand::Slash(insert) => {
                if insert != "/settings" {
                    self.on_show_main_view(MainView::Chat, cx);
                }
                self.run_slash_command(&insert, cx);
            }
            PaletteCommand::OpenSettings(section) => {
                self.on_show_main_view(MainView::Settings, cx);
                self.on_show_settings_section(section, cx);
            }
        }
    }

    fn restore_focus_after_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.vm().view == MainView::Chat {
            self.composer.read(cx).focus_handle(cx).focus(window, cx);
        } else {
            self.focus_handle.focus(window, cx);
        }
    }
}
