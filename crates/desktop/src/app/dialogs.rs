use crate::settings::{SettingsDialog, SettingsPage, settings_dialog_view};
use crate::workspace::session_search_dialog_view;
use std::path::PathBuf;

use gpui_kit::component::Sizable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::{
    AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};
use harness::SessionInfo;

use crate::app::DesktopApp;
use crate::rendering::automation::ids;
use crate::rendering::theme::{UiPalette, palette};

pub(crate) enum Modal {
    SessionSearch {
        selected: usize,
    },
    RenameSession {
        project_index: usize,
        path: PathBuf,
        input: Entity<InputState>,
    },
    DeleteArchivedSession {
        project_index: usize,
        session: SessionInfo,
    },
    RemoveProject(usize),
    Settings(Box<SettingsDialog>),
}

impl DesktopApp {
    pub(crate) fn open_target_rename_session_dialog(
        &mut self,
        project_index: usize,
        path: PathBuf,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if path.as_os_str().is_empty() || self.session_is_active(project_index, &path, cx) {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).default_value(title));
        self.modal = Some(Modal::RenameSession {
            project_index,
            path,
            input: input.clone(),
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    pub(crate) fn open_delete_archived_session_dialog(
        &mut self,
        project_index: usize,
        session: SessionInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modal = Some(Modal::DeleteArchivedSession {
            project_index,
            session,
        });
        self.modal_focus.focus(window, cx);
        cx.notify();
    }

    pub(crate) fn open_remove_project_dialog(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.project_store.project(index).is_some() {
            self.modal = Some(Modal::RemoveProject(index));
            self.modal_focus.focus(window, cx);
            cx.notify();
        }
    }

    pub(crate) fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let was_session_search = matches!(self.modal, Some(Modal::SessionSearch { .. }));
        self.modal = None;
        if was_session_search {
            self.session_search
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    pub(crate) fn confirm_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::RenameSession {
            project_index,
            path,
            input,
        }) = &self.modal
        else {
            return;
        };
        let project_index = *project_index;
        let path = path.clone();
        let title = input.read(cx).value().trim().to_owned();
        if title.is_empty() {
            return;
        }
        self.modal = None;
        self.rename_target_session(project_index, path, title, window, cx);
    }

    pub(crate) fn modal_view(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let colors = palette(cx);
        let is_session_search = matches!(self.modal, Some(Modal::SessionSearch { .. }));
        let dialog_label = match &self.modal {
            Some(Modal::SessionSearch { .. }) => "Search sessions",
            Some(Modal::RenameSession { .. }) => "Rename session",
            Some(Modal::DeleteArchivedSession { .. }) => "Delete session",
            Some(Modal::RemoveProject(_)) => "Remove project",
            Some(Modal::Settings(_)) => "Settings",
            None => return None,
        };
        let content = match &self.modal {
            Some(Modal::SessionSearch { .. }) => session_search_dialog_view(self, cx, colors),
            Some(Modal::RenameSession { input, .. }) => modal_card("Rename session", colors)
                .child(
                    div()
                        .text_sm()
                        .text_color(colors.muted_text)
                        .child("Use a short title that will be easy to find later."),
                )
                .child(
                    Input::new(input)
                        .accessibility_id(ids::DIALOG_PRIMARY_INPUT)
                        .aria_label("Session title")
                        .large(),
                )
                .child(
                    modal_actions()
                        .child(Button::new("cancel-rename").label("Cancel").on_click(
                            cx.listener(|this, _, window, cx| this.close_modal(window, cx)),
                        ))
                        .child(
                            Button::new("confirm-rename")
                                .label("Rename")
                                .primary()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.confirm_rename(window, cx)
                                })),
                        ),
                )
                .into_any_element(),
            Some(Modal::DeleteArchivedSession {
                project_index,
                session,
            }) => {
                let project_index = *project_index;
                let session = session.clone();
                let title = session.title.clone();
                modal_card("Delete session?", colors)
                    .child(format!("“{title}” will be permanently deleted."))
                    .child(
                        div()
                            .text_sm()
                            .text_color(colors.muted_text)
                            .child("This cannot be undone."),
                    )
                    .child(
                        modal_actions()
                            .child(
                                Button::new("cancel-delete-session")
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.modal = Some(Modal::Settings(Box::new(
                                            SettingsDialog::new(SettingsPage::Archives),
                                        )));
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("confirm-delete-session")
                                    .label("Delete")
                                    .danger()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.modal = Some(Modal::Settings(Box::new(
                                            SettingsDialog::new(SettingsPage::Archives),
                                        )));
                                        this.delete_archived_session(
                                            project_index,
                                            session.clone(),
                                            cx,
                                        );
                                    })),
                            ),
                    )
                    .into_any_element()
            }
            Some(Modal::RemoveProject(index)) => {
                let index = *index;
                let name = self
                    .project_store
                    .project(index)
                    .map(|project| project.name.clone())
                    .unwrap_or_default();
                modal_card("Remove project?", colors)
                    .child(format!("Remove “{name}” from {}?", crate::APP_NAME))
                    .child(
                        div()
                            .text_sm()
                            .text_color(colors.muted_text)
                            .child("The project folder and its session history stay on disk."),
                    )
                    .child(
                        modal_actions()
                            .child(
                                Button::new("cancel-remove-project")
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.close_modal(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("confirm-remove-project")
                                    .label("Remove")
                                    .danger()
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.modal = None;
                                        this.remove_project(index, window, cx)
                                    })),
                            ),
                    )
                    .into_any_element()
            }
            Some(Modal::Settings(dialog)) => settings_dialog_view(self, dialog, cx, colors),
            None => return None,
        };

        let owner = cx.entity().downgrade();
        Some(
            gpui_kit::base::Dialog::new(cx)
                .focus_handle(self.modal_focus.clone())
                .backdrop(div().absolute().inset_0().bg(colors.overlay))
                .on_ok(move |_, window, cx| {
                    let _ = owner.update(cx, |this, cx| match this.modal {
                        Some(Modal::SessionSearch { .. }) => {
                            this.open_selected_session_search_result(window, cx)
                        }
                        _ => this.confirm_rename(window, cx),
                    });
                    false
                })
                .on_close(cx.listener(|this, _, window, cx| this.close_modal(window, cx)))
                .when(is_session_search, |dialog| {
                    dialog.items_start().pt(px(96.0))
                })
                .popup(
                    div()
                        .id("modal-content")
                        .when(cfg!(test), |element| {
                            element.debug_selector(|| "modal-content".into())
                        })
                        .role(gpui_kit::accesskit::Role::Dialog)
                        .occlude()
                        .accessibility_id(ids::DIALOG)
                        .aria_label(dialog_label)
                        .max_w_full()
                        .max_h_full()
                        .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation()
                        })
                        .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                            if !matches!(this.modal, Some(Modal::SessionSearch { .. })) {
                                return;
                            }
                            match event.keystroke.key.as_str() {
                                "up" => this.move_session_search_selection(-1, cx),
                                "down" => this.move_session_search_selection(1, cx),
                                _ => return,
                            }
                            cx.stop_propagation();
                        }))
                        .child(content),
                )
                .into_any_element(),
        )
    }
}

fn modal_card(title: &'static str, colors: UiPalette) -> gpui_kit::Div {
    div()
        .flex()
        .flex_col()
        .gap_4()
        .w(px(480.0))
        .p_6()
        .rounded_xl()
        .border_1()
        .border_color(colors.border)
        .bg(colors.surface)
        .shadow_xl()
        .child(
            div()
                .text_lg()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(title),
        )
}

fn modal_actions() -> gpui_kit::Div {
    div().flex().items_center().justify_end().gap_2().pt_2()
}
