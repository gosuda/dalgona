//! A2A 1.0.0 task states, transitions, and error mapping.
//!
//! Context is a session; task is a turn. Terminal states are absorbing.
//! Turn execution itself runs through `Host` and lands with the agent seam.

/// An A2A task state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskState {
    /// The task was submitted and awaits a worker.
    Submitted,
    /// The task is running.
    Working,
    /// The task waits for input.
    InputRequired,
    /// The task completed.
    Completed,
    /// The task was refused.
    Rejected,
    /// The task was canceled.
    Canceled,
    /// The task failed.
    Failed,
}

impl TaskState {
    /// Returns true for terminal states.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Rejected | Self::Canceled | Self::Failed
        )
    }

    /// Returns the A2A 1.0 wire name of this state.
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Submitted => "TASK_STATE_SUBMITTED",
            Self::Working => "TASK_STATE_WORKING",
            Self::InputRequired => "TASK_STATE_INPUT_REQUIRED",
            Self::Completed => "TASK_STATE_COMPLETED",
            Self::Rejected => "TASK_STATE_REJECTED",
            Self::Canceled => "TASK_STATE_CANCELED",
            Self::Failed => "TASK_STATE_FAILED",
        }
    }
}

/// An event that advances one task.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskEvent {
    /// A worker picked up the task.
    Start,
    /// A question opened.
    RequestInput,
    /// An answer arrived.
    Answer,
    /// The turn ended normally.
    Complete,
    /// The turn was refused.
    Refuse,
    /// Cancellation arrived.
    Cancel,
    /// The turn failed.
    Fail,
}

/// Advances one task, returning the next state or an error for illegal moves.
#[must_use]
pub fn transition(state: TaskState, event: TaskEvent) -> Option<TaskState> {
    match (state, event) {
        (TaskState::Submitted, TaskEvent::Start)
        | (TaskState::InputRequired, TaskEvent::Answer) => Some(TaskState::Working),
        (TaskState::Working, TaskEvent::RequestInput) => Some(TaskState::InputRequired),
        (TaskState::Working, TaskEvent::Complete) => Some(TaskState::Completed),
        (TaskState::Working, TaskEvent::Refuse) => Some(TaskState::Rejected),
        (TaskState::Working | TaskState::InputRequired, TaskEvent::Cancel) => {
            Some(TaskState::Canceled)
        }
        (TaskState::Working, TaskEvent::Fail) => Some(TaskState::Failed),
        _ => None,
    }
}

/// A stable A2A error with JSON-RPC, REST, and status mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct A2aError {
    /// The JSON-RPC code.
    pub code: i32,
    /// The HTTP status.
    pub http: u16,
    /// The REST status name.
    pub status: &'static str,
    /// The protocol reason name.
    pub reason: &'static str,
}

/// Looks up the stable error for one reason name.
#[must_use]
pub fn error_for(reason: &str) -> A2aError {
    match reason {
        "TASK_NOT_FOUND" => A2aError {
            code: -32001,
            http: 404,
            status: "NOT_FOUND",
            reason: "TASK_NOT_FOUND",
        },
        "TASK_NOT_CANCELABLE" => A2aError {
            code: -32002,
            http: 400,
            status: "FAILED_PRECONDITION",
            reason: "TASK_NOT_CANCELABLE",
        },
        "UNSUPPORTED_OPERATION" => A2aError {
            code: -32004,
            http: 400,
            status: "UNIMPLEMENTED",
            reason: "UNSUPPORTED_OPERATION",
        },
        "CONTENT_TYPE_NOT_SUPPORTED" => A2aError {
            code: -32005,
            http: 400,
            status: "INVALID_ARGUMENT",
            reason: "CONTENT_TYPE_NOT_SUPPORTED",
        },
        "VERSION_NOT_SUPPORTED" => A2aError {
            code: -32009,
            http: 400,
            status: "FAILED_PRECONDITION",
            reason: "VERSION_NOT_SUPPORTED",
        },
        "INVALID_ARGUMENT" => A2aError {
            code: -32602,
            http: 400,
            status: "INVALID_ARGUMENT",
            reason: "INVALID_ARGUMENT",
        },
        "METHOD_NOT_FOUND" => A2aError {
            code: -32601,
            http: 404,
            status: "NOT_FOUND",
            reason: "METHOD_NOT_FOUND",
        },
        "INVALID_REQUEST" => A2aError {
            code: -32600,
            http: 400,
            status: "INVALID_ARGUMENT",
            reason: "INVALID_REQUEST",
        },
        "PARSE_ERROR" => A2aError {
            code: -32700,
            http: 400,
            status: "INVALID_ARGUMENT",
            reason: "PARSE_ERROR",
        },
        _ => A2aError {
            code: -32603,
            http: 500,
            status: "INTERNAL",
            reason: "INTERNAL",
        },
    }
}
