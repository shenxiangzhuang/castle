use super::*;
use crate::domain::MessageId;
use crate::domain::session_document::ConversationItemId;
use harness::{ConversationNode, ConversationNodeKind, InputId};

#[derive(Clone)]
pub(crate) struct EditDraft {
    pub(crate) target: InputId,
    pub(crate) original: String,
    pub(crate) revision: u64,
    pub(crate) head: Option<u64>,
    pub(super) saved: String,
}

impl DesktopApp {
    pub(crate) fn message_node(
        &self,
        key: MessageId,
        cx: &Context<Self>,
    ) -> Option<ConversationNode> {
        let runtime = self.selected_runtime.read(cx);
        match self.core.session_view.item_id(key)? {
            ConversationItemId::Input(id) => runtime.tree().input_node(id).cloned(),
            ConversationItemId::ResponseSegment { request_id, .. } => {
                runtime.tree().request_node(request_id).cloned()
            }
            _ => None,
        }
    }

    pub(crate) fn can_edit_message(&self, key: MessageId, cx: &Context<Self>) -> bool {
        let runtime = self.selected_runtime.read(cx);
        !self.selection_pending()
            && !self.composer_submitting
            && self.edit_draft.is_none()
            && runtime.can_branch()
            && self
                .message_node(key, cx)
                .is_some_and(|node| node.input_id().is_some())
    }

    pub(crate) fn can_fork_message(&self, key: MessageId, cx: &Context<Self>) -> bool {
        let runtime = self.selected_runtime.read(cx);
        !self.selection_pending()
            && !self.composer_submitting
            && self.edit_draft.is_none()
            && runtime.can_branch()
            && self.message_node(key, cx).is_some_and(|node| {
                matches!(node.kind, ConversationNodeKind::Assistant(_))
                    && node.settled
                    && node.completed
                    && node.safe
            })
    }

    pub(crate) fn edit_message(
        &mut self,
        key: MessageId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_edit_message(key, cx) {
            return;
        }
        let Some(node) = self.message_node(key, cx) else {
            return;
        };
        let Some(target) = node.input_id().cloned() else {
            return;
        };
        let runtime = self.selected_runtime.read(cx);
        self.edit_draft = Some(EditDraft {
            target,
            original: node.text.clone(),
            revision: runtime.observation().durable_revision,
            head: runtime.tree().head(),
            saved: self.input.read(cx).value().to_string(),
        });
        self.input.update(cx, |input, cx| {
            input.set_value(node.text, window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    pub(crate) fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer_submitting {
            return;
        }
        if let Some(edit) = self.edit_draft.take() {
            self.input
                .update(cx, |input, cx| input.set_value(edit.saved, window, cx));
            cx.notify();
        }
    }

    pub(crate) fn apply_composer_restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.pending_fork_refresh) {
            // Catalog changes (including parent deletion) can change the divider height.
            self.chat.borrow().list.remeasure();
            for project in self.project_runtimes.values() {
                for runtime in project.sessions.values() {
                    runtime.update(cx, |runtime, cx| runtime.refresh_fork_children(cx));
                }
            }
        }
        if let Some((text, displaced)) = self.composer_restore.take()
            && self.input.read(cx).value().as_str() == displaced
        {
            self.input
                .update(cx, |input, cx| input.set_value(text, window, cx));
        }
    }

    pub(crate) fn fork_message(
        &mut self,
        key: MessageId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_fork_message(key, cx) {
            return;
        }
        let Some(node) = self.message_node(key, cx) else {
            return;
        };
        let Some(project_id) = self
            .project_store
            .project(self.core.workspace.active_project)
            .map(|project| project.id.clone())
        else {
            return;
        };
        let Some(result) = self
            .selected_runtime
            .update(cx, |runtime, cx| runtime.fork(node.id, Some(node.id), cx))
        else {
            return;
        };
        let generation = self.begin_runtime_selection_intent();
        cx.spawn_in(window, async move |this, cx| {
            let result = result
                .await
                .unwrap_or_else(|_| Err("Could not create fork".into()));
            let _ = cx.update(|window, app| {
                this.update(app, |this, cx| {
                    match result {
                        Ok(session) => {
                            let project_index = this
                                .project_store
                                .projects()
                                .iter()
                                .position(|project| project.id == project_id);
                            if let Some(runtime) = project_index
                                .and_then(|index| this.create_runtime(index, session, cx))
                            {
                                if generation == this.open_generation {
                                    this.select_runtime(runtime, cx);
                                    this.apply_composer_restore(window, cx);
                                }
                                this.refresh_project_catalog(&project_id);
                            }
                        }
                        Err(error) => this.notice(error),
                    }
                    cx.notify();
                })
            });
        })
        .detach();
    }
}
