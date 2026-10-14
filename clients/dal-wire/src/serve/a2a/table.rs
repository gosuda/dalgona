//! The bounded A2A context and task tables.
//!
//! At most 4096 contexts are kept; the oldest context without a live task
//! is evicted. At most 1024 tasks are kept; the oldest terminal task is
//! evicted, and a table full of unfinished tasks refuses new ones.

use std::collections::VecDeque;
use std::fmt;
use std::num::NonZeroU64;

use dal_core::{Request, SessionId, TurnId};
use sonic_rs::Value;
use tokio::sync::{mpsc, watch};

use super::error::Fail;
use crate::a2a::{TaskEvent, TaskState, transition};

/// Maximum retained contexts.
pub(crate) const MAX_CONTEXTS: usize = 4096;
/// Maximum retained tasks.
pub(crate) const MAX_TASKS: usize = 1024;
/// The refusal text when every retained task is unfinished.
pub(crate) const TASK_LIMIT_TEXT: &str = "dalgon serve holds 1024 unfinished A2A tasks";

/// One task identity: the context session plus its turn.
///
/// Turn ids are scoped to one session, so the wire task id is
/// `<contextId>.<turn>`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct TaskKey {
    /// The context session.
    pub session: SessionId,
    /// The turn inside that session.
    pub turn: TurnId,
}

impl TaskKey {
    /// Parses one wire task id.
    pub(crate) fn parse(id: &str) -> Option<Self> {
        let (session, turn) = id.rsplit_once('.')?;
        let session = SessionId::parse(session).ok()?;
        let turn = turn.parse::<u64>().ok().and_then(NonZeroU64::new)?;
        Some(Self {
            session,
            turn: TurnId::new(turn),
        })
    }
}

impl fmt::Display for TaskKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.session, self.turn)
    }
}

/// How one SSE sink frames its stream responses.
#[derive(Clone, Debug)]
pub(crate) enum Framing {
    /// HTTP+JSON: `data: <StreamResponse>`.
    Rest,
    /// JSON-RPC: each response wrapped in a result envelope with this id.
    JsonRpc(Value),
}

/// One live SSE consumer of a task's events.
#[derive(Clone, Debug)]
pub(crate) struct Sink {
    /// The event-stream body sender; `None` closes the stream.
    pub tx: mpsc::Sender<Option<String>>,
    /// The framing of every event.
    pub framing: Framing,
}

/// One retained A2A task.
#[derive(Debug)]
pub(crate) struct TaskRec {
    /// The task identity.
    pub key: TaskKey,
    /// The current state.
    pub state: TaskState,
    /// The open request while input is required.
    pub request: Option<Request>,
    /// The user prompt text of the task.
    pub prompt: String,
    /// The assistant text collected from deltas.
    pub text: String,
    /// The number of text chunks already streamed.
    pub chunks: usize,
    /// Live SSE sinks.
    pub sinks: Vec<Sink>,
    /// Publishes every state change to waiting requests.
    pub watch: watch::Sender<TaskState>,
}

impl TaskRec {
    /// Creates a task that a worker has already picked up.
    pub(crate) fn started(key: TaskKey, prompt: String) -> Self {
        let state =
            transition(TaskState::Submitted, TaskEvent::Start).unwrap_or(TaskState::Working);
        let (watch, _) = watch::channel(state);
        Self {
            key,
            state,
            request: None,
            prompt,
            text: String::new(),
            chunks: 0,
            sinks: Vec::new(),
            watch,
        }
    }

    /// Applies one state event; illegal moves change nothing and return false.
    pub(crate) fn advance(&mut self, event: TaskEvent) -> bool {
        match transition(self.state, event) {
            Some(next) => {
                self.state = next;
                if next != TaskState::InputRequired {
                    self.request = None;
                }
                self.watch.send_replace(next);
                true
            }
            None => false,
        }
    }
}

/// One retained context.
#[derive(Debug)]
struct ContextRec {
    /// The backing session.
    session: SessionId,
}

/// The A2A context and task tables.
#[derive(Default)]
pub(crate) struct A2aState {
    /// Retained contexts in insertion order.
    contexts: VecDeque<ContextRec>,
    /// Retained tasks in insertion order.
    tasks: VecDeque<TaskRec>,
    /// Bound agents per context session.
    agents: Vec<(SessionId, dal_agent::Agent)>,
}

impl A2aState {
    /// Creates empty tables.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Returns true when the context is retained.
    pub(crate) fn has_context(&self, session: SessionId) -> bool {
        self.contexts
            .iter()
            .any(|context| context.session == session)
    }

    /// Returns the number of retained contexts.
    #[cfg(test)]
    pub(crate) fn context_count(&self) -> usize {
        self.contexts.len()
    }

    /// Returns the number of retained tasks.
    #[cfg(test)]
    pub(crate) fn task_count(&self) -> usize {
        self.tasks.len()
    }

    /// Records one context, returning the sessions evicted to make room.
    pub(crate) fn remember_context(&mut self, session: SessionId) -> Vec<SessionId> {
        let mut evicted = Vec::new();
        if self.has_context(session) {
            return evicted;
        }
        while self.contexts.len() >= MAX_CONTEXTS {
            let idle = self
                .contexts
                .iter()
                .position(|context| self.live_task(context.session).is_none());
            let Some(index) = idle else { break };
            if let Some(context) = self.contexts.remove(index) {
                self.agents.retain(|(bound, _)| *bound != context.session);
                evicted.push(context.session);
            }
        }
        self.contexts.push_back(ContextRec { session });
        evicted
    }

    /// Binds the agent that serves one context.
    pub(crate) fn bind_agent(&mut self, session: SessionId, agent: dal_agent::Agent) {
        self.agents.retain(|(bound, _)| *bound != session);
        self.agents.push((session, agent));
    }

    /// Returns the agent bound to one context.
    pub(crate) fn agent(&self, session: SessionId) -> Option<dal_agent::Agent> {
        self.agents
            .iter()
            .find(|(bound, _)| *bound == session)
            .map(|(_, agent)| agent.clone())
    }

    /// Returns the unfinished task of one context.
    pub(crate) fn live_task(&self, session: SessionId) -> Option<&TaskRec> {
        self.tasks
            .iter()
            .find(|task| task.key.session == session && !task.state.is_terminal())
    }

    /// Makes room for one task by evicting the oldest terminal task.
    ///
    /// # Errors
    ///
    /// Returns the `-32603` limit failure when every retained task is unfinished.
    pub(crate) fn reserve(&mut self) -> Result<(), Fail> {
        while self.tasks.len() >= MAX_TASKS {
            let terminal = self.tasks.iter().position(|task| task.state.is_terminal());
            match terminal {
                Some(index) => {
                    self.tasks.remove(index);
                }
                None => return Err(Fail::internal(TASK_LIMIT_TEXT)),
            }
        }
        Ok(())
    }

    /// Inserts one task after reserving its slot.
    ///
    /// # Errors
    ///
    /// Returns the `-32603` limit failure when every retained task is unfinished.
    pub(crate) fn insert(&mut self, task: TaskRec) -> Result<(), Fail> {
        self.reserve()?;
        self.tasks.push_back(task);
        Ok(())
    }

    /// Finds one task.
    pub(crate) fn task(&self, key: TaskKey) -> Option<&TaskRec> {
        self.tasks.iter().find(|task| task.key == key)
    }

    /// Finds one task mutably.
    pub(crate) fn task_mut(&mut self, key: TaskKey) -> Option<&mut TaskRec> {
        self.tasks.iter_mut().find(|task| task.key == key)
    }
}
