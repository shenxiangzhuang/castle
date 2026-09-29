use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable, Icon, IconName, Side};
use gpui_kit::{
    Context, Focusable, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, accesskit::Role, div, prelude::FluentBuilder, px,
};

use crate::agent_config::{DEEPSEEK_PROVIDER_ID, OPENAI_PROVIDER_ID};
use crate::app::{DesktopApp, composer_model_indices};
use crate::application::{composer_status, empty_conversation_view_model};
use crate::assets::DesktopIconName;
use crate::domain::{Action, ComposerMenu, RunState};
use crate::platform::gpui::measured_container;
use crate::ui_automation::ids;
use crate::ui_theme::{metrics, palette};

impl DesktopApp {
    pub(crate) fn empty_conversation(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = palette(cx);
        let view = empty_conversation_view_model(&self.core);
        let workspace = self
            .project_store
            .project(self.core.workspace.active_project)
            .map(|project| project.name.clone())
            .unwrap_or_else(|| "Choose workspace".into());
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .items_center()
            .justify_center()
            .px_4()
            .pb(px(34.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .max_w(px(self.core.layout.composer_max_width))
                    .gap_3()
                    .children(self.continued_from_chat(cx))
                    .when(view.show_intro, |hero| {
                        hero.child(
                            div()
                                .flex()
                                .items_center()
                                .justify_center()
                                .gap_2()
                                .child(Icon::new(DesktopIconName::Castle).size_6())
                                .child(
                                    div()
                                        .text_xl()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .child("The Castle Is Out of Reach"),
                                )
                                .child(
                                    div()
                                        .px_2()
                                        .py(px(2.0))
                                        .rounded_full()
                                        .border_1()
                                        .border_color(colors.border)
                                        .bg(colors.subtle)
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child("Preview"),
                                ),
                        )
                    })
                    .when(view.show_workspace, |hero| {
                        hero.child(
                            div().flex().items_center().gap_4().child(
                                Button::new("hero-workspace")
                                    .icon(IconName::FolderOpen)
                                    .label(workspace)
                                    .ghost()
                                    .compact()
                                    .tooltip("Choose workspace")
                                    .map(|button| {
                                        self.composer_menu_trigger(
                                            ComposerMenu::Workspace,
                                            button,
                                            cx,
                                        )
                                    }),
                            ),
                        )
                    })
                    .when(view.show_composer, |hero| {
                        hero.child(self.composer_card(true, cx))
                    }),
            )
    }

    pub(crate) fn docked_composer(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let colors = palette(cx);
        let status = composer_status(&self.core);
        let history = self.core.session_view.trajectory.stats();
        let full_status = format!(
            "Session total: {status}. Selected path (including inherited history): {} input tokens, {} output tokens.",
            history.input_tokens(),
            history.total_output_tokens()
        );
        let shaped_status: SharedString = status.clone().into();
        let style = window.text_style();
        let status_width = window
            .text_system()
            .shape_line(
                shaped_status.clone(),
                window.rem_size() * 0.75,
                &[style.to_run(shaped_status.len())],
                None,
            )
            .width;
        let status_is_truncated = status_width > px(self.core.layout.composer_max_width.max(0.0));
        div()
            .flex()
            .flex_col()
            .flex_none()
            .items_center()
            .px_4()
            .pb_2()
            .gap_2()
            .child(self.composer_card(false, cx))
            .when(!status.is_empty(), |composer| {
                composer.child(
                    div()
                        .id("composer-session-stats")
                        .w_full()
                        .max_w(px(self.core.layout.composer_max_width))
                        .text_center()
                        .text_xs()
                        .truncate()
                        .text_color(colors.muted_text)
                        .child(status)
                        .when(status_is_truncated, |status| {
                            status.tooltip(move |window, cx| {
                                Tooltip::new(full_status.clone()).build(window, cx)
                            })
                        }),
                )
            })
    }

    fn composer_card(&self, hero: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = palette(cx);
        let running = self.session_running();
        let selection_pending = self.selection_pending();
        let preparing = selection_pending || matches!(self.core.run, RunState::Preparing);
        let empty = self.input.read(cx).value().trim().is_empty();
        let runtime = self.selected_runtime.read(cx);
        let archived = runtime.snapshot().session.is_archived();
        let editing = self.edit_draft.is_some();
        let unchanged = self
            .edit_draft
            .as_ref()
            .is_some_and(|edit| edit.original.trim() == self.input.read(cx).value().trim());
        let pending_inputs = runtime.pending_inputs();
        let input_action_pending = runtime.input_action_pending;
        let input_error = runtime.input_error.clone();
        let allow_all_tools = runtime.snapshot().allow_all_tools;
        let (permission_icon, permission_label, permission_description) = if allow_all_tools {
            (IconName::CircleCheck, "Allow", "Allow all tools")
        } else {
            (IconName::CircleUser, "Ask", "Ask before tools")
        };
        let model_configured = self.models[self.selected_model].model.has_api_key();
        let elapsed = self
            .selected_started_at
            .map(|started_at| started_at.elapsed())
            .unwrap_or_default();
        let configured_model = &self.models[self.selected_model];
        let model_name = if configured_model.profile.display_name.trim().is_empty() {
            &configured_model.profile.model_id
        } else {
            &configured_model.profile.display_name
        };
        let model = if model_configured {
            let name = compact_model_name(&configured_model.provider_id, model_name);
            match self.selected_reasoning_effort.as_ref() {
                Some(effort) => format!("{name} · {}", effort_label(effort)),
                None => name,
            }
        } else {
            "Configure model".into()
        };
        let model_tooltip = if model_configured {
            self.selected_reasoning_effort
                .as_ref()
                .map(|effort| format!("{} · {} reasoning", self.model, effort_label(effort)))
                .unwrap_or_else(|| self.model.clone())
        } else {
            "Configure an OpenAI or DeepSeek provider".into()
        };
        let measurement_owner = cx.entity().downgrade();
        div()
            .id(if hero {
                "hero-composer"
            } else {
                "docked-composer"
            })
            .role(Role::Form)
            .accessibility_id(ids::COMPOSER)
            .aria_label("Message composer")
            .relative()
            .flex()
            .flex_col()
            .w_full()
            .max_w(px(self.core.layout.composer_max_width))
            .rounded(px(metrics::COMPOSER_RADIUS))
            .border_1()
            .border_color(colors.border)
            .bg(colors.surface)
            .shadow_lg()
            .child(measured_container(
                measurement_owner,
                |bounds, this: &mut DesktopApp, cx| {
                    this.update_composer_measurement(bounds.height, cx)
                },
                |this: &mut DesktopApp, window, cx| this.restore_chat_tail_after_layout(window, cx),
            ))
            .when(!pending_inputs.is_empty(), |card| {
                card.child(
                    div().id("pending-messages").role(Role::Group).aria_label("Pending messages").flex().flex_col().gap_2().p_3()
                        .border_b_1().border_color(colors.border)
                        .child(div().flex().items_center().justify_between()
                            .child(div().text_xs().text_color(colors.muted_text)
                                .child(if running { "Pending messages" } else { "Pending messages · paused" }))
                            .when(!running, |header| header.child(
                                Button::new("resume-pending").label("Continue").ghost().compact()
                                    .disabled(preparing || input_action_pending || !model_configured)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.selected_runtime.update(cx, |runtime, cx| runtime.resume_pending(window, cx));
                                    })))))
                        .child(div().id("pending-message-list").flex().flex_col().gap_2()
                            .max_h(px(180.0)).overflow_y_scroll()
                            .children(pending_inputs.into_iter().enumerate().map(|(index, input)| {
                                let prioritize_id = input.input_id.clone();
                                let edit_id = input.input_id.clone();
                                let cancel_id = input.input_id;
                                let prioritized = input.origin == kcastle_agent::InputOrigin::Steer;
                                div().id(("pending-message", index)).role(Role::Group).aria_label(format!("{}: {}", if prioritized { "Waiting to join current task" } else { "Pending" }, input.input)).flex().items_center().gap_2()
                                    .child(div().flex_1().min_w(px(0.0)).text_sm().child(input.input))
                                    .when(prioritized, |row| row.child(div().text_xs().text_color(colors.muted_text)
                                        .child(if running { "Waiting to join current task" } else { "Prioritized · paused" })))
                                    .when(running && !prioritized, |row| row.child(
                                        Button::new(("prioritize-input", index)).label("Prioritize").ghost().compact()
                                            .tooltip("Join after the current response and tools finish")
                                            .disabled(preparing || input_action_pending)
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.selected_runtime.update(cx, |runtime, cx|
                                                    runtime.change_pending(prioritize_id.clone(), true, window, cx));
                                            }))))
                                    .child(Button::new(("edit-input", index)).accessibility_label("Edit pending message")
                                        .icon(crate::assets::DesktopIconName::SquarePen).ghost().compact()
                                        .tooltip(if self.input.read(cx).value().is_empty() { "Edit pending message" } else { "Send or clear your draft before editing" })
                                        .disabled(preparing || input_action_pending || self.composer_submitting || !self.input.read(cx).value().is_empty())
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.edit_pending(edit_id.clone(), window, cx);
                                        })))
                                    .child(Button::new(("cancel-input", index)).accessibility_label("Remove pending message").icon(IconName::Close).ghost().compact()
                                        .tooltip("Remove pending message").disabled(preparing || input_action_pending)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.selected_runtime.update(cx, |runtime, cx|
                                                runtime.change_pending(cancel_id.clone(), false, window, cx));
                                        })))
                            })))
                )
            })
            .when(editing, |card| card.child(div().px_3().py_2().flex().items_center().justify_between()
                .text_sm().gap_2().child(div().flex_1().min_w_0().child("Editing message · Creates a branch")
                    .child(div().text_xs().child("Workspace files are unchanged")))
                .child(Button::new("cancel-message-edit").label("Cancel").ghost().compact()
                    .disabled(self.composer_submitting)
                    .on_click(cx.listener(|this, _, window, cx| this.cancel_edit(window, cx))))))
            .when_some(input_error, |card, error| card.child(div().px_3().py_2().text_sm().child(error)))
            .child(
                div()
                    .id(if hero {
                        "hero-composer-input"
                    } else {
                        "docked-composer-input"
                    })
                    .role(Role::Group)
                    .accessibility_id(ids::COMPOSER_INPUT)
                    .aria_label("Message the agent")
                    .capture_key_down(cx.listener(|this, event, window, cx| {
                        this.handle_root_key(event, window, cx)
                    }))
                    .child(
                        Textarea::new(&self.input)
                            .disabled(editing && self.composer_submitting)
                            .aria_label("Message the agent")
                            .appearance(false)
                            .bordered(false)
                            .text_base(),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .flex_wrap()
                    .gap_3()
                    .px_2()
                    .pb(px(metrics::COMPOSER_CONTROLS_BOTTOM_INSET))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .when(!hero, |controls| {
                                controls.child(
                                    Button::new("commands")
                                        .accessibility_id(ids::COMPOSER_COMMANDS)
                                        .icon(IconName::Plus)
                                        .ghost()
                                        .compact()
                                        .disabled(selection_pending)
                                        .tooltip("Commands")
                                        .on_key_down(cx.listener(|this, event, window, cx| {
                                            this.handle_root_key(event, window, cx)
                                        }))
                                        .map(|button| {
                                            self.composer_menu_trigger(
                                                ComposerMenu::Commands,
                                                button,
                                                cx,
                                            )
                                        }),
                                )
                            })
                            .child(
                                Button::new(if hero {
                                    "hero-access-settings"
                                } else {
                                    "access-settings"
                                })
                                .accessibility_id(ids::COMPOSER_PERMISSION)
                                .icon(permission_icon)
                                .label(permission_label)
                                .accessibility_label(permission_description)
                                .ghost()
                                .compact()
                                .disabled(selection_pending)
                                .tooltip(permission_description)
                                .on_key_down(cx.listener(|this, event, window, cx| {
                                    this.handle_root_key(event, window, cx)
                                }))
                                .map(|button| {
                                    self.composer_menu_trigger(ComposerMenu::Permission, button, cx)
                                }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .min_w(px(0.0))
                            .gap_2()
                            .child(
                                Button::new(if hero {
                                    "hero-model-settings"
                                } else {
                                    "model-settings"
                                })
                                .accessibility_id(ids::COMPOSER_MODEL)
                                .accessibility_label(model_tooltip.clone())
                                .icon(provider_icon(&configured_model.provider_id))
                                .label(model)
                                .ghost()
                                .compact()
                                .disabled(running || selection_pending)
                                .on_key_down(cx.listener(|this, event, window, cx| {
                                    this.handle_root_key(event, window, cx)
                                }))
                                .map(|button| {
                                    self.composer_menu_trigger(ComposerMenu::Model, button, cx)
                                }),
                            )
                            .children(running.then(|| {
                                div()
                                    .text_xs()
                                    .text_color(colors.muted_text)
                                    .child(format_duration(elapsed))
                            }))
                            .child(if running && empty {
                                Button::new("stop")
                                    .when(cfg!(test), |button| button.debug_selector(|| "stop".into()))
                                    .accessibility_id(ids::COMPOSER_STOP)
                                    .icon(IconName::Close)
                                    .rounded(px(999.0))
                                    .tooltip("Stop")
                                    .on_click(cx.listener(|this, _, _, cx| this.abort(cx)))
                                    .into_any_element()
                            } else {
                                Button::new("send")
                                    .when(cfg!(test), |button| button.debug_selector(|| "send".into()))
                                    .accessibility_id(ids::COMPOSER_SEND)
                                    .role(Role::DefaultButton)
                                    .icon(IconName::ArrowUp)
                                    .primary()
                                    .loading(preparing || self.composer_submitting)
                                    .disabled(empty || preparing || self.composer_submitting || !model_configured || archived || unchanged)
                                    .rounded(px(999.0))
                                    .tooltip(if editing { "Save & resend" } else if running { "Send after the current task" } else { "Send message" })
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.submit(window, cx)),
                                    )
                                    .into_any_element()
                            }),
                    ),
            )
    }

    pub(crate) fn composer_menu_trigger(
        &self,
        kind: ComposerMenu,
        button: Button,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let owner = cx.entity().downgrade();
        Popover::new(("composer-popup", kind as usize))
            .anchor(if kind == ComposerMenu::Model {
                gpui_kit::Anchor::BottomCenter
            } else {
                gpui_kit::Anchor::BottomLeft
            })
            .appearance(false)
            .trigger(button)
            .open(self.core.composer.menu == Some(kind))
            .when_some(self.composer_popup.as_ref(), |popover, popup| {
                popover.track_focus(&popup.focus_handle(cx))
            })
            .on_open_change(cx.listener(move |this, open, window, cx| {
                if *open {
                    this.open_composer_menu(kind, window, cx);
                } else if this.core.composer.menu == Some(kind) {
                    this.dispatch(Action::SetComposerMenu(None), window, cx);
                    this.composer_popup = None;
                }
            }))
            .content(move |_, _, cx| {
                div()
                    .id("composer-menu")
                    .accessibility_id(ids::COMPOSER_MENU)
                    .children(
                        owner
                            .upgrade()
                            .and_then(|owner| owner.read(cx).composer_popup.clone()),
                    )
            })
            .into_any_element()
    }

    pub(crate) fn open_composer_menu(
        &mut self,
        kind: ComposerMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if kind == ComposerMenu::Model && !self.models[self.selected_model].model.has_api_key() {
            self.open_model_settings_dialog(window, cx);
            return;
        }
        self.dispatch(Action::SetComposerMenu(Some(kind)), window, cx);
        let owner = cx.entity().downgrade();
        // Menu builders read the app; run after the opening update releases its borrow.
        window.defer(cx, move |window, cx| {
            let Some(app) = owner.upgrade() else {
                return;
            };
            if app.read(cx).core.composer.menu != Some(kind) {
                return;
            }
            let popup = PopupMenu::build(window, cx, move |menu, window, cx| {
                composer_popup_menu(owner, kind, menu, window, cx)
            });
            app.update(cx, |this, cx| {
                cx.subscribe_in(
                    &popup,
                    window,
                    |this, _, _: &gpui_kit::DismissEvent, window, cx| {
                        this.dispatch(Action::SetComposerMenu(None), window, cx);
                        // Do not steal focus from an action that just opened a dialog.
                        if this.modal.is_none() {
                            this.input.update(cx, |input, cx| input.focus(window, cx));
                        }
                    },
                )
                .detach();
                popup.focus_handle(cx).focus(window, cx);
                this.composer_popup = Some(popup);
                cx.notify();
            });
        });
    }

    pub(crate) fn approval_card(&self, cx: &mut Context<Self>) -> Option<gpui_kit::AnyElement> {
        let colors = palette(cx);
        self.core.approval.as_ref().map(|approval| {
            let allow_id = approval.call_id.clone();
            let deny_id = approval.call_id.clone();
            div()
                .flex()
                .justify_center()
                .px_4()
                .pb_2()
                .child(
                    div()
                        .id("approval-card")
                        .role(Role::AlertDialog)
                        .accessibility_id(ids::APPROVAL)
                        .aria_label(format!("Allow {}?", approval.name))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_4()
                        .w_full()
                        .max_w(px(self.core.layout.composer_max_width))
                        .p_4()
                        .rounded_xl()
                        .border_1()
                        .border_color(colors.border)
                        .bg(colors.surface)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w(px(0.0))
                                .gap_1()
                                .child(
                                    div()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .child(format!("Allow {}?", approval.name)),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .font_family("SF Mono")
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child(approval.arguments.clone()),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_none()
                                .gap_2()
                                .child(
                                    Button::new("deny-tool")
                                        .accessibility_id(ids::APPROVAL_DENY)
                                        .label("Deny")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.decide(deny_id.clone(), false, cx)
                                        })),
                                )
                                .child(
                                    Button::new("allow-tool")
                                        .accessibility_id(ids::APPROVAL_ALLOW)
                                        .label("Allow")
                                        .primary()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.decide(allow_id.clone(), true, cx)
                                        })),
                                ),
                        ),
                )
                .into_any_element()
        })
    }
}

fn composer_popup_menu(
    owner: gpui_kit::WeakEntity<DesktopApp>,
    kind: ComposerMenu,
    mut menu: PopupMenu,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    menu = menu
        .min_w(px(if kind == ComposerMenu::Model {
            180.0
        } else {
            240.0
        }))
        .max_h(px(360.0))
        .scrollable(true);
    let Some(app) = owner.upgrade() else {
        return menu;
    };
    match kind {
        ComposerMenu::Commands => {
            let export = owner.clone();
            let permission = owner.clone();
            menu = menu
                .item(
                    PopupMenuItem::new("Export session")
                        .icon(IconName::ArrowDown)
                        .on_click(move |_, window, cx| {
                            let _ =
                                export.update(cx, |this, cx| this.export_session_log(window, cx));
                        }),
                )
                .submenu("Permission", window, cx, move |menu, window, cx| {
                    composer_popup_menu(
                        permission.clone(),
                        ComposerMenu::Permission,
                        menu,
                        window,
                        cx,
                    )
                });
            if composer_model_indices(&app.read(cx).models)
                .next()
                .is_some()
            {
                menu.submenu("Model", window, cx, move |menu, window, cx| {
                    composer_popup_menu(owner.clone(), ComposerMenu::Model, menu, window, cx)
                })
            } else {
                menu.item(
                    PopupMenuItem::new("Configure model").on_click(move |_, window, cx| {
                        let _ = owner
                            .update(cx, |this, cx| this.open_model_settings_dialog(window, cx));
                    }),
                )
            }
        }
        ComposerMenu::Permission => {
            let allow = app
                .read(cx)
                .selected_runtime
                .read(cx)
                .snapshot()
                .allow_all_tools;
            for (value, label) in [(false, "Ask before tools"), (true, "Allow all tools")] {
                let owner = owner.clone();
                menu = menu.item(PopupMenuItem::new(label).checked(allow == value).on_click(
                    move |_, _, cx| {
                        let _ = owner.update(cx, |this, cx| this.set_allow_all_tools(value, cx));
                    },
                ));
            }
            menu
        }
        ComposerMenu::Model => {
            let app = app.read(cx);
            menu = menu.check_side(Side::Right).item(
                PopupMenuItem::element(|_, _| div().ml_neg_4().child("Model")).disabled(true),
            );
            for index in composer_model_indices(&app.models) {
                let owner = owner.clone();
                let configured = &app.models[index];
                let model_name = if configured.profile.display_name.trim().is_empty() {
                    &configured.profile.model_id
                } else {
                    &configured.profile.display_name
                };
                menu = menu.item(
                    PopupMenuItem::new(compact_model_name(&configured.provider_id, model_name))
                        .icon(provider_icon(&configured.provider_id))
                        .checked(index == app.selected_model)
                        .on_click(move |_, _, cx| {
                            let _ = owner.update(cx, |this, cx| this.select_model(index, cx));
                        }),
                );
            }
            menu = menu.separator().item(
                PopupMenuItem::element(|_, _| div().ml_neg_4().child("Reasoning")).disabled(true),
            );
            for effort in app.models[app.selected_model].model.reasoning_efforts() {
                let owner = owner.clone();
                let effort = *effort;
                menu = menu.item(
                    PopupMenuItem::new(effort_label(&effort))
                        .icon(IconName::Asterisk)
                        .checked(app.selected_reasoning_effort == Some(effort))
                        .on_click(move |_, _, cx| {
                            let _ =
                                owner.update(cx, |this, cx| this.set_reasoning_effort(effort, cx));
                        }),
                );
            }
            menu
        }
        ComposerMenu::Workspace => {
            let app = app.read(cx);
            for (index, project) in app.project_store.projects().iter().enumerate() {
                let owner = owner.clone();
                menu = menu.item(
                    PopupMenuItem::new(project.name.clone())
                        .checked(index == app.core.workspace.active_project)
                        .on_click(move |_, window, cx| {
                            let _ =
                                owner.update(cx, |this, cx| this.switch_project(index, window, cx));
                        }),
                );
            }
            menu.separator().item(
                PopupMenuItem::new("Add workspace")
                    .icon(IconName::Plus)
                    .on_click(move |_, window, cx| {
                        let _ = owner.update(cx, |this, cx| this.add_project(window, cx));
                    }),
            )
        }
    }
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds >= 60 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

fn effort_label(effort: &kcastle_agent::ReasoningEffort) -> &'static str {
    match effort {
        kcastle_agent::ReasoningEffort::None => "Off",
        kcastle_agent::ReasoningEffort::Minimal => "Minimal",
        kcastle_agent::ReasoningEffort::Low => "Low",
        kcastle_agent::ReasoningEffort::Medium => "Medium",
        kcastle_agent::ReasoningEffort::High => "High",
        kcastle_agent::ReasoningEffort::Xhigh => "XHigh",
    }
}

fn compact_model_name(provider_id: &str, name: &str) -> String {
    match provider_id {
        DEEPSEEK_PROVIDER_ID | "deepseek" => name
            .strip_prefix("DeepSeek-")
            .unwrap_or(name)
            .replace('-', " "),
        OPENAI_PROVIDER_ID => name.strip_prefix("GPT-").unwrap_or(name).to_owned(),
        _ => name.to_owned(),
    }
}

fn provider_icon(provider_id: &str) -> Icon {
    match provider_id {
        DEEPSEEK_PROVIDER_ID | "deepseek" => Icon::new(DesktopIconName::DeepSeek),
        OPENAI_PROVIDER_ID => Icon::new(DesktopIconName::OpenAi),
        _ => Icon::new(IconName::Bot),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composer_model_label_drops_redundant_provider_copy() {
        assert_eq!(
            compact_model_name(DEEPSEEK_PROVIDER_ID, "DeepSeek-V4-Flash"),
            "V4 Flash"
        );
        assert_eq!(
            compact_model_name(OPENAI_PROVIDER_ID, "GPT-5.6 Sol"),
            "5.6 Sol"
        );
        assert_eq!(effort_label(&kcastle_agent::ReasoningEffort::High), "High");
    }
}
