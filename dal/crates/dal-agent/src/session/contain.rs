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

/// Reads the text of one panic payload.
fn panic_text(payload: &(dyn Any + Send)) -> Box<str> {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).into()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.as_str().into()
    } else {
        "no message".into()
    }
}

#[cfg(test)]
mod tests {
    use super::contained;

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
}
