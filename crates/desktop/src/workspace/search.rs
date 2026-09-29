//! Session search overlay, results and keyboard selection.
use std::path::PathBuf;

use gpui_kit::component::Sizable;
use gpui_kit::component::input::Input;
use gpui_kit::{
    App, Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, Window, accesskit::Role, div, prelude::FluentBuilder, px,
};

use crate::app::Action;
use crate::app::{DesktopApp, same_path, session_age};
use crate::rendering::automation::ids;
use crate::rendering::theme::UiPalette;

use crate::app::Modal;

#[derive(Clone)]
struct SessionSearchResult {
    project_index: usize,
    project_name: String,
    path: PathBuf,
    title: String,
    summary: Option<String>,
    updated_at: u64,
}

const SESSION_SEARCH_RESULT_LIMIT: usize = 8;

pub(crate) fn session_search_dialog_view(
    app: &DesktopApp,
    cx: &mut Context<DesktopApp>,
    colors: UiPalette,
) -> gpui_kit::AnyElement {
    let selected = match app.modal {
        Some(Modal::SessionSearch { selected }) => selected,
        _ => 0,
    };
    let results = app.session_search_results(cx);
    let empty = results.is_empty();
    let selected = selected.min(results.len().saturating_sub(1));

    div()
        .id("session-search-dialog")
        .when(cfg!(test), |element| {
            element.debug_selector(|| "session-search-dialog".into())
        })
        .flex()
        .flex_col()
        .w(px(560.0))
        .max_w_full()
        .rounded_xl()
        .border_1()
        .border_color(colors.border)
        .bg(colors.surface)
        .shadow_xl()
        .overflow_hidden()
        .child(
            div().p_3().border_b_1().border_color(colors.border).child(
                Input::new(&app.session_search)
                    .accessibility_id(ids::SESSION_SEARCH_INPUT)
                    .aria_label("Search sessions")
                    .large()
                    .cleanable(true),
            ),
        )
        .child(
            div()
                .id("session-search-results")
                .role(Role::List)
                .aria_label("Session search results")
                .flex()
                .flex_col()
                .p_2()
                .children(results.into_iter().enumerate().map(|(index, result)| {
                    let open_path = result.path.clone();
                    let keyboard_path = result.path.clone();
                    let project_index = result.project_index;
                    let is_selected = index == selected;
                    let age = session_age(result.updated_at);
                    div()
                        .id(("session-search-result", index))
                        .role(Role::ListItem)
                        .aria_label(format!("{} — {}", result.title, result.project_name))
                        .aria_selected(is_selected)
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .min_h(px(52.0))
                        .px_3()
                        .py_2()
                        .rounded_lg()
                        .cursor_pointer()
                        .tab_index(0)
                        .when(is_selected, |row| row.bg(colors.selected))
                        .hover(move |row| row.bg(colors.hover))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_session_search_result(
                                project_index,
                                open_path.clone(),
                                window,
                                cx,
                            )
                        }))
                        .on_key_down(cx.listener(
                            move |this, event: &gpui_kit::KeyDownEvent, window, cx| {
                                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                    this.open_session_search_result(
                                        project_index,
                                        keyboard_path.clone(),
                                        window,
                                        cx,
                                    );
                                }
                            },
                        ))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w(px(0.0))
                                .gap_1()
                                .child(div().truncate().text_sm().child(result.title))
                                .child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child(match result.summary {
                                            Some(summary) => {
                                                format!("{} · {summary}", result.project_name)
                                            }
                                            None => result.project_name,
                                        }),
                                ),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(colors.muted_text)
                                .child(age),
                        )
                }))
                .children(empty.then(|| {
                    div()
                        .h(px(96.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .text_color(colors.muted_text)
                        .child("No sessions found")
                })),
        )
        .child(
            div()
                .flex()
                .justify_end()
                .gap_3()
                .border_t_1()
                .border_color(colors.border)
                .px_4()
                .py_2()
                .text_xs()
                .text_color(colors.muted_text)
                .child("↑↓ Navigate")
                .child("↵ Open")
                .child("Esc Close"),
        )
        .into_any_element()
}

impl DesktopApp {
    fn session_search_results(&self, cx: &App) -> Vec<SessionSearchResult> {
        let query = self.session_search.read(cx).value().trim().to_lowercase();
        let mut results = self
            .project_store
            .projects()
            .iter()
            .enumerate()
            .flat_map(|(project_index, project)| {
                self.project_sessions
                    .get(&project.sessions_dir)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(move |session| (project_index, project.name.clone(), session))
            })
            .filter_map(|(project_index, project_name, session)| {
                let selected = project_index == self.core.workspace.active_project
                    && same_path(&session.path, &self.core.session.current);
                let title = if selected && self.core.session_view.conversation.title != "New chat" {
                    self.core.session_view.conversation.title.clone()
                } else if session.title == "Untitled session" {
                    "New Session".into()
                } else {
                    session.title.clone()
                };
                let title_matches = title.to_lowercase().contains(&query);
                let content_matches = self.session_document_matches(&session.path, &query);
                if !query.is_empty() && !title_matches && !content_matches {
                    return None;
                }
                let summary = (!query.is_empty() && !title_matches)
                    .then(|| self.session_document_summary(&session.path, &query))
                    .flatten();
                Some(SessionSearchResult {
                    project_index,
                    project_name,
                    path: session.path,
                    title,
                    summary,
                    updated_at: session.updated_at,
                })
            })
            .collect::<Vec<_>>();
        results.sort_by_key(|result| std::cmp::Reverse(result.updated_at));
        // ponytail: keep the palette keyboard-sized; paginate only if users cannot narrow queries.
        results.truncate(SESSION_SEARCH_RESULT_LIMIT);
        results
    }

    pub(crate) fn open_session_search_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dispatch(Action::CloseTransientOverlays, window, cx);
        self.session_search
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.modal = Some(Modal::SessionSearch { selected: 0 });
        self.session_search
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    pub(crate) fn move_session_search_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let result_count = self.session_search_results(cx).len();
        let Some(Modal::SessionSearch { selected }) = &mut self.modal else {
            return;
        };
        *selected = selected
            .saturating_add_signed(delta)
            .min(result_count.saturating_sub(1));
        cx.notify();
    }

    pub(crate) fn open_selected_session_search_result(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(Modal::SessionSearch { selected }) = self.modal else {
            return;
        };
        let Some(result) = self.session_search_results(cx).get(selected).cloned() else {
            return;
        };
        self.open_session_search_result(result.project_index, result.path, window, cx);
    }

    fn open_session_search_result(
        &mut self,
        project_index: usize,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modal = None;
        self.session_search
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.open_project_session(project_index, path, window, cx);
        self.input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }
}
