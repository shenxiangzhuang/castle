use super::*;
use crate::assets::DesktopIconName;
use gpui_kit::component::{
    Disableable, Icon, Sizable,
    button::{Button, ButtonCustomVariant, ButtonVariants},
    hover_card::HoverCard,
    popover::Popover,
};
use gpui_kit::{
    AnyElement, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, WeakEntity, div, prelude::FluentBuilder,
};
use std::sync::Arc;

#[derive(Clone)]
struct Relation {
    id: String,
    title: String,
    path: Option<PathBuf>,
    age: Option<String>,
    archived: bool,
}

// Both directions are lists; rendering and navigation do not assume a single parent.
#[derive(Default)]
struct Relations {
    parents: Vec<Relation>,
    children: Vec<Relation>,
}

impl Relations {
    fn icon(&self) -> Option<(DesktopIconName, &'static str)> {
        match (self.parents.is_empty(), self.children.is_empty()) {
            (true, true) => None,
            (false, true) => Some((DesktopIconName::SessionParents, "Parent sessions")),
            (true, false) => Some((DesktopIconName::SessionChildren, "Child sessions")),
            (false, false) => Some((
                DesktopIconName::SessionRelations,
                "Parent and child sessions",
            )),
        }
    }

    fn render(&self, owner: &WeakEntity<DesktopApp>, cx: &gpui_kit::App) -> AnyElement {
        let colors = crate::ui_theme::palette(cx);
        div()
            .id("session-relations-list")
            .when(cfg!(test), |list| {
                list.debug_selector(|| "session-relations-list".into())
            })
            .flex()
            .flex_col()
            .w(px(184.0))
            .p(px(4.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(colors.border)
            .bg(colors.surface)
            .shadow_sm()
            .font_weight(gpui_kit::FontWeight::NORMAL)
            .max_h(px(320.0))
            .overflow_y_scroll()
            .children(
                [("Parents", &self.parents), ("Children", &self.children)]
                    .into_iter()
                    .filter(|(_, rows)| !rows.is_empty())
                    .map(|(label, rows)| {
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .px(px(8.0))
                                    .pt(px(4.0))
                                    .pb(px(2.0))
                                    .text_size(px(11.0))
                                    .line_height(px(16.0))
                                    .text_color(colors.muted_text)
                                    .child(label),
                            )
                            .children(rows.iter().map(|relation| {
                                let owner = owner.clone();
                                let path = relation.path.clone();
                                let title = if path.is_some() {
                                    relation.title.clone()
                                } else {
                                    format!("{} · unavailable", relation.title)
                                };
                                Button::new(SharedString::from(format!(
                                    "relation-{label}-{}",
                                    relation.id
                                )))
                                .when(cfg!(test), |button| {
                                    let id = format!("relation-{label}-{}", relation.id);
                                    button.debug_selector(move || id.clone())
                                })
                                .accessibility_label(if relation.archived {
                                    format!("{title} · Archived")
                                } else {
                                    title.clone()
                                })
                                .when(relation.archived, |button| {
                                    button.child(
                                        Icon::new(DesktopIconName::Archive)
                                            .size(px(12.0))
                                            .text_color(colors.muted_text),
                                    )
                                })
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .truncate()
                                        .text_left()
                                        .font_weight(gpui_kit::FontWeight::NORMAL)
                                        .text_size(px(13.0))
                                        .when(cfg!(test), |text| {
                                            let id =
                                                format!("relation-title-{label}-{}", relation.id);
                                            text.debug_selector(move || id.clone())
                                        })
                                        .child(title),
                                )
                                .when_some(relation.age.as_ref(), |button, age| {
                                    button.child(
                                        div()
                                            .flex_none()
                                            .text_size(px(11.0))
                                            .font_weight(gpui_kit::FontWeight::NORMAL)
                                            .text_color(colors.muted_text)
                                            .when(cfg!(test), |text| {
                                                let id =
                                                    format!("relation-age-{label}-{}", relation.id);
                                                text.debug_selector(move || id.clone())
                                            })
                                            .child(age.clone()),
                                    )
                                })
                                .custom(
                                    ButtonCustomVariant::new(cx)
                                        .foreground(if relation.archived {
                                            colors.muted_text
                                        } else {
                                            colors.text
                                        })
                                        .hover(colors.text.opacity(0.05))
                                        .active(colors.text.opacity(0.08)),
                                )
                                .small()
                                .h(px(28.0))
                                .px(px(8.0))
                                .rounded(px(4.0))
                                .w_full()
                                .disabled(path.is_none())
                                .on_click(
                                    move |_, window, cx| {
                                        if let (Some(owner), Some(path)) =
                                            (owner.upgrade(), path.as_ref())
                                        {
                                            owner.update(cx, |app, cx| {
                                                app.relations_open = false;
                                                app.relations_hovered = false;
                                                app.open_session(path.clone(), window, cx);
                                                cx.notify();
                                            });
                                        }
                                    },
                                )
                            }))
                    }),
            )
            .into_any_element()
    }
}

impl DesktopApp {
    pub(crate) fn session_relation_info(&self, id: &SessionId) -> Option<&SessionInfo> {
        self.project_sessions
            .values()
            .chain(self.project_archived_sessions.values())
            .flatten()
            .find(|info| &info.id == id)
    }

    pub(crate) fn session_relations_icon(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let runtime = self.selected_runtime.read(cx);
        let relations = Relations {
            parents: runtime
                .tree()
                .origin
                .iter()
                .map(|origin| {
                    let info = self.session_relation_info(&origin.session_id);
                    Relation {
                        id: origin.session_id.to_string(),
                        title: info.map_or_else(|| origin.title.clone(), |info| info.title.clone()),
                        path: info.map(|info| info.path.clone()),
                        age: info.map(|info| session_age(self.session_modified_at(info))),
                        archived: info.is_some_and(SessionInfo::is_archived),
                    }
                })
                .collect(),
            children: runtime
                .fork_children
                .iter()
                .map(|(info, _)| Relation {
                    id: info.id.to_string(),
                    title: info.title.clone(),
                    path: Some(info.path.clone()),
                    age: Some(session_age(self.session_modified_at(info))),
                    archived: info.is_archived(),
                })
                .collect(),
        };
        let (icon, label) = relations.icon()?;
        let relations = Arc::new(relations);
        let owner = cx.entity().downgrade();
        // Click/keyboard uses the same list as hover, with the framework handling focus/Escape.
        let colors = crate::ui_theme::palette(cx);
        let trigger = Popover::new("session-relations-popover")
            .appearance(false)
            .open(self.relations_open)
            .on_open_change(cx.listener(|app, open, _, cx| {
                app.relations_open = *open;
                app.relations_hovered = false;
                cx.notify();
            }))
            .trigger(
                Button::new("session-relations")
                    .when(cfg!(test), |button| {
                        button.debug_selector(|| "session-relations".into())
                    })
                    .accessibility_label(label)
                    .icon(icon)
                    .custom(
                        ButtonCustomVariant::new(cx)
                            .foreground(colors.muted_text)
                            .hover(colors.text.opacity(0.04))
                            .active(colors.text.opacity(0.04)),
                    )
                    .small()
                    .rounded(px(5.0)),
            )
            .content({
                let relations = relations.clone();
                let owner = owner.clone();
                move |_, _, cx| relations.render(&owner, cx)
            });
        if self.relations_open {
            Some(trigger.into_any_element())
        } else {
            Some(
                HoverCard::new(SharedString::from(format!(
                    "relations-{}",
                    self.core.session.current.display()
                )))
                .appearance(false)
                .anchor(gpui_kit::Anchor::TopLeft)
                .trigger(trigger)
                .on_open_change(cx.listener(|app, open, _, cx| {
                    app.relations_hovered = *open;
                    cx.notify();
                }))
                .content(move |_, _, cx| relations.render(&owner, cx))
                .into_any_element(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relation_icons_handle_multiple_parents_and_children() {
        let row = || Relation {
            id: "id".into(),
            title: "title".into(),
            path: None,
            age: None,
            archived: false,
        };
        let mut relations = Relations::default();
        assert!(relations.icon().is_none());
        relations.parents = vec![row(), row()];
        assert!(matches!(
            relations.icon(),
            Some((DesktopIconName::SessionParents, _))
        ));
        relations.children = vec![row(), row(), row()];
        assert!(matches!(
            relations.icon(),
            Some((DesktopIconName::SessionRelations, _))
        ));
        relations.parents.clear();
        assert!(matches!(
            relations.icon(),
            Some((DesktopIconName::SessionChildren, _))
        ));
    }
}
