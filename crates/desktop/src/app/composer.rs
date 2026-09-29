use super::*;

impl DesktopApp {
    pub(crate) fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_composer_restore(window, cx);
        if self.selection_pending() || self.composer_submitting {
            return;
        }
        if !self.models[self.selected_model].model.has_api_key() {
            self.open_model_settings_dialog(window, cx);
            return;
        }
        let value = self.input.read(cx).value().trim().to_owned();
        if value.is_empty() {
            return;
        }
        let runtime = self.selected_runtime.clone();
        let edit = self.edit_draft.clone();
        if edit
            .as_ref()
            .is_some_and(|edit| edit.original.trim() == value)
        {
            return;
        }
        let Some(accepted) = runtime.update(cx, |runtime, cx| {
            if let Some(edit) = &edit {
                runtime.submit_edit(
                    value,
                    edit.target.clone(),
                    edit.revision,
                    edit.head,
                    window,
                    cx,
                )
            } else {
                runtime.submit(value, window, cx)
            }
        }) else {
            return;
        };
        let revision = self.composer_edit_revision;
        let original = self.input.read(cx).value().to_string();
        self.composer_submitting = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = accepted.await.unwrap_or_else(|_| {
                Err("Input acceptance was not confirmed; reopen the session before retrying".into())
            });
            let _ = cx.update(|window, app| {
                this.update(app, |this, cx| {
                    this.composer_submitting = false;
                    if this.selected_runtime != runtime {
                        if result.is_ok() {
                            let id = runtime.read(cx).snapshot().session.id;
                            if let Some((text, stored_edit)) = this.composer_drafts.get_mut(&id) {
                                if *text == original {
                                    *text = edit.map_or_else(String::new, |edit| edit.saved);
                                }
                                *stored_edit = None;
                            }
                        }
                        cx.notify();
                        return;
                    }
                    match result {
                        Ok(()) => {
                            let saved = this
                                .edit_draft
                                .take()
                                .map_or_else(String::new, |edit| edit.saved);
                            if this.composer_edit_revision == revision
                                && this.input.read(cx).value().as_str() == original
                            {
                                this.input.update(cx, |input, cx| {
                                    input.set_value(saved, window, cx);
                                    input.set_placeholder("Message the agent", window, cx);
                                });
                            }
                            this.dispatch(Action::Scroll(ScrollIntent::JumpToTail), window, cx);
                        }
                        Err(error) => this.notice(error),
                    }
                    cx.notify();
                })
            });
        })
        .detach();
    }

    pub(crate) fn edit_pending(
        &mut self,
        input_id: harness::InputId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selection_pending()
            || self.composer_submitting
            || !self.input.read(cx).value().is_empty()
        {
            return;
        }
        let runtime = self.selected_runtime.clone();
        let Some(pending) = runtime
            .read(cx)
            .pending_inputs()
            .into_iter()
            .find(|input| input.input_id == input_id)
        else {
            return;
        };
        let Some(accepted) = runtime.update(cx, |runtime, cx| {
            runtime.change_pending(input_id, false, window, cx)
        }) else {
            return;
        };
        let original = pending.input;
        self.input.update(cx, |input, cx| {
            input.set_value(original.clone(), window, cx);
            input.focus(window, cx);
        });
        let revision = self.composer_edit_revision;
        // Editing may continue, but resubmission must wait for durable withdrawal.
        self.composer_submitting = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = accepted
                .await
                .unwrap_or_else(|_| Err("Message could not be withdrawn".into()));
            let _ = cx.update(|window, app| {
                this.update(app, |this, cx| {
                    this.composer_submitting = false;
                    if this.selected_runtime != runtime {
                        cx.notify();
                        return;
                    }
                    if let Err(error) = result {
                        if this.composer_edit_revision == revision
                            && this.input.read(cx).value().as_str() == original
                        {
                            this.input.update(cx, |input, cx| input.set_value("", window, cx));
                        }
                        this.notice(format!("Could not edit pending message: {error}. Any changes you made remain in the composer."));
                    }
                    cx.notify();
                })
            });
        })
        .detach();
    }

    pub(crate) fn decide(&mut self, call_id: String, allow: bool, cx: &mut Context<Self>) {
        if self.selection_pending() {
            return;
        }
        self.selected_runtime
            .update(cx, |runtime, cx| runtime.decide(call_id, allow, cx));
    }

    pub(crate) fn abort(&mut self, cx: &mut Context<Self>) {
        if self.selection_pending() {
            return;
        }
        self.selected_runtime
            .update(cx, |runtime, cx| runtime.abort(cx));
    }

    pub(crate) fn notice(&mut self, text: impl Into<String>) {
        let _ = self.transition(Action::AppendTransientNotice(Box::new(message(
            Role::Notice,
            text.into(),
        ))));
    }

    pub(super) fn sync_message_presentations(&mut self) {
        let namespace = self
            .runtime_location(&self.selected_runtime)
            .map(|(project_id, session_id)| presentation_namespace(&project_id, &session_id))
            .unwrap_or_else(|| "unregistered-session".to_owned());
        self.message_presentations
            .get_mut()
            .activate(namespace.clone());
        self.chat.get_mut().activate(namespace);
    }

    pub(crate) fn set_allow_all_tools(&mut self, allow: bool, cx: &mut Context<Self>) {
        if self.selection_pending() {
            self.dispatch_local(Action::SetComposerMenu(None), cx);
            return;
        }
        if !self
            .selected_runtime
            .update(cx, |runtime, cx| runtime.set_allow_all_tools(allow, cx))
        {
            self.notice("Stop this session before changing its tool permission");
        }
        self.dispatch_local(Action::SetComposerMenu(None), cx);
        cx.notify();
    }

    pub(crate) fn set_default_allow_all_tools(&mut self, allow: bool, cx: &mut Context<Self>) {
        if let Err(error) = self.settings.set_allow_all_tools(allow) {
            self.notice(format!("Could not save default permission: {error}"));
        }
        cx.notify();
    }

    pub(crate) fn set_reasoning_effort(
        &mut self,
        effort: harness::ReasoningEffort,
        cx: &mut Context<Self>,
    ) {
        if self.selection_pending() || self.task_active() {
            return;
        }
        self.selected_runtime
            .update(cx, |runtime, cx| runtime.set_reasoning_effort(effort, cx));
        self.selected_reasoning_effort = Some(effort);
        cx.notify();
    }

    pub(crate) fn select_model(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.selection_pending()
            || self.task_active()
            || index >= self.models.len()
            || !self.models[index].model.has_api_key()
            || index == self.selected_model
        {
            return;
        }
        let configured = self.models[index].clone();
        let label = configured.label();
        self.selected_runtime
            .update(cx, |runtime, cx| runtime.select_model(&configured, cx));
        self.selected_model = index;
        self.model = label;
        self.selected_reasoning_effort = configured.reasoning_effort;
        self.dispatch_local(Action::SetComposerMenu(None), cx);
    }

    pub(crate) fn refresh_idle_runtime_models(&mut self, cx: &mut Context<Self>) {
        let updates = self
            .project_runtimes
            .values()
            .flat_map(|project| project.sessions.values())
            .filter_map(|runtime| {
                let snapshot = runtime.read(cx).snapshot();
                let model_id = snapshot.config.model.model_id?;
                let configured = self.models.iter().find(|model| model.id == model_id)?;
                Some((runtime.clone(), configured.clone()))
            })
            .collect::<Vec<_>>();
        for (runtime, configured) in updates {
            runtime.update(cx, |runtime, cx| {
                runtime.refresh_model(&configured, cx);
            });
        }
    }
}
