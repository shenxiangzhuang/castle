//! The conversation path is independent of physical journal order. This is a projection;
//! SessionMachine remains the authority that admits selections and imported histories.
use async_openai::types::responses::{
    EasyInputContent, InputItem, Item, MessageItem, OutputMessageContent,
};
use im::{OrdMap, Vector};
use serde::{Deserialize, Serialize};

use super::SessionId;
use super::event::{InputId, RecordedEvent, RequestId, SessionEvent, StepOutcome};

pub type ConversationNodeId = u64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkOrigin {
    pub session_id: SessionId,
    pub revision: u64,
    pub anchor: ConversationNodeId,
    pub head: Option<ConversationNodeId>,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConversationNodeKind {
    User(InputId),
    Assistant(RequestId),
    Tool,
    Compaction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationNode {
    pub id: ConversationNodeId,
    pub parent: Option<ConversationNodeId>,
    pub kind: ConversationNodeKind,
    pub text: String,
    pub settled: bool,
    pub completed: bool,
    pub safe: bool,
    pub inherited: bool,
    open_tools: usize,
}

impl ConversationNode {
    pub fn input_id(&self) -> Option<&InputId> {
        match &self.kind {
            ConversationNodeKind::User(id) => Some(id),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConversationTree {
    nodes: OrdMap<ConversationNodeId, ConversationNode>,
    head: Option<ConversationNodeId>,
    requests: OrdMap<RequestId, ConversationNodeId>,
    inputs: OrdMap<InputId, (String, usize)>,
    input_nodes: OrdMap<InputId, ConversationNodeId>,
    owners: Vector<Option<ConversationNodeId>>,
    events: Vector<RecordedEvent>,
    prefix: Vec<usize>,
    pub origin: Option<ForkOrigin>,
    branches: usize,
    children: OrdMap<Option<ConversationNodeId>, usize>,
}

impl ConversationTree {
    pub(crate) fn mark_inherited(&mut self) {
        let ids: Vec<_> = self.nodes.keys().copied().collect();
        for id in ids {
            if let Some(node) = self.nodes.get_mut(&id) {
                node.inherited = true;
            }
        }
    }
    pub fn head(&self) -> Option<ConversationNodeId> {
        self.head
    }
    pub fn nodes(&self) -> impl DoubleEndedIterator<Item = &ConversationNode> {
        self.nodes.values()
    }
    pub fn node(&self, id: ConversationNodeId) -> Option<&ConversationNode> {
        self.nodes.get(&id)
    }
    pub fn has_branches(&self) -> bool {
        self.branches > 0
    }
    pub fn path(&self, head: Option<ConversationNodeId>) -> Vec<ConversationNodeId> {
        let mut path = Vec::new();
        let mut cursor = head;
        while let Some(node) = cursor.and_then(|id| self.nodes.get(&id)) {
            path.push(node.id);
            cursor = node.parent;
        }
        path.reverse();
        path
    }
    pub fn can_select(&self, head: Option<ConversationNodeId>) -> bool {
        head.is_none_or(|id| {
            self.nodes
                .get(&id)
                .is_some_and(|node| node.settled && node.safe)
        })
    }
    /// Evidence for one path. Boundary-only events may leave a historical lifecycle open;
    /// the fork builder closes those brackets through the normal recovery planner.
    pub fn path_events(&self, head: Option<ConversationNodeId>) -> Vec<RecordedEvent> {
        let path: std::collections::HashSet<_> = self.path(head).into_iter().collect();
        self.events
            .iter()
            .zip(&self.owners)
            .filter(|(_, owner)| owner.is_some_and(|id| path.contains(&id)))
            .map(|(event, _)| event.clone())
            .collect()
    }
    pub fn input_node(&self, input: &InputId) -> Option<&ConversationNode> {
        self.input_nodes
            .get(input)
            .and_then(|id| self.nodes.get(id))
    }
    pub fn request_node(&self, request: &RequestId) -> Option<&ConversationNode> {
        self.requests.get(request).and_then(|id| self.nodes.get(id))
    }

    fn push(&mut self, kind: ConversationNodeKind, text: String, settled: bool) -> u64 {
        let id = self.nodes.len() as u64 + 1;
        let count = self.children.entry(self.head).or_default();
        if *count > 0 {
            self.branches += 1;
        }
        *count += 1;
        let open_tools = self
            .head
            .and_then(|id| self.nodes.get(&id))
            .map_or(0, |node| node.open_tools);
        self.nodes.insert(
            id,
            ConversationNode {
                id,
                parent: self.head,
                completed: !matches!(kind, ConversationNodeKind::Assistant(_)),
                kind,
                text,
                settled,
                safe: open_tools == 0,
                inherited: false,
                open_tools,
            },
        );
        self.head = Some(id);
        for index in self.prefix.drain(..) {
            self.owners.set(index, Some(id));
        }
        id
    }

    /// Only call on accepted committed events (or a candidate already validated by the owner).
    pub fn observe(&mut self, recorded: &RecordedEvent) {
        if let SessionEvent::SessionForked { origin, events } = &recorded.event {
            for event in events {
                self.observe(event);
            }
            self.mark_inherited();
            self.origin = Some(origin.clone());
            return;
        }
        if let SessionEvent::ConversationHeadSelected { head } = &recorded.event {
            self.head = *head;
            self.prefix.clear();
            return;
        }
        let index = self.events.len();
        // Internal evidence offsets also span imported history, unlike child journal seqs.
        let mut evidence = recorded.clone();
        evidence.seq = index as u64;
        self.events.push_back(evidence);
        self.owners.push_back(self.head);
        match &recorded.event {
            SessionEvent::RunStarted { .. }
            | SessionEvent::TurnStarted { .. }
            | SessionEvent::StepStarted { .. } => {
                self.prefix.push(index);
                self.owners.set(index, None);
            }
            SessionEvent::InputSubmitted {
                input_id, input, ..
            } => {
                self.inputs.insert(input_id.clone(), (input.clone(), index));
                self.owners.set(index, None);
            }
            SessionEvent::InputPrioritized { .. } | SessionEvent::InputCancelled { .. } => {
                self.owners.set(index, None);
            }
            SessionEvent::InputAttached { input_id, .. } => {
                if let Some((text, submitted)) = self.inputs.get(input_id).cloned() {
                    let id = self.push(ConversationNodeKind::User(input_id.clone()), text, true);
                    self.input_nodes.insert(input_id.clone(), id);
                    self.owners.set(submitted, Some(id));
                    self.owners.set(index, Some(id));
                }
            }
            SessionEvent::RequestSnapshot { request_id, .. } => {
                let id = self.push(
                    ConversationNodeKind::Assistant(request_id.clone()),
                    String::new(),
                    false,
                );
                self.requests.insert(request_id.clone(), id);
                self.owners.set(index, Some(id));
            }
            SessionEvent::AssistantChunk { request_id, chunk } => {
                if let Some(id) = self.requests.get(request_id).copied() {
                    self.owners.set(index, Some(id));
                    if let super::event::AssistantChunk::OutputTextDelta { delta } = chunk
                        && let Some(node) = self.nodes.get_mut(&id)
                    {
                        append_label(&mut node.text, delta);
                    }
                }
            }
            SessionEvent::AssistantCompleted {
                request_id, items, ..
            } => {
                if let Some(id) = self.requests.get(request_id).copied() {
                    self.owners.set(index, Some(id));
                    if let Some(node) = self.nodes.get_mut(&id) {
                        node.settled = true;
                        node.completed = true;
                        node.open_tools += items
                            .iter()
                            .filter(|item| {
                                matches!(
                                    item,
                                    async_openai::types::responses::InputItem::Item(
                                        async_openai::types::responses::Item::FunctionCall(_)
                                    )
                                )
                            })
                            .count();
                        node.safe = node.open_tools == 0;
                        node.text.clear();
                        for item in items {
                            match item {
                                InputItem::EasyMessage(message) => {
                                    if let EasyInputContent::Text(text) = &message.content {
                                        append_label(&mut node.text, text);
                                    }
                                }
                                InputItem::Item(Item::Message(MessageItem::Output(message))) => {
                                    for content in &message.content {
                                        append_label(
                                            &mut node.text,
                                            match content {
                                                OutputMessageContent::OutputText(content) => {
                                                    &content.text
                                                }
                                                OutputMessageContent::Refusal(content) => {
                                                    &content.refusal
                                                }
                                            },
                                        );
                                    }
                                }
                                _ => {}
                            }
                        }
                        if node.text.is_empty() {
                            node.text = "Assistant response".into();
                        }
                    }
                }
            }
            SessionEvent::ModelRequestFailed { request_id, .. } => {
                if let Some(id) = self.requests.get(request_id).copied() {
                    self.owners.set(index, Some(id));
                    if let Some(node) = self.nodes.get_mut(&id) {
                        node.settled = true;
                    }
                }
            }
            SessionEvent::ToolResultAttached { .. } => {
                let id = self.push(ConversationNodeKind::Tool, "Tool result".into(), true);
                self.owners.set(index, Some(id));
                if let Some(node) = self.nodes.get_mut(&id) {
                    node.open_tools = node.open_tools.saturating_sub(1);
                    node.safe = node.open_tools == 0;
                }
            }
            SessionEvent::CompactionStarted { .. } => {
                let id = self.push(
                    ConversationNodeKind::Compaction,
                    "Context compaction".into(),
                    false,
                );
                self.owners.set(index, Some(id));
            }
            SessionEvent::CompactionFinished { .. } => {
                if let Some(node) = self.head.and_then(|id| self.nodes.get_mut(&id)) {
                    node.settled = true;
                }
            }
            SessionEvent::StepTerminated { outcome, .. } if *outcome != StepOutcome::Completed => {
                if let Some(node) = self.head.and_then(|id| self.nodes.get_mut(&id)) {
                    node.settled = true;
                }
            }
            _ => {}
        }
    }
}

// Labels remain bounded even for very large streaming/provider messages.
fn append_label(label: &mut String, text: &str) {
    let remaining = 120usize.saturating_sub(label.chars().count());
    label.extend(text.chars().take(remaining));
}
