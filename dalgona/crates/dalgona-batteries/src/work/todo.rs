// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use std::fmt;

use dal_agent::error::ServiceError;
use dal_agent::ext::{Caller, Services, ToolOutcome};
use dal_core::RawJson;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub(crate) const TODO_KIND: &str = "todo";
pub(crate) const NO_TODOS: &str = "No todo list.";
pub(crate) const UPDATED: &str = "Todo list updated.";
pub(crate) const MAX_ITEMS: usize = 26;
pub(crate) const MAX_SUBJECT_BYTES: usize = 200;
pub(crate) const MAX_DESCRIPTION_BYTES: usize = 2000;
pub(crate) const TOOL_INPUT_CAP: usize = 16_384;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TodoState {
    Pending,
    InProgress,
    Done,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TodoItem {
    pub subject: String,
    pub description: String,
    pub state: TodoState,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct TodoRecord {
    pub list: Vec<TodoItem>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TodoArgs {
    /// `read` returns the current list; `write` replaces it with `todos`.
    #[schemars(with = "TodoActionSchema")]
    pub action: String,
    /// The complete replacement list, 1 to 26 items; present only for `write`.
    #[serde(default)]
    #[schemars(with = "TodosFieldSchema")]
    pub todos: TodosField,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TodoItemInput {
    /// A short title, 1 to 200 bytes after trimming.
    pub subject: String,
    /// Optional detail, at most 2000 bytes.
    #[serde(default)]
    pub description: String,
    /// `pending`, `in_progress` (at most one item), `done`, or `cancelled`.
    #[schemars(with = "TodoStateSchema")]
    pub state: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum TodoActionSchema {
    Read,
    Write,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum TodoStateSchema {
    Pending,
    InProgress,
    Done,
    Cancelled,
}

/// Presence of the `todos` member: absent, explicit null, or a complete array.
/// Explicit null is malformed for both actions; only absence means "no member".
#[derive(Clone, Debug, Default)]
pub(crate) enum TodosField {
    #[default]
    Absent,
    Null,
    List(Vec<TodoItemInput>),
}

type TodosFieldSchema = Option<Vec<TodoItemInput>>;

impl<'de> Deserialize<'de> for TodosField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct TodosVisitor;
        impl<'de> serde::de::Visitor<'de> for TodosVisitor {
            type Value = TodosField;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an absent todos member, null, or an array of todo items")
            }
            fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(TodosField::Null)
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(TodosField::Null)
            }
            fn visit_some<D: serde::Deserializer<'de>>(
                self,
                deserializer: D,
            ) -> Result<Self::Value, D::Error> {
                Vec::<TodoItemInput>::deserialize(deserializer).map(TodosField::List)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element()? {
                    items.push(item);
                }
                Ok(TodosField::List(items))
            }
        }
        deserializer.deserialize_option(TodosVisitor)
    }
}

pub(crate) enum TodoAction {
    Read,
    Write(Vec<TodoItem>),
}

#[derive(Debug)]
pub(crate) enum TodoInputError {
    Argument(String),
    InvalidTodo(super::Error),
}

impl fmt::Display for TodoInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Argument(message) => formatter.write_str(message),
            Self::InvalidTodo(error) => fmt::Display::fmt(error, formatter),
        }
    }
}

pub(crate) fn parse_args(
    args: TodoArgs,
    state_display_cap: usize,
) -> Result<TodoAction, TodoInputError> {
    match args.action.as_str() {
        "read" => match args.todos {
            TodosField::Absent => Ok(TodoAction::Read),
            TodosField::Null | TodosField::List(_) => Err(TodoInputError::Argument(
                "todo: todos is not valid for action read".to_owned(),
            )),
        },
        "write" => {
            let inputs = match args.todos {
                TodosField::List(inputs) => inputs,
                TodosField::Absent => {
                    return Err(TodoInputError::Argument(
                        "todo: action \"write\" requires \"todos\"".to_owned(),
                    ));
                }
                TodosField::Null => {
                    return Err(TodoInputError::Argument(
                        "todo: todos must be an array for action write".to_owned(),
                    ));
                }
            };
            if !(1..=MAX_ITEMS).contains(&inputs.len()) {
                return Err(TodoInputError::InvalidTodo(invalid("1 to 26 items")));
            }
            let mut items = Vec::with_capacity(inputs.len());
            for input in inputs {
                let state = match input.state.as_str() {
                    "pending" => TodoState::Pending,
                    "in_progress" => TodoState::InProgress,
                    "done" => TodoState::Done,
                    "cancelled" => TodoState::Cancelled,
                    unknown => {
                        let unknown = truncate_utf8(unknown, state_display_cap);
                        return Err(TodoInputError::InvalidTodo(super::Error::TodoInvalid {
                            rule: format!("unknown state \"{unknown}\""),
                        }));
                    }
                };
                items.push(TodoItem {
                    subject: input.subject.trim().to_owned(),
                    description: input.description,
                    state,
                });
            }
            validate(&items).map_err(TodoInputError::InvalidTodo)?;
            Ok(TodoAction::Write(items))
        }
        _ => Err(TodoInputError::Argument(
            "todo: action must be read or write".to_owned(),
        )),
    }
}

fn truncate_utf8(text: &str, byte_cap: usize) -> &str {
    let mut end = text.len().min(byte_cap);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

pub(crate) fn validate(items: &[TodoItem]) -> Result<(), super::Error> {
    if !(1..=MAX_ITEMS).contains(&items.len()) {
        return Err(invalid("1 to 26 items"));
    }
    for item in items {
        if !(1..=MAX_SUBJECT_BYTES).contains(&item.subject.len()) {
            return Err(invalid("subject must be 1 to 200 bytes"));
        }
        if item.description.len() > MAX_DESCRIPTION_BYTES {
            return Err(invalid("description must be at most 2000 bytes"));
        }
    }
    if items
        .iter()
        .filter(|item| item.state == TodoState::InProgress)
        .count()
        > 1
    {
        return Err(invalid("at most one in_progress"));
    }
    Ok(())
}

fn invalid(rule: &str) -> super::Error {
    super::Error::TodoInvalid {
        rule: rule.to_owned(),
    }
}

pub(crate) fn fold(records: &[Box<RawJson>]) -> Vec<TodoItem> {
    fold_bodies(records.iter().map(|body| &**body))
}

/// Folds borrowed current-leaf record bodies, replacing the accumulator with
/// each decodable whole-list body and skipping malformed bodies.
pub(crate) fn fold_bodies<'a>(bodies: impl Iterator<Item = &'a RawJson>) -> Vec<TodoItem> {
    let mut items = Vec::new();
    for body in bodies {
        if let Ok(record) = body.decode_as::<TodoRecord>() {
            items = record.list;
        }
    }
    items
}

pub(crate) fn render(items: &[TodoItem]) -> String {
    let capacity = items
        .iter()
        .map(|item| {
            6 + item.subject.len()
                + match &item.state {
                    TodoState::Pending | TodoState::Done => 0,
                    TodoState::InProgress => " (in progress)".len(),
                    TodoState::Cancelled => " (cancelled)".len(),
                }
        })
        .sum::<usize>()
        + items.len().saturating_sub(1);
    let mut output = String::with_capacity(capacity);
    for (index, item) in items.iter().enumerate() {
        if index != 0 {
            output.push('\n');
        }
        match &item.state {
            TodoState::Pending | TodoState::InProgress => output.push_str("- [ ] "),
            TodoState::Done => output.push_str("- [x] "),
            TodoState::Cancelled => output.push_str("- [-] "),
        }
        output.push_str(&item.subject);
        match &item.state {
            TodoState::Pending | TodoState::Done => {}
            TodoState::InProgress => output.push_str(" (in progress)"),
            TodoState::Cancelled => output.push_str(" (cancelled)"),
        }
    }
    output
}

pub(crate) fn display(items: &[TodoItem]) -> String {
    if items.is_empty() {
        NO_TODOS.to_owned()
    } else {
        render(items)
    }
}

pub(crate) async fn load(
    services: &dyn Services,
    caller: &Caller,
) -> Result<Vec<TodoItem>, ServiceError> {
    let records = services.records(caller, TODO_KIND).await?;
    Ok(fold(&records))
}

pub(crate) async fn tool(
    args: &str,
    services: &dyn Services,
    caller: &Caller,
) -> Result<String, ToolOutcome> {
    let args = sonic_rs::from_str::<TodoArgs>(args)
        .map_err(|error| super::failure(format!("todo: invalid input: {error}")))?;
    match parse_args(args, TOOL_INPUT_CAP).map_err(super::failure)? {
        TodoAction::Read => {
            let items = load(services, caller)
                .await
                .map_err(super::service_outcome)?;
            Ok(display(&items))
        }
        TodoAction::Write(list) => {
            let record = TodoRecord { list };
            super::append(services, caller, TODO_KIND, &record)
                .await
                .map_err(super::service_outcome)?;
            Ok(format!("{}\n{UPDATED}", render(&record.list)))
        }
    }
}

pub(crate) fn open_todos(items: &[TodoItem]) -> (usize, usize, Vec<String>) {
    let mut open = 0;
    let mut first_titles = Vec::with_capacity(3);
    for item in items {
        if matches!(&item.state, TodoState::Pending | TodoState::InProgress) {
            open += 1;
            if first_titles.len() < 3 {
                first_titles.push(item.subject.clone());
            }
        }
    }
    (open, items.len(), first_titles)
}

pub(crate) fn todo_all_terminal(items: &[TodoItem]) -> bool {
    items
        .iter()
        .all(|item| matches!(&item.state, TodoState::Done | TodoState::Cancelled))
}

#[cfg(test)]
mod tests {
    use std::error::Error as StdError;

    use super::*;

    #[derive(Deserialize)]
    struct OracleRecord {
        list: Vec<OracleItem>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum OracleState {
        Pending,
        InProgress,
        Done,
        Cancelled,
    }

    #[derive(Deserialize)]
    struct OracleItem {
        subject: String,
        description: String,
        state: OracleState,
    }

    fn item(subject: &str, state: TodoState) -> TodoItem {
        TodoItem {
            subject: subject.to_owned(),
            description: String::new(),
            state,
        }
    }

    fn record(items: Vec<TodoItem>) -> Result<Box<RawJson>, Box<dyn StdError>> {
        let body = sonic_rs::to_string(&TodoRecord { list: items })?;
        Ok(Box::new(RawJson::parse(&body)?))
    }

    fn error_text<T, E: fmt::Display>(result: Result<T, E>) -> String {
        match result {
            Ok(_) => "operation unexpectedly succeeded".to_owned(),
            Err(error) => error.to_string(),
        }
    }

    fn fold_oracle(records: &[Box<RawJson>]) -> Vec<TodoItem> {
        let mut current = Vec::new();
        for body in records {
            if let Ok(record) = body.decode_as::<OracleRecord>() {
                current = record
                    .list
                    .into_iter()
                    .map(|item| TodoItem {
                        subject: item.subject,
                        description: item.description,
                        state: match item.state {
                            OracleState::Pending => TodoState::Pending,
                            OracleState::InProgress => TodoState::InProgress,
                            OracleState::Done => TodoState::Done,
                            OracleState::Cancelled => TodoState::Cancelled,
                        },
                    })
                    .collect();
            }
        }
        current
    }

    #[test]
    fn todo_fold_uses_last_decodable_whole_list() -> Result<(), Box<dyn StdError>> {
        let records = [
            record(vec![item("first", TodoState::Pending)])?,
            Box::new(RawJson::parse(r#"{"list":{}}"#)?),
            record(vec![item("replacement", TodoState::Done)])?,
        ];

        assert_eq!(fold(&records), vec![item("replacement", TodoState::Done)]);
        Ok(())
    }

    #[test]
    fn todo_fold_skips_unknown_state_record() -> Result<(), Box<dyn StdError>> {
        let records = [
            record(vec![item("kept", TodoState::Pending)])?,
            Box::new(RawJson::parse(
                r#"{"list":[{"subject":"bad","description":"","state":"waiting"}]}"#,
            )?),
        ];

        assert_eq!(fold(&records), vec![item("kept", TodoState::Pending)]);
        Ok(())
    }

    #[test]
    fn todo_fold_matches_an_independent_leaf_oracle() -> Result<(), Box<dyn StdError>> {
        let mut leaves = [Vec::new(), Vec::new()];
        for index in 0..64 {
            let state = match index % 4 {
                0 => TodoState::Pending,
                1 => TodoState::InProgress,
                2 => TodoState::Done,
                _ => TodoState::Cancelled,
            };
            let leaf = index % leaves.len();
            leaves[leaf].push(record(vec![item(&format!("task-{index}"), state)])?);
            if index % 7 == 0 {
                leaves[leaf].push(Box::new(RawJson::parse(r#"{"list":{}}"#)?));
            }
            for records in &leaves {
                assert_eq!(fold(records), fold_oracle(records));
            }
        }
        Ok(())
    }

    #[test]
    fn todo_validation_rejects_empty_list() {
        assert_eq!(
            error_text(validate(&[])),
            "invalid todo list: 1 to 26 items"
        );
    }

    #[test]
    fn todo_validation_rejects_more_than_26_items() {
        let items = (0..27)
            .map(|index| item(&format!("item {index}"), TodoState::Pending))
            .collect::<Vec<_>>();

        assert_eq!(
            error_text(validate(&items)),
            "invalid todo list: 1 to 26 items"
        );
    }

    #[test]
    fn todo_validation_accepts_26_items() {
        let items = (0..MAX_ITEMS)
            .map(|index| item(&format!("item {index}"), TodoState::Pending))
            .collect::<Vec<_>>();

        assert!(validate(&items).is_ok());
    }

    #[test]
    fn todo_validation_rejects_two_in_progress_items() {
        let items = vec![
            item("first", TodoState::InProgress),
            item("second", TodoState::InProgress),
        ];

        assert_eq!(
            error_text(validate(&items)),
            "invalid todo list: at most one in_progress"
        );
    }

    #[test]
    fn todo_validation_trims_subject_before_storage() {
        let args = TodoArgs {
            action: "write".to_owned(),
            todos: TodosField::List(vec![TodoItemInput {
                subject: format!("  {}  ", "é".repeat(100)),
                description: String::new(),
                state: "pending".to_owned(),
            }]),
        };
        let action = parse_args(args, 16_384);

        assert!(matches!(&action, Ok(TodoAction::Write(_))));
        if let Ok(TodoAction::Write(items)) = action {
            assert_eq!(items[0].subject, "é".repeat(100));
        }
    }

    #[test]
    fn todo_validation_rejects_subject_over_200_bytes() {
        assert_eq!(
            error_text(validate(&[item(&"s".repeat(201), TodoState::Pending)])),
            "invalid todo list: subject must be 1 to 200 bytes"
        );
    }

    #[test]
    fn todo_validation_rejects_empty_trimmed_subject() {
        let args = TodoArgs {
            action: "write".to_owned(),
            todos: TodosField::List(vec![TodoItemInput {
                subject: " \t\n ".to_owned(),
                description: String::new(),
                state: "pending".to_owned(),
            }]),
        };

        assert_eq!(
            error_text(parse_args(args, 16_384)),
            "invalid todo list: subject must be 1 to 200 bytes"
        );
    }

    #[test]
    fn todo_validation_accepts_2000_description_bytes() {
        let items = [TodoItem {
            subject: "valid".to_owned(),
            description: "d".repeat(2000),
            state: TodoState::Done,
        }];

        assert!(validate(&items).is_ok());
    }

    #[test]
    fn todo_validation_rejects_description_over_2000_bytes() {
        let items = [TodoItem {
            subject: "valid".to_owned(),
            description: "d".repeat(2001),
            state: TodoState::Done,
        }];

        assert_eq!(
            error_text(validate(&items)),
            "invalid todo list: description must be at most 2000 bytes"
        );
    }

    #[test]
    fn todo_parse_bounds_unknown_state_on_utf8_boundary() {
        let args = TodoArgs {
            action: "write".to_owned(),
            todos: TodosField::List(vec![TodoItemInput {
                subject: "valid".to_owned(),
                description: String::new(),
                state: "éxx".to_owned(),
            }]),
        };

        assert_eq!(
            error_text(parse_args(args, 3)),
            "invalid todo list: unknown state \"éx\""
        );
    }

    #[test]
    fn todo_parse_requires_a_list_for_write() {
        let args = TodoArgs {
            action: "write".to_owned(),
            todos: TodosField::Absent,
        };

        assert_eq!(
            error_text(parse_args(args, 16_384)),
            "todo: action \"write\" requires \"todos\""
        );
    }

    #[test]
    fn todo_parse_rejects_todos_on_read() {
        let args = TodoArgs {
            action: "read".to_owned(),
            todos: TodosField::List(Vec::new()),
        };

        assert_eq!(
            error_text(parse_args(args, 16_384)),
            "todo: todos is not valid for action read"
        );
    }

    #[test]
    fn todo_parse_rejects_null_todos_on_read() {
        let args = TodoArgs {
            action: "read".to_owned(),
            todos: TodosField::Null,
        };

        assert_eq!(
            error_text(parse_args(args, 16_384)),
            "todo: todos is not valid for action read"
        );
    }

    #[test]
    fn todo_parse_rejects_null_todos_on_write() {
        let args = TodoArgs {
            action: "write".to_owned(),
            todos: TodosField::Null,
        };

        assert_eq!(
            error_text(parse_args(args, 16_384)),
            "todo: todos must be an array for action write"
        );
    }

    #[test]
    fn todo_decode_absent_description_defaults_to_empty() {
        let raw = r#"{"action":"write","todos":[{"subject":"s","state":"pending"}]}"#;
        let args = sonic_rs::from_str::<TodoArgs>(raw);

        assert!(args.is_ok());
        if let Ok(args) = args {
            let action = parse_args(args, 16_384);
            assert!(matches!(&action, Ok(TodoAction::Write(_))));
            if let Ok(TodoAction::Write(items)) = action {
                assert_eq!(items[0].description, "");
            }
        }
    }

    #[test]
    fn todo_decode_null_description_fails() {
        let raw =
            r#"{"action":"write","todos":[{"subject":"s","description":null,"state":"pending"}]}"#;

        assert!(sonic_rs::from_str::<TodoArgs>(raw).is_err());
    }

    #[test]
    fn todo_decode_unknown_state_reaches_parser() {
        let raw = r#"{"action":"write","todos":[{"subject":"s","state":"waiting"}]}"#;
        let args = sonic_rs::from_str::<TodoArgs>(raw);

        assert!(args.is_ok());
        if let Ok(args) = args {
            assert_eq!(
                error_text(parse_args(args, 16_384)),
                "invalid todo list: unknown state \"waiting\""
            );
        }
    }

    #[test]
    fn todo_decode_null_todos_on_read_is_rejected() {
        let raw = r#"{"action":"read","todos":null}"#;
        let args = sonic_rs::from_str::<TodoArgs>(raw);

        assert!(args.is_ok());
        if let Ok(args) = args {
            assert_eq!(
                error_text(parse_args(args, 16_384)),
                "todo: todos is not valid for action read"
            );
        }
    }

    #[test]
    fn todo_render_matches_every_state() {
        let items = [
            item("Fix the parser", TodoState::InProgress),
            item("Add tests", TodoState::Pending),
            item("Write the plan", TodoState::Done),
            item("Drop the old flag", TodoState::Cancelled),
        ];

        assert_eq!(
            render(&items),
            "- [ ] Fix the parser (in progress)\n- [ ] Add tests\n- [x] Write the plan\n- [-] Drop the old flag (cancelled)"
        );
    }

    #[test]
    fn todo_render_empty_list_is_empty() {
        assert_eq!(render(&[]), "");
    }

    #[test]
    fn open_todos_returns_first_three_nonterminal_subjects() {
        let items = [
            item("one", TodoState::Pending),
            item("done", TodoState::Done),
            item("two", TodoState::InProgress),
            item("three", TodoState::Pending),
            item("four", TodoState::Pending),
        ];

        assert_eq!(
            open_todos(&items),
            (4, 5, vec!["one".into(), "two".into(), "three".into()])
        );
    }

    #[test]
    fn todo_all_terminal_includes_empty_list() {
        assert!(todo_all_terminal(&[]));
    }

    #[test]
    fn todo_all_terminal_includes_done_and_cancelled_items() {
        let closed = [
            item("done", TodoState::Done),
            item("cancelled", TodoState::Cancelled),
        ];

        assert!(todo_all_terminal(&closed));
    }

    #[test]
    fn todo_all_terminal_rejects_open_items() {
        let items = [item("open", TodoState::InProgress)];

        assert!(!todo_all_terminal(&items));
    }
}

#[cfg(test)]
mod host_tests {
    use super::super::support::{ScriptedWorkHost, TestResult};

    fn write_args(items: &[(&str, &str, &str)]) -> Result<String, sonic_rs::Error> {
        let mut encoded = Vec::new();
        for (subject, description, state) in items {
            encoded.push(format!(
                r#"{{"subject":{},"description":{},"state":{}}}"#,
                sonic_rs::to_string(subject)?,
                sonic_rs::to_string(description)?,
                sonic_rs::to_string(state)?
            ));
        }
        Ok(format!(
            r#"{{"action":"write","todos":[{}]}}"#,
            encoded.join(",")
        ))
    }

    #[tokio::test]
    async fn todo_write_read() -> TestResult {
        let host = ScriptedWorkHost::open();
        let args = r#"{"action":"write","todos":[{"subject":"  Fix the parser  ","state":"in_progress"},{"subject":"Add tests","description":"cover fold","state":"pending"},{"subject":"Write the plan","state":"done"}]}"#;

        let written = host.tool("todo", args).await?;
        assert_eq!(
            written,
            "- [ ] Fix the parser (in progress)\n- [ ] Add tests\n- [x] Write the plan\nTodo list updated."
        );

        let bodies = host.services.all_bodies("todo");
        assert_eq!(
            bodies,
            [
                r#"{"list":[{"subject":"Fix the parser","description":"","state":"in_progress"},{"subject":"Add tests","description":"cover fold","state":"pending"},{"subject":"Write the plan","description":"","state":"done"}]}"#
            ]
        );

        let read = host.tool("todo", r#"{"action":"read"}"#).await?;
        assert_eq!(
            read,
            "- [ ] Fix the parser (in progress)\n- [ ] Add tests\n- [x] Write the plan"
        );

        let (quiet, status) = host.status();
        assert!(quiet);
        assert_eq!(status.as_deref(), Some("1/3 done · Fix the parser"));
        Ok(())
    }

    #[tokio::test]
    async fn todo_validation() -> TestResult {
        let host = ScriptedWorkHost::open();
        let padded = format!("  {}  ", "a".repeat(201));
        let long_description = "d".repeat(2001);
        let twenty_seven: Vec<(&str, &str, &str)> = vec![("x", "", "pending"); 27];
        let cases = [
            (
                write_args(&[("a", "", "in_progress"), ("b", "", "in_progress")])?,
                "invalid todo list: at most one in_progress",
            ),
            (
                write_args(&twenty_seven)?,
                "invalid todo list: 1 to 26 items",
            ),
            (
                write_args(&[(padded.as_str(), "", "pending")])?,
                "invalid todo list: subject must be 1 to 200 bytes",
            ),
            (
                write_args(&[("a", long_description.as_str(), "pending")])?,
                "invalid todo list: description must be at most 2000 bytes",
            ),
            (
                write_args(&[("a", "", "waiting")])?,
                "invalid todo list: unknown state \"waiting\"",
            ),
            (
                r#"{"action":"write","todos":[]}"#.to_owned(),
                "invalid todo list: 1 to 26 items",
            ),
        ];
        for (args, expected) in cases {
            assert_eq!(host.tool("todo", &args).await?, expected);
            assert_eq!(host.services.all_bodies("todo").len(), 0);
        }

        let boundary = write_args(&[(&"a".repeat(200), &"d".repeat(2000), "pending")])?;
        assert_eq!(
            host.tool("todo", &boundary).await?,
            format!("- [ ] {}\nTodo list updated.", "a".repeat(200))
        );
        assert_eq!(host.services.all_bodies("todo").len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn todo_follows_leaf() -> TestResult {
        let host = ScriptedWorkHost::open();
        host.todo_tool(&write_args(&[("first", "", "pending")])?)
            .await;
        host.todo_tool(&write_args(&[("second", "", "pending")])?)
            .await;
        assert_eq!(host.todos_command("").await?, "- [ ] second");

        host.services.set_leaf(Some(0));
        assert_eq!(host.todos_command("").await?, "- [ ] first");

        host.services.set_leaf(None);
        assert_eq!(host.todos_command("").await?, "No todo list.");

        host.services.set_leaf(Some(0));
        host.todo_tool(&write_args(&[("branch", "", "done")])?)
            .await;
        assert_eq!(host.todos_command("").await?, "- [x] branch");

        host.services.set_leaf(Some(1));
        assert_eq!(host.todos_command("").await?, "- [ ] second");
        Ok(())
    }

    #[tokio::test]
    async fn todo_ephemeral_session() -> TestResult {
        let host = ScriptedWorkHost::open();
        host.tool("todo", &write_args(&[("only", "", "pending")])?)
            .await?;
        host.tool("todo", r#"{"action":"read"}"#).await?;
        host.todos_command("").await?;
        host.scheme("current").await?;
        host.scheme("terminal").await?;

        assert_eq!(host.services.side_calls(), 0);
        assert_eq!(host.services.all_bodies("todo").len(), 1);
        Ok(())
    }

    fn next(seed: &mut u64) -> usize {
        *seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from(*seed >> 40).unwrap_or(0)
    }

    fn rendered(items: &[(&str, &str)]) -> String {
        let lines: Vec<String> = items
            .iter()
            .map(|(subject, state)| match *state {
                "done" => format!("- [x] {subject}"),
                "cancelled" => format!("- [-] {subject} (cancelled)"),
                "in_progress" => format!("- [ ] {subject} (in progress)"),
                _ => format!("- [ ] {subject}"),
            })
            .collect();
        lines.join("\n")
    }

    #[tokio::test]
    async fn todo_fold_property() -> TestResult {
        let subjects = ["alpha", "beta", "gamma", "delta"];
        let states = ["pending", "done", "cancelled", "in_progress"];
        let host = ScriptedWorkHost::open();
        let mut seed = 7_u64;
        let mut entries: Vec<(Option<usize>, Option<String>)> = Vec::new();
        let mut leaf: Option<usize> = None;
        for _ in 0..80 {
            match next(&mut seed) % 4 {
                0 | 1 => {
                    let count = 1 + next(&mut seed) % 3;
                    let mut items = Vec::new();
                    let mut running = false;
                    for _ in 0..count {
                        let subject = subjects[next(&mut seed) % subjects.len()];
                        let mut state = states[next(&mut seed) % states.len()];
                        if state == "in_progress" && running {
                            state = "pending";
                        }
                        running |= state == "in_progress";
                        items.push((subject, state));
                    }
                    let args: Vec<(&str, &str, &str)> =
                        items.iter().map(|(s, st)| (*s, "", *st)).collect();
                    host.tool("todo", &write_args(&args)?).await?;
                    entries.push((leaf, Some(rendered(&items))));
                    leaf = Some(entries.len() - 1);
                }
                2 => {
                    let body = if next(&mut seed).is_multiple_of(2) {
                        r#"{"list":{}}"#
                    } else {
                        r#"{"list":[{"subject":"bad","description":"","state":"waiting"}]}"#
                    };
                    host.services.push_raw("todo", body)?;
                    entries.push((leaf, None));
                    leaf = Some(entries.len() - 1);
                }
                _ if entries.is_empty() => {}
                _ => {
                    let pick = next(&mut seed) % (entries.len() + 1);
                    leaf = pick.checked_sub(1);
                    host.services.set_leaf(leaf);
                }
            }
            let mut expected = "No todo list.".to_owned();
            let mut cursor = leaf;
            while let Some(index) = cursor {
                if let (_, Some(text)) = &entries[index] {
                    expected.clone_from(text);
                    break;
                }
                cursor = entries[index].0;
            }
            assert_eq!(host.tool("todo", r#"{"action":"read"}"#).await?, expected);
        }
        Ok(())
    }
}
