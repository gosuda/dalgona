//! Panic containment for extension code the session driver awaits.
//!
//! The driver is one task per session. A panic inside a tool or a slash
//! handler would end that task and leave every waiting client without an
//! answer, so the driver turns the panic into an ordinary error value at the
//! call site instead.

use std::any::Any;
use std::panic::AssertUnwindSafe;

use futures::FutureExt;

/// Awaits `future`, returning the panic message when it panics.
///
/// Callers hold no lock across the awaited future that a panic could leave
/// inconsistent, so unwinding past it is safe to continue from.
pub(crate) async fn contained<T>(future: impl Future<Output = T>) -> Result<T, Box<str>> {
    AssertUnwindSafe(future)
        .catch_unwind()
        .await
        .map_err(|payload| panic_text(payload.as_ref()))
}

/// Longest panic text kept for the model, in bytes, marker included.
const MAX_PANIC_TEXT: usize = 2048;
/// Marks a panic payload cut to [`MAX_PANIC_TEXT`].
const TRUNCATED_MARKER: &str = "…[truncated]";

/// Builds the tool-result text for a tool that panicked mid-call.
///
/// The message names the tool and carries the bounded panic text, so the
/// model and the client both see why the call produced no result.
pub(crate) fn crashed_tool(name: impl std::fmt::Display, panic: &str) -> String {
    format!(
        "The {name} tool crashed and did not finish: {panic}. Report this to the tool's author, or try a different approach."
    )
}

/// Reads the text of one panic payload, bounded for the model context.
fn panic_text(payload: &(dyn Any + Send)) -> Box<str> {
    if let Some(text) = payload.downcast_ref::<&str>() {
        bound_text(text)
    } else if let Some(text) = payload.downcast_ref::<String>() {
        bound_text(text)
    } else {
        "no message".into()
    }
}

/// Cuts `text` to [`MAX_PANIC_TEXT`] bytes on a char boundary, marking the cut.
///
/// The marker bytes come out of the same budget, so the result never exceeds
/// the bound.
fn bound_text(text: &str) -> Box<str> {
    if text.len() <= MAX_PANIC_TEXT {
        return text.into();
    }
    let mut end = MAX_PANIC_TEXT.saturating_sub(TRUNCATED_MARKER.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATED_MARKER}", &text[..end]).into()
}

#[cfg(test)]
mod tests {
    use super::{contained, crashed_tool};

    #[tokio::test]
    async fn returns_the_output_of_a_future_that_completes() {
        assert_eq!(contained(async { 7 }).await, Ok(7));
    }

    #[tokio::test]
    async fn returns_the_message_of_a_str_panic() {
        let result = contained::<u8>(async { panic!("static text") }).await;
        assert_eq!(result, Err("static text".into()));
    }

    #[tokio::test]
    async fn returns_the_message_of_a_formatted_panic() {
        let code = 3;
        let result = contained::<u8>(async move { panic!("code {code}") }).await;
        assert_eq!(result, Err("code 3".into()));
    }

    #[tokio::test]
    async fn names_a_payload_that_is_not_text() {
        let result = contained::<u8>(async { std::panic::panic_any(5_u8) }).await;
        assert_eq!(result, Err("no message".into()));
    }

    #[tokio::test]
    async fn cuts_a_long_panic_on_a_char_boundary_with_a_marker() {
        // The cut budget lands inside the two-byte `é`, so the kept head must
        // back off to a char boundary and the whole result must fit 2 KiB.
        let long = format!("{}é{}", "a".repeat(2033), "b".repeat(100));
        let result = contained::<u8>(async move { panic!("{long}") }).await;
        let Err(text) = result else {
            panic!("a panicking future reports its text");
        };
        assert!(
            text.ends_with("…[truncated]"),
            "the cut carries its marker: {text}"
        );
        assert!(
            text.len() <= 2048,
            "marker included, the kept text stays within the bound: {}",
            text.len()
        );
        assert!(
            text.starts_with(&"a".repeat(2033)),
            "the head survives the cut"
        );
    }

    #[tokio::test]
    async fn keeps_a_short_panic_whole() {
        let result = contained::<u8>(async { panic!("small") }).await;
        assert_eq!(result, Err("small".into()));
    }

    #[test]
    fn crashed_tool_names_the_tool_and_carries_the_panic() {
        assert_eq!(
            crashed_tool("explode", "tool state was corrupt"),
            "The explode tool crashed and did not finish: tool state was corrupt. \
             Report this to the tool's author, or try a different approach."
        );
    }
}
