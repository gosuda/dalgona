use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use futures::{channel::mpsc, executor::block_on, stream};

use super::*;

fn text(text: &str) -> StreamEvent {
    StreamEvent::TextDelta { text: text.into() }
}

fn stop() -> StreamEvent {
    StreamEvent::Stop {
        reason: StopReason::EndTurn,
    }
}

fn zero_usage() -> StreamEvent {
    StreamEvent::Usage {
        usage: Usage {
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            cost_usd: None,
        },
    }
}

fn counter() -> (Arc<AtomicUsize>, impl FnOnce() + Send + 'static) {
    let count = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&count);
    (count, move || {
        seen.fetch_add(1, Ordering::SeqCst);
    })
}

fn from_items(items: Vec<Result<StreamEvent, ProviderError>>) -> (EventStream, Arc<AtomicUsize>) {
    let (count, cancel) = counter();
    (EventStream::new(stream::iter(items), cancel), count)
}

#[test]
fn async_producer_is_delivered_in_order_and_stop_keeps_the_transport() {
    let (sender, receiver) = mpsc::unbounded();
    let (count, cancel) = counter();
    let mut events = EventStream::new(receiver, cancel);
    let script = vec![
        text("Hel"),
        text("lo"),
        StreamEvent::ToolCallsDone { calls: Vec::new() },
        zero_usage(),
        stop(),
    ];
    let producer = {
        let script = script.clone();
        async move {
            for event in script {
                // Yield between sends so the consumer observes Pending;
                // the self-wake keeps the single-threaded executor going.
                let mut yielded = false;
                poll_fn(|cx| {
                    if yielded {
                        return Poll::Ready(());
                    }
                    yielded = true;
                    cx.waker().wake_by_ref();
                    Poll::Pending
                })
                .await;
                sender.unbounded_send(Ok(event)).unwrap();
            }
            // The sender stays alive: a healthy connection after Stop.
            sender
        }
    };
    let consumer = async {
        let mut seen = Vec::new();
        while let Some(item) = events.next().await {
            seen.push(item.unwrap());
        }
        seen
    };
    let (_sender, seen) = block_on(async { futures::join!(producer, consumer) });
    assert_eq!(seen, script);
    drop(events);
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
#[cfg_attr(debug_assertions, should_panic(expected = "after its terminal event"))]
fn second_stop_is_never_delivered() {
    let (mut events, _count) = from_items(vec![Ok(stop()), Ok(stop())]);
    block_on(async {
        assert_eq!(events.next().await.unwrap().unwrap(), stop());
        assert!(events.next().await.is_none());
        assert!(events.next().await.is_none());
    });
}

#[test]
fn stop_after_error_is_unreachable() {
    // An error drops the source at once, so a second terminal queued
    // behind it is never polled, in debug and release alike.
    let (mut events, count) = from_items(vec![Err(ProviderError::Overloaded), Ok(stop())]);
    block_on(async {
        assert!(matches!(
            events.next().await,
            Some(Err(ProviderError::Overloaded))
        ));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(events.next().await.is_none());
        assert!(events.next().await.is_none());
    });
    drop(events);
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn error_is_the_terminal_and_cancels_once() {
    let (mut events, count) = from_items(vec![
        Ok(text("a")),
        Err(ProviderError::Protocol {
            family: Family::Anthropic,
            detail: "content block index went backwards".into(),
        }),
    ]);
    block_on(async {
        assert_eq!(events.next().await.unwrap().unwrap(), text("a"));
        assert_eq!(count.load(Ordering::SeqCst), 0);
        let error = events.next().await.unwrap().unwrap_err();
        assert!(matches!(error, ProviderError::Protocol { .. }));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(events.next().await.is_none());
        assert!(events.next().await.is_none());
    });
    drop(events);
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn source_end_without_terminal_is_a_stream_cut() {
    let (mut events, count) = from_items(vec![Ok(text("a"))]);
    block_on(async {
        assert_eq!(events.next().await.unwrap().unwrap(), text("a"));
        assert!(matches!(
            events.next().await,
            Some(Err(ProviderError::StreamCut))
        ));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(events.next().await.is_none());
    });
    drop(events);
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn dropping_before_the_terminal_drops_the_source_then_cancels_once() {
    struct Socket(Arc<AtomicBool>);
    impl Drop for Socket {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let closed = Arc::new(AtomicBool::new(false));
    let socket = Socket(Arc::clone(&closed));
    let stalled = stream::poll_fn(move |_| {
        let _open = &socket;
        Poll::<Option<Result<StreamEvent, ProviderError>>>::Pending
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let closed_first = Arc::new(AtomicBool::new(false));
    let cancel = {
        let calls = Arc::clone(&calls);
        let closed = Arc::clone(&closed);
        let closed_first = Arc::clone(&closed_first);
        move || {
            calls.fetch_add(1, Ordering::SeqCst);
            closed_first.store(closed.load(Ordering::SeqCst), Ordering::SeqCst);
        }
    };
    let mut events = EventStream::new(stream::iter([Ok(text("first"))]).chain(stalled), cancel);
    assert_eq!(block_on(events.next()).unwrap().unwrap(), text("first"));
    assert!(events.next().now_or_never().is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    drop(events);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(closed_first.load(Ordering::SeqCst));
}

#[test]
fn argument_fragments_reassemble_byte_for_byte() {
    let args = "{ \"b\" : 1.50E+2, \"a\":[ ], \"s\":\"caf\u{e9}\" }";
    let bytes = args.as_bytes();
    // Split inside the two-byte UTF-8 sequence of the accented letter.
    let split = args.find('\u{e9}').unwrap() + 1;
    let fragments = [&bytes[..7], &bytes[7..split], &bytes[split..]];
    let mut items: Vec<_> = fragments
        .iter()
        .map(|fragment| {
            Ok(StreamEvent::ToolArgsDelta {
                id: "call_1".into(),
                fragment: fragment.to_vec(),
            })
        })
        .collect();
    items.push(Ok(stop()));
    let (mut events, _count) = from_items(items);
    let mut assembled = Vec::new();
    block_on(async {
        while let Some(item) = events.next().await {
            if let StreamEvent::ToolArgsDelta { fragment, .. } = item.unwrap() {
                assembled.extend_from_slice(&fragment);
            }
        }
    });
    assert_eq!(assembled, bytes);
    match ToolArgs::from_bytes(&assembled) {
        ToolArgs::Parsed(raw) => assert_eq!(raw.as_str(), args),
        other => panic!("expected parsed arguments, got {other:?}"),
    }
}

#[test]
fn invalid_argument_bytes_are_reported_not_repaired() {
    for bytes in [&b""[..], b"{\"a\":", b"{} {}", b"\"\xff\""] {
        assert!(
            matches!(ToolArgs::from_bytes(bytes), ToolArgs::Invalid { .. }),
            "{bytes:?} must be invalid"
        );
    }
}
