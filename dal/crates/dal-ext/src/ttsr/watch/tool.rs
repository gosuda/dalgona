//! Tool-argument streams for the output watch.
//!
//! Tool deltas flow through the typed argument readers, so matches see
//! decoded added text and path-qualified items. Matches that land before
//! their item path park in [`ToolStream::pending`] and resolve when the
//! path arrives; the item end drops whatever is still unpathed.

use std::sync::Arc;

use super::super::matcher::{Compiled, StreamState};
use super::super::readers::{ArgReader, ReaderEvent, reader_for};
use super::super::scope::admits_tool;
use super::super::value::{ScopeSpec, ToolScope};
use super::Watch;

pub(super) struct ToolStream {
    pub(super) reader: Box<dyn ArgReader>,
    pub(super) state: StreamState,
    pub(super) item_path: Option<String>,
    pub(super) pending: Vec<PendingFire>,
}

impl std::fmt::Debug for ToolStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolStream")
            .field("state", &self.state)
            .field("item_path", &self.item_path)
            .field("pending", &self.pending)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub(super) struct PendingFire {
    admitted: usize,
    pattern: Box<str>,
    excerpt: String,
}

impl Watch {
    pub(super) fn feed_tool(&mut self, tool: &str, delta: &str) {
        self.ensure_tool(tool);
        let mut events = Vec::new();
        if let Some(stream) = self.tools.get_mut(tool) {
            stream.reader.feed(delta, &mut events);
        }
        self.apply_tool_events(tool, &events);
    }

    pub(super) fn ensure_tool(&mut self, tool: &str) {
        if self.tools.contains_key(tool) {
            return;
        }
        let tool_rules: Vec<(usize, Vec<Arc<Compiled>>)> = self
            .admitted
            .iter()
            .enumerate()
            .filter(|(_, admitted)| admitted.tools && watches_tool(&admitted.rule.scope, tool))
            .map(|(slot, admitted)| (slot, admitted.compiled.clone()))
            .collect();
        let edit_style = self.edit_style;
        let mut state = StreamState::new();
        for (slot, compiled) in &tool_rules {
            state.add_rule(*slot, compiled);
        }
        self.tools.insert(
            tool.to_owned().into_boxed_str(),
            ToolStream {
                reader: reader_for(tool, edit_style),
                state,
                item_path: None,
                pending: Vec::new(),
            },
        );
    }

    pub(super) fn apply_tool_events(&mut self, tool: &str, events: &[ReaderEvent]) {
        for event in events {
            match event {
                ReaderEvent::ItemStart => {
                    if let Some(stream) = self.tools.get_mut(tool) {
                        stream.item_path = None;
                        stream.pending.clear();
                    }
                }
                ReaderEvent::Path(path) => {
                    let ws_root = self.ws_root.clone();
                    let mut ready = Vec::new();
                    if let Some(stream) = self.tools.get_mut(tool) {
                        stream.item_path = Some(path.clone());
                        ready = std::mem::take(&mut stream.pending);
                    }
                    for pending in ready {
                        let admitted_rule = Arc::clone(&self.admitted[pending.admitted].rule);
                        if admits_tool(&admitted_rule.scope, tool, Some(path), &ws_root) {
                            self.emit_tool_fire(
                                pending.admitted,
                                tool,
                                Some(path.clone()),
                                pending.pattern,
                                pending.excerpt,
                            );
                        }
                    }
                }
                ReaderEvent::Added(text) => {
                    self.apply_added(tool, text);
                }
                ReaderEvent::ItemEnd => {
                    if let Some(stream) = self.tools.get_mut(tool) {
                        stream.pending.clear();
                        stream.item_path = None;
                    }
                }
            }
        }
    }

    pub(super) fn apply_added(&mut self, tool: &str, text: &str) {
        let ws_root = self.ws_root.clone();
        self.judge_window.push(text.as_bytes());
        let mut matched = Vec::new();
        if let Some(stream) = self.tools.get_mut(tool) {
            stream.state.feed(text.as_bytes(), &mut matched);
        }
        for fire in matched {
            let admitted_rule = Arc::clone(&self.admitted[fire.rule].rule);
            let path = self
                .tools
                .get(tool)
                .and_then(|stream| stream.item_path.clone());
            if admits_tool(&admitted_rule.scope, tool, path.as_deref(), &ws_root) {
                self.emit_tool_fire(
                    fire.rule,
                    tool,
                    path,
                    fire.condition.src().into(),
                    fire.excerpt,
                );
            } else if path.is_none()
                && may_admit_with_path(&admitted_rule.scope, tool)
                && let Some(stream) = self.tools.get_mut(tool)
            {
                stream.pending.push(PendingFire {
                    admitted: fire.rule,
                    pattern: fire.condition.src().into(),
                    excerpt: fire.excerpt,
                });
            }
        }
    }
}

fn watches_tool(scope: &ScopeSpec, tool: &str) -> bool {
    match &scope.tools {
        ToolScope::All => true,
        ToolScope::Tools(patterns) => patterns.iter().any(|pattern| {
            pattern.available
                && (pattern.tool.as_ref() == "*" || pattern.tool.eq_ignore_ascii_case(tool))
        }),
    }
}

fn may_admit_with_path(scope: &ScopeSpec, tool: &str) -> bool {
    match &scope.tools {
        ToolScope::All => false,
        ToolScope::Tools(patterns) => patterns.iter().any(|pattern| {
            pattern.available
                && (pattern.tool.as_ref() == "*" || pattern.tool.eq_ignore_ascii_case(tool))
                && pattern.glob.is_some()
        }),
    }
}
