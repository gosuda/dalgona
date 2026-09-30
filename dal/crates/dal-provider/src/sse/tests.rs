use futures::{StreamExt, executor::block_on, stream};
use proptest::{arbitrary::any, collection::vec, strategy::Strategy};

use super::*;

const BASIC_TEXT_STREAM: &str = concat!(
    "event: message_start\n",
    "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_\u{2026}\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-opus-5-5\",\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":25,\"output_tokens\":1}}}\n",
    "\n",
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
    "\n",
    "event: ping\n",
    "data: {\"type\":\"ping\"}\n",
    "\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n",
    "\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"!\"}}\n",
    "\n",
    "event: content_block_stop\n",
    "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
    "\n",
    "event: message_delta\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":15}}\n",
    "\n",
    "event: message_stop\n",
    "data: {\"type\":\"message_stop\"}\n",
);

fn decode_texts(chunks: Vec<Vec<u8>>) -> Vec<Result<SseEvent, String>> {
    block_on(decode_stream(stream::iter(chunks)).collect::<Vec<_>>())
        .into_iter()
        .map(|item| match item {
            Ok(event) => Ok(event),
            Err(error) => Err(error.to_string()),
        })
        .collect()
}

fn decode_frames(chunks: Vec<Vec<u8>>) -> Vec<SseEvent> {
    decode_texts(chunks)
        .into_iter()
        .map(Result::unwrap)
        .collect()
}

fn frame(name: Option<&str>, data: &str) -> SseEvent {
    SseEvent {
        name: name.map(String::from),
        data: String::from(data),
    }
}

fn wire_type(data: &str) -> &str {
    let start = data.find("\"type\":\"").expect("type member") + 8;
    let rest = &data[start..];
    &rest[..rest.find('"').expect("type value end")]
}

fn comment_line(length: usize) -> Vec<u8> {
    let mut line = vec![b':'];
    line.extend(std::iter::repeat_n(b'x', length - 1));
    line.push(b'\n');
    line
}

fn short_line_event(joined_len: usize) -> Vec<u8> {
    let mut wire = Vec::new();
    let mut joined = 0;
    while joined < joined_len {
        let separator = usize::from(joined > 0);
        let value = (joined_len - joined - separator).min(9);
        wire.extend_from_slice(b"data: ");
        wire.extend(std::iter::repeat_n(b'x', value));
        wire.push(b'\n');
        joined += value + separator;
    }
    wire
}

fn chunked(bytes: &[u8], cuts: &[usize]) -> Vec<Vec<u8>> {
    let mut points: Vec<usize> = cuts.iter().map(|cut| cut % (bytes.len() + 1)).collect();
    points.sort_unstable();
    let mut chunks = Vec::new();
    let mut previous = 0;
    for point in points {
        chunks.push(bytes[previous..point].to_vec());
        previous = point;
    }
    chunks.push(bytes[previous..].to_vec());
    chunks
}

fn name_strategy() -> impl Strategy<Value = String> {
    any::<String>().prop_filter("name has no line break", |name| {
        !name.contains('\r') && !name.contains('\n')
    })
}

fn event_strategy() -> impl Strategy<Value = SseEvent> {
    let data = any::<String>()
        .prop_filter("data has no carriage return", |data| !data.contains('\r'))
        .prop_map(|data| {
            if data.is_empty() {
                String::from("x")
            } else {
                data
            }
        });
    (proptest::option::of(name_strategy()), data).prop_map(|(name, data)| SseEvent { name, data })
}

#[test]
fn sse_parser() {
    let frames = decode_frames(vec![BASIC_TEXT_STREAM.as_bytes().to_vec()]);
    assert_eq!(frames.len(), 8);
    let names: Vec<Option<&str>> = frames.iter().map(|event| event.name.as_deref()).collect();
    assert_eq!(
        names,
        vec![
            Some("message_start"),
            Some("content_block_start"),
            Some("ping"),
            Some("content_block_delta"),
            Some("content_block_delta"),
            Some("content_block_stop"),
            Some("message_delta"),
            Some("message_stop"),
        ]
    );
    let types: Vec<&str> = frames.iter().map(|event| wire_type(&event.data)).collect();
    assert_eq!(
        types,
        vec![
            "message_start",
            "content_block_start",
            "ping",
            "content_block_delta",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop",
        ]
    );
}

#[test]
fn sse_line_endings() {
    let expected = decode_frames(vec![BASIC_TEXT_STREAM.as_bytes().to_vec()]);
    let streams = [
        BASIC_TEXT_STREAM.replace('\n', "\r\n"),
        BASIC_TEXT_STREAM.replace('\n', "\r"),
        format!("\u{feff}{BASIC_TEXT_STREAM}"),
        format!("\u{feff}{}", BASIC_TEXT_STREAM.replace('\n', "\r\n")),
    ];
    for stream in streams {
        assert_eq!(
            decode_frames(vec![stream.as_bytes().to_vec()]),
            expected,
            "{stream:?}"
        );
    }
}

#[test]
fn sse_end_of_input() {
    assert_eq!(
        decode_frames(vec![
            b"event: message_stop\ndata: {\"type\":\"message_stop\"}".to_vec()
        ]),
        vec![frame(Some("message_stop"), "{\"type\":\"message_stop\"}")]
    );
    assert_eq!(
        decode_frames(vec![b"data: x".to_vec()]),
        vec![frame(None, "x")]
    );
    assert_eq!(
        decode_frames(vec![b"data: x\r".to_vec()]),
        vec![frame(None, "x")]
    );
    assert!(decode_frames(vec![b"event: ping".to_vec()]).is_empty());
}

#[test]
fn sse_size_limits() {
    let mut over_line = comment_line(SSE_LINE_LIMIT + 1);
    over_line.extend_from_slice(b"data: later\n\n");
    assert_eq!(
        decode_texts(vec![over_line]),
        vec![Err(String::from("SSE line exceeds 1 MiB."))]
    );
    let mut over_event = short_line_event(SSE_EVENT_LIMIT + 1);
    over_event.extend_from_slice(b"data: later\n\n");
    assert_eq!(
        decode_texts(vec![over_event]),
        vec![Err(String::from("SSE event exceeds 8 MiB."))]
    );
}

#[test]
fn size_limits_are_inclusive() {
    let mut at_line_limit = comment_line(SSE_LINE_LIMIT);
    at_line_limit.extend_from_slice(b"data: x\n\n");
    assert_eq!(decode_frames(vec![at_line_limit]), vec![frame(None, "x")]);
    let mut at_event_limit = short_line_event(SSE_EVENT_LIMIT);
    at_event_limit.push(b'\n');
    let frames = decode_frames(vec![at_event_limit]);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].data.len(), SSE_EVENT_LIMIT);
}

#[test]
fn limit_failure_ends_the_stream_after_one_error() {
    let mut wire = comment_line(SSE_LINE_LIMIT + 1);
    wire.extend_from_slice(b"data: later\n\n");
    let chunks: Vec<Vec<u8>> = wire.chunks(64).map(<[u8]>::to_vec).collect();
    assert_eq!(
        decode_texts(chunks),
        vec![Err(String::from("SSE line exceeds 1 MiB."))]
    );
}

#[test]
fn framing_fields_and_comments_keep_exact_values() {
    let wire = concat!(
        ": keep-alive\n",
        ":no space\n",
        "id: 7\n",
        "retry: 100\n",
        "unknown: value\n",
        "data: a:b\n",
        "data:  two spaces kept\n",
        "data:\ttab kept\n",
        "data:\n",
        "data: x\n",
        "\n",
    );
    assert_eq!(
        decode_frames(vec![wire.as_bytes().to_vec()]),
        vec![frame(None, "a:b\n two spaces kept\n\ttab kept\n\nx")]
    );
}

#[test]
fn empty_data_semantics() {
    assert!(decode_frames(vec![b"data:\n\n".to_vec()]).is_empty());
    assert!(decode_frames(vec![b"data: \n\n".to_vec()]).is_empty());
    assert!(decode_frames(vec![b"event: ping\n\n".to_vec()]).is_empty());
    assert!(decode_frames(vec![b": comment\n\n".to_vec()]).is_empty());
    assert_eq!(
        decode_frames(vec![b"data:  \n\n".to_vec()]),
        vec![frame(None, " ")]
    );
    assert_eq!(
        decode_frames(vec![b"data:\ndata: x\n\n".to_vec()]),
        vec![frame(None, "\nx")]
    );
    assert_eq!(
        decode_frames(vec![b"data: x\ndata:\n\n".to_vec()]),
        vec![frame(None, "x\n")]
    );
    assert_eq!(
        decode_frames(vec![b"data:\ndata:\n\n".to_vec()]),
        vec![frame(None, "\n")]
    );
    assert_eq!(
        decode_frames(vec![b"event: a\n\ndata: x\n\n".to_vec()]),
        vec![frame(None, "x")]
    );
}

#[test]
fn event_names_are_set_and_reset() {
    let wire = concat!(
        "event: a\ndata: 1\n\n",
        "data: 2\n\n",
        "event:\ndata: 3\n\n",
        "event: d\ndata: 4\ndata: 5\n\n",
    );
    assert_eq!(
        decode_frames(vec![wire.as_bytes().to_vec()]),
        vec![
            frame(Some("a"), "1"),
            frame(None, "2"),
            frame(Some(""), "3"),
            frame(Some("d"), "4\n5"),
        ]
    );
}

#[test]
fn one_leading_byte_order_mark_is_stripped() {
    let marked = "\u{feff}data: \u{feff}x\n\ndata: y\u{feff}\n\n".to_owned();
    assert_eq!(
        decode_frames(vec![marked.as_bytes().to_vec()]),
        vec![frame(None, "\u{feff}x"), frame(None, "y\u{feff}")]
    );
    let prefixed = "data: x\n\n\u{feff}data: y\n\n".to_owned();
    assert_eq!(
        decode_frames(vec![prefixed.as_bytes().to_vec()]),
        vec![frame(None, "x")]
    );
    assert!(decode_frames(vec![vec![0xEF, 0xBB, 0xBF]]).is_empty());
    assert!(decode_frames(vec![vec![0xEF, 0xBB]]).is_empty());
}

#[test]
fn every_chunk_boundary_and_multibyte_split() {
    let wire = "\u{feff}event: ping\ndata: {\"x\":\"\u{e9}\"}\r\n\r\ndata: a\r\ndata: \u{fc}n";
    let expected = decode_frames(vec![wire.as_bytes().to_vec()]);
    assert_eq!(
        expected,
        vec![
            frame(Some("ping"), "{\"x\":\"\u{e9}\"}"),
            frame(None, "a\n\u{fc}n"),
        ]
    );
    let bytes = wire.as_bytes();
    for at in 0..=bytes.len() {
        let chunks = vec![bytes[..at].to_vec(), bytes[at..].to_vec()];
        assert_eq!(decode_frames(chunks), expected, "split at {at}");
    }
    let one_byte: Vec<Vec<u8>> = bytes.iter().map(|byte| vec![*byte]).collect();
    assert_eq!(decode_frames(one_byte), expected);
}

#[test]
fn invalid_utf8_becomes_replacement_characters() {
    let mut wire = b"data: ".to_vec();
    wire.extend([0xFF, 0xFE]);
    wire.extend_from_slice(b"\n\ndata: tail ");
    wire.push(0xC3);
    assert_eq!(
        decode_frames(vec![wire]),
        vec![
            frame(None, "\u{FFFD}\u{FFFD}"),
            frame(None, "tail \u{FFFD}"),
        ]
    );
}

#[test]
fn encode_writes_the_canonical_framing() {
    let events = vec![
        SseEvent {
            name: Some(String::from("message_start")),
            data: String::from("{\"type\":\"message_start\"}"),
        },
        SseEvent {
            name: None,
            data: String::from("a\nb"),
        },
        SseEvent {
            name: Some(String::new()),
            data: String::from(" x"),
        },
    ];
    assert_eq!(
        encode(&events),
        concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\"}\n",
            "\n",
            "data: a\n",
            "data: b\n",
            "\n",
            "event: \n",
            "data:  x\n",
            "\n",
        )
        .as_bytes()
    );
}

proptest::proptest! {
    #[test]
    fn sse_roundtrip_property(
        events in vec(event_strategy(), 0..8),
        cuts in vec(any::<usize>(), 0..24),
    ) {
        let wire = encode(&events);
        let frames = decode_frames(chunked(&wire, &cuts));
        proptest::prop_assert_eq!(frames, events);
    }
}
