use futures::{Stream, StreamExt, executor::block_on};

use super::*;

/// CRC-32 (IEEE) over `bytes`, the encoder side of the decoder's check.
fn crc(bytes: &[u8]) -> u32 {
    let mut hasher = Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

/// Builds one wire frame: prelude, string headers, payload, and both CRCs.
fn frame(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
    let mut header_block = Vec::new();
    for (name, value) in headers {
        header_block.push(u8::try_from(name.len()).unwrap_or(u8::MAX));
        header_block.extend_from_slice(name.as_bytes());
        header_block.push(7);
        header_block
            .extend_from_slice(&u16::try_from(value.len()).unwrap_or(u16::MAX).to_be_bytes());
        header_block.extend_from_slice(value.as_bytes());
    }
    let headers_len = u32::try_from(header_block.len()).unwrap_or(u32::MAX);
    let total =
        u32::try_from(PRELUDE_LEN + header_block.len() + payload.len() + 4).unwrap_or(u32::MAX);
    let mut wire = Vec::with_capacity(usize::try_from(total).unwrap_or(usize::MAX));
    wire.extend_from_slice(&total.to_be_bytes());
    wire.extend_from_slice(&headers_len.to_be_bytes());
    wire.extend_from_slice(&crc(&wire).to_be_bytes());
    wire.extend_from_slice(&header_block);
    wire.extend_from_slice(payload);
    let message_crc = crc(&wire);
    wire.extend_from_slice(&message_crc.to_be_bytes());
    wire
}

fn event_frame(event_type: &str, payload: &str) -> Vec<u8> {
    frame(
        &[
            (":message-type", "event"),
            (":event-type", event_type),
            (":content-type", "application/json"),
        ],
        payload.as_bytes(),
    )
}

fn decode_all(chunks: Vec<Vec<u8>>) -> Result<Vec<Frame>, FrameError> {
    let mut decoder = FrameDecoder::new();
    let mut frames = Vec::new();
    for chunk in chunks {
        let pushed = decoder.push(&chunk);
        frames.extend(pushed.frames);
        if let Some(failure) = pushed.failure {
            return Err(failure);
        }
    }
    decoder.finish()?;
    Ok(frames)
}

/// One wire frame around an already encoded header block.
fn raw_frame(header_block: &[u8], payload: &[u8]) -> Vec<u8> {
    let headers_len = u32::try_from(header_block.len()).unwrap_or(u32::MAX);
    let total =
        u32::try_from(PRELUDE_LEN + header_block.len() + payload.len() + 4).unwrap_or(u32::MAX);
    let mut wire = Vec::new();
    wire.extend_from_slice(&total.to_be_bytes());
    wire.extend_from_slice(&headers_len.to_be_bytes());
    wire.extend_from_slice(&crc(&wire).to_be_bytes());
    wire.extend_from_slice(header_block);
    wire.extend_from_slice(payload);
    let message_crc = crc(&wire);
    wire.extend_from_slice(&message_crc.to_be_bytes());
    wire
}

/// Rewrites both declared lengths and reseals the prelude CRC, so the
/// failure under test is the length check and not the CRC check.
fn reseal(wire: &mut [u8], total: u32, headers: u32) {
    wire[..4].copy_from_slice(&total.to_be_bytes());
    wire[4..8].copy_from_slice(&headers.to_be_bytes());
    let prelude_crc = crc(&wire[..8]).to_be_bytes();
    wire[8..12].copy_from_slice(&prelude_crc);
}

fn typed(typename: &str, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(u8::try_from(typename.len()).unwrap_or(u8::MAX));
    out.extend_from_slice(typename.as_bytes());
    out.extend_from_slice(value);
    out
}

#[test]
fn decodes_one_frame_per_message_across_chunk_boundaries() {
    let wire = event_frame("messageStart", r#"{"role":"assistant"}"#);
    for split in 1..wire.len() {
        let frames = decode_all(vec![wire[..split].to_vec(), wire[split..].to_vec()])
            .unwrap_or_else(|_| panic!("split {split} decodes"));
        assert_eq!(frames.len(), 1, "split {split} yields one frame");
        assert_eq!(frames[0].string_header(":message-type"), Some("event"));
        assert_eq!(frames[0].string_header(":event-type"), Some("messageStart"));
        assert_eq!(frames[0].payload, br#"{"role":"assistant"}"#);
    }
}

#[test]
fn decodes_many_frames_from_one_chunk_and_ignores_nothing() {
    let wire = [
        event_frame("messageStart", "{}"),
        event_frame("messageStop", r#"{"stopReason":"end_turn"}"#),
    ]
    .concat();
    let frames = decode_all(vec![wire]).unwrap_or_else(|_| panic!("two frames decode"));
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[1].string_header(":event-type"), Some("messageStop"));
}

#[test]
fn crc_mismatch_is_a_typed_failure_and_ends_the_decoder() {
    let mut wire = event_frame("metadata", "{}");
    let last = wire.len() - 1;
    wire[last] ^= 0x01;
    let mut decoder = FrameDecoder::new();
    let pushed = decoder.push(&wire);
    assert_eq!(pushed.frames, Vec::new());
    assert!(matches!(
        pushed.failure,
        Some(FrameError::Crc {
            scope: CrcScope::Message,
            ..
        })
    ));
    // A failed decoder is closed: later input yields nothing at all, not
    // the same failure again, and the end of input stays quiet.
    assert_eq!(decoder.push(&wire), Pushed::default());
    assert_eq!(decoder.finish(), Ok(()));
}

#[test]
fn prelude_crc_mismatch_fails_before_any_payload_read() {
    let mut wire = event_frame("metadata", "{}");
    wire[8] ^= 0x01;
    let error = decode_all(vec![wire])
        .err()
        .unwrap_or_else(|| panic!("prelude CRC byte fails"));
    assert!(matches!(
        error,
        FrameError::Crc {
            scope: CrcScope::Prelude,
            ..
        }
    ));
}

#[test]
fn truncation_reports_declared_and_available() {
    let wire = event_frame("metadata", "{}");
    let total = u32::try_from(wire.len()).unwrap_or(u32::MAX);
    // A cut anywhere after the prelude names the frame's own declared total
    // and the bytes that did arrive, from one byte short down to the bare
    // prelude.
    for cut in PRELUDE_LEN..wire.len() {
        let mut decoder = FrameDecoder::new();
        assert_eq!(
            decoder.push(&wire[..cut]),
            Pushed::default(),
            "an incomplete frame is neither a frame nor a failure (cut {cut})"
        );
        assert_eq!(
            decoder.finish(),
            Err(FrameError::Truncated {
                declared: total,
                available: cut,
            }),
            "cut {cut}"
        );
    }
    // A cut inside the prelude declares nothing yet: the bytes buffered stand
    // in for both numbers.
    for cut in 1..PRELUDE_LEN {
        let mut decoder = FrameDecoder::new();
        assert_eq!(decoder.push(&wire[..cut]), Pushed::default(), "cut {cut}");
        let buffered = u32::try_from(cut).unwrap_or(u32::MAX);
        assert_eq!(
            decoder.finish(),
            Err(FrameError::Truncated {
                declared: buffered,
                available: cut,
            }),
            "prelude cut {cut}"
        );
    }
    // The whole frame is not truncated.
    let mut decoder = FrameDecoder::new();
    assert_eq!(decoder.push(&wire).frames.len(), 1);
    assert_eq!(decoder.finish(), Ok(()));
}

#[test]
fn the_smallest_frame_is_sixteen_bytes_and_decodes_empty() {
    // No headers and no payload: the prelude plus the message CRC is the
    // least a frame can be. One byte fewer is refused as a length pair the
    // framing cannot produce, so this is the exact accepted edge.
    let wire = raw_frame(&[], b"");
    assert_eq!(wire.len(), MESSAGE_OVERHEAD);
    let frames = decode_all(vec![wire]).unwrap_or_else(|_| panic!("the 16-byte frame decodes"));
    assert_eq!(
        frames,
        vec![Frame {
            headers: BTreeMap::new(),
            payload: Vec::new(),
        }]
    );
}

#[test]
fn a_frame_of_exactly_the_maximum_length_decodes() {
    // The limit is on the declared total, inclusive: 8 MiB exactly is
    // accepted (one byte more is refused by the oversize test above).
    let payload_len = usize::try_from(MAX_FRAME_LEN).unwrap_or(usize::MAX) - MESSAGE_OVERHEAD;
    let wire = raw_frame(&[], &vec![b'x'; payload_len]);
    assert_eq!(wire.len(), payload_len + MESSAGE_OVERHEAD);
    let frames = decode_all(vec![wire]).unwrap_or_else(|_| panic!("a frame at the limit decodes"));
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].payload.len(), payload_len);
}

#[test]
fn a_header_block_of_exactly_the_maximum_length_decodes() {
    // The limit is on the declared header length, inclusive. Each string
    // header costs name length (1) + name (1) + type (1) + value length (2)
    // + value, so two one-letter names with these value lengths fill the
    // block to exactly MAX_HEADERS_LEN bytes.
    let max = usize::try_from(MAX_HEADERS_LEN).unwrap_or(usize::MAX);
    let first = usize::from(u16::MAX);
    let second = max - 10 - first;
    let mut block = Vec::new();
    for (name, length) in [("a", first), ("b", second)] {
        let mut value = vec![7];
        value.extend_from_slice(&u16::try_from(length).unwrap_or(u16::MAX).to_be_bytes());
        value.extend(std::iter::repeat_n(b'v', length));
        block.extend_from_slice(&typed(name, &value));
    }
    assert_eq!(block.len(), max);
    let frames = decode_all(vec![raw_frame(&block, b"p")])
        .unwrap_or_else(|_| panic!("a header block at the limit decodes"));
    assert_eq!(frames[0].payload, b"p");
    assert_eq!(
        frames[0]
            .headers
            .get("a")
            .and_then(HeaderValue::as_str)
            .map(str::len),
        Some(first)
    );
    assert_eq!(
        frames[0]
            .headers
            .get("b")
            .and_then(HeaderValue::as_str)
            .map(str::len),
        Some(second)
    );
}

#[test]
fn a_header_block_cut_anywhere_never_reads_into_the_payload() {
    // The payload is the rest of a valid header block, so a parser that read
    // past the declared block end would find the missing bytes there and
    // succeed. Cut on an entry boundary the shorter block is valid; cut
    // anywhere inside an entry it must fail at that entry's offset.
    let entries = [
        typed("bool", &[0]),
        typed("byte", &[2, 0xFB]),
        typed("short", &[3, 0x12, 0x34]),
        typed("int", &[4, 0x12, 0x34, 0x56, 0x78]),
        typed("long", &[5, 1, 2, 3, 4, 5, 6, 7, 8]),
        typed("blob", &[6, 0, 2, 0xAA, 0xBB]),
        typed("text", &[7, 0, 2, b'o', b'k']),
        typed("stamp", &[8, 0, 0, 0, 0, 0, 0, 0, 1]),
        typed(
            "uuid",
            &[9, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        ),
    ];
    let block = entries.concat();
    let mut boundaries = vec![0];
    let mut end = 0;
    for entry in &entries {
        end += entry.len();
        boundaries.push(end);
    }
    for cut in 0..=block.len() {
        let result = decode_all(vec![raw_frame(&block[..cut], &block[cut..])]);
        if let Some(complete) = boundaries.iter().position(|boundary| *boundary == cut) {
            let frames = result.unwrap_or_else(|_| panic!("cut {cut} is an entry boundary"));
            assert_eq!(frames[0].headers.len(), complete, "cut {cut}");
            assert_eq!(frames[0].payload, block[cut..], "cut {cut}");
        } else {
            let entry_start = boundaries
                .iter()
                .copied()
                .filter(|boundary| *boundary < cut)
                .max()
                .unwrap_or(0);
            assert_eq!(
                result,
                Err(FrameError::Header {
                    offset: entry_start
                }),
                "cut {cut}"
            );
        }
    }
}

/// Rewrites the declared total length and reseals the prelude CRC, so the
/// failure under test is the length check and not the CRC check.
fn reseal_total(wire: &mut [u8], total: u32) {
    wire[..4].copy_from_slice(&total.to_be_bytes());
    let prelude_crc = crc(&wire[..8]).to_be_bytes();
    wire[8..12].copy_from_slice(&prelude_crc);
}

#[test]
fn oversized_declared_lengths_are_refused() {
    let mut wire = event_frame("metadata", "{}");
    reseal_total(&mut wire, u32::MAX);
    let error = decode_all(vec![wire])
        .err()
        .unwrap_or_else(|| panic!("u32::MAX total refuses"));
    assert!(matches!(
        error,
        FrameError::FrameLimit {
            total: u32::MAX,
            ..
        }
    ));

    let mut wire = event_frame("metadata", "{}");
    reseal_total(&mut wire, MAX_FRAME_LEN + 1);
    let error = decode_all(vec![wire])
        .err()
        .unwrap_or_else(|| panic!("total over MAX_FRAME_LEN refuses"));
    assert!(matches!(error, FrameError::FrameLimit { total, .. } if total == MAX_FRAME_LEN + 1));
}

#[test]
fn a_header_value_cut_by_the_block_end_reads_no_payload_bytes() {
    // An int32 header whose four value bytes would run past the header
    // block into the payload must fail, not decode payload as header data.
    let mut block = Vec::new();
    block.extend_from_slice(&typed("n", &[4, 0x12]));
    let payload = [0x34, 0x56, 0x78, 0x9A];
    let headers_len = u32::try_from(block.len()).unwrap_or(u32::MAX);
    let mut wire = Vec::new();
    let total = u32::try_from(PRELUDE_LEN + block.len() + payload.len() + 4).unwrap_or(u32::MAX);
    wire.extend_from_slice(&total.to_be_bytes());
    wire.extend_from_slice(&headers_len.to_be_bytes());
    wire.extend_from_slice(&crc(&wire).to_be_bytes());
    wire.extend_from_slice(&block);
    wire.extend_from_slice(&payload);
    let message_crc = crc(&wire);
    wire.extend_from_slice(&message_crc.to_be_bytes());
    let error = decode_all(vec![wire])
        .err()
        .unwrap_or_else(|| panic!("a value cut by the block end refuses"));
    assert!(matches!(error, FrameError::Header { .. }));
}

#[test]
fn the_byte_stream_yields_framing_failures_once_then_ends() {
    let mut bad = event_frame("metadata", "{}");
    let last = bad.len() - 1;
    bad[last] ^= 0x01;
    let mut stream = Box::pin(decode_stream(futures::stream::iter(vec![bad])));
    let failure = block_on(stream.next())
        .and_then(Result::err)
        .unwrap_or_else(|| panic!("the flipped CRC is yielded, not swallowed"));
    assert!(matches!(
        failure,
        FrameError::Crc {
            scope: CrcScope::Message,
            ..
        }
    ));
    assert!(
        block_on(stream.next()).is_none(),
        "exactly one failure, then the stream ends"
    );

    let wire = event_frame("metadata", "{}");
    let mut stream = Box::pin(decode_stream(futures::stream::iter(vec![
        wire[..wire.len() - 3].to_vec(),
    ])));
    let failure = block_on(stream.next())
        .and_then(Result::err)
        .unwrap_or_else(|| panic!("the cut frame is yielded, not swallowed"));
    assert!(matches!(failure, FrameError::Truncated { .. }));
    assert!(block_on(stream.next()).is_none());
}

#[test]
fn typed_header_values_round_trip() {
    let mut block = Vec::new();
    block.extend_from_slice(&typed("bool-true", &[0]));
    block.extend_from_slice(&typed("bool-false", &[1]));
    block.extend_from_slice(&typed("byte", &[2, 0xFB]));
    block.extend_from_slice(&typed("short", &[3, 0x12, 0x34]));
    block.extend_from_slice(&typed("int", &[4, 0x12, 0x34, 0x56, 0x78]));
    block.extend_from_slice(&typed("long", &[5, 1, 2, 3, 4, 5, 6, 7, 8]));
    block.extend_from_slice(&typed("blob", &[6, 0, 2, 0xAA, 0xBB]));
    block.extend_from_slice(&typed("text", &[7, 0, 2, b'o', b'k']));
    block.extend_from_slice(&typed("stamp", &[8, 0, 0, 0, 0, 0, 0, 0, 1]));
    block.extend_from_slice(&typed(
        "uuid",
        &[9, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    ));
    let headers_len = u32::try_from(block.len()).unwrap_or(u32::MAX);
    let mut wire = Vec::new();
    let total = u32::try_from(PRELUDE_LEN + block.len() + 4).unwrap_or(u32::MAX);
    wire.extend_from_slice(&total.to_be_bytes());
    wire.extend_from_slice(&headers_len.to_be_bytes());
    wire.extend_from_slice(&crc(&wire).to_be_bytes());
    wire.extend_from_slice(&block);
    let message_crc = crc(&wire);
    wire.extend_from_slice(&message_crc.to_be_bytes());

    let frames = decode_all(vec![wire]).unwrap_or_else(|_| panic!("all types decode"));
    let headers = &frames
        .first()
        .unwrap_or_else(|| panic!("one frame"))
        .headers;
    assert_eq!(headers.get("bool-true"), Some(&HeaderValue::Bool(true)));
    assert_eq!(headers.get("bool-false"), Some(&HeaderValue::Bool(false)));
    assert_eq!(headers.get("byte"), Some(&HeaderValue::Byte(-5)));
    assert_eq!(headers.get("short"), Some(&HeaderValue::Int16(0x1234)));
    assert_eq!(headers.get("int"), Some(&HeaderValue::Int32(0x1234_5678)));
    assert_eq!(
        headers.get("long"),
        Some(&HeaderValue::Int64(0x0102_0304_0506_0708))
    );
    assert_eq!(
        headers.get("blob"),
        Some(&HeaderValue::ByteArray(vec![0xAA, 0xBB]))
    );
    assert_eq!(
        headers.get("text"),
        Some(&HeaderValue::String(String::from("ok")))
    );
    assert_eq!(headers.get("stamp"), Some(&HeaderValue::Timestamp(1)));
    assert_eq!(
        headers.get("uuid"),
        Some(&HeaderValue::Uuid([
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15
        ]))
    );
}

#[test]
fn unknown_header_type_and_empty_name_are_header_failures() {
    let mut block = typed("a", &[42]);
    let headers_len = u32::try_from(block.len()).unwrap_or(u32::MAX);
    let mut wire = Vec::new();
    let total = u32::try_from(PRELUDE_LEN + block.len() + 4).unwrap_or(u32::MAX);
    wire.extend_from_slice(&total.to_be_bytes());
    wire.extend_from_slice(&headers_len.to_be_bytes());
    wire.extend_from_slice(&crc(&wire).to_be_bytes());
    wire.append(&mut block);
    wire.extend_from_slice(&crc(&wire).to_be_bytes());
    let error = decode_all(vec![wire])
        .err()
        .unwrap_or_else(|| panic!("unknown type refuses"));
    assert!(matches!(error, FrameError::HeaderType { kind: 42 }));

    let mut block = vec![0];
    block.extend_from_slice(&crc(&[]).to_be_bytes());
    let headers_len = u32::try_from(block.len()).unwrap_or(u32::MAX);
    let mut wire = Vec::new();
    let total = u32::try_from(PRELUDE_LEN + block.len() + 4).unwrap_or(u32::MAX);
    wire.extend_from_slice(&total.to_be_bytes());
    wire.extend_from_slice(&headers_len.to_be_bytes());
    wire.extend_from_slice(&crc(&wire).to_be_bytes());
    wire.append(&mut block);
    wire.extend_from_slice(&crc(&wire).to_be_bytes());
    let error = decode_all(vec![wire])
        .err()
        .unwrap_or_else(|| panic!("empty name refuses"));
    assert!(matches!(error, FrameError::Header { .. }));
}

#[test]
fn repeated_header_names_keep_the_last_value() {
    let wire = frame(
        &[(":event-type", "first"), (":event-type", "second")],
        b"{}",
    );
    let frames = decode_all(vec![wire]).unwrap_or_else(|_| panic!("frame decodes"));
    assert_eq!(frames[0].string_header(":event-type"), Some("second"));
}

#[test]
fn stream_ends_cleanly_on_exact_boundary_and_reports_after_frames() {
    let wire = event_frame("metadata", "{}");
    let frames =
        decode_all(vec![wire.clone(), wire]).unwrap_or_else(|_| panic!("boundary frames decode"));
    assert_eq!(frames.len(), 2);
}

/// A byte source that records when it is dropped: the cancellation witness.
struct DropWitness {
    chunks: std::vec::IntoIter<Vec<u8>>,
    dropped: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for DropWitness {
    fn drop(&mut self) {
        self.dropped
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Stream for DropWitness {
    type Item = Vec<u8>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::task::Poll::Ready(self.chunks.next())
    }
}

#[test]
fn dropping_the_stream_drops_the_byte_source_and_an_error_does_too() {
    let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let source = DropWitness {
        chunks: vec![event_frame("metadata", "{}")].into_iter(),
        dropped: std::sync::Arc::clone(&dropped),
    };
    let mut stream = Box::pin(decode_stream(source));
    let frame = block_on(stream.next())
        .and_then(Result::ok)
        .unwrap_or_else(|| panic!("one frame arrives"));
    assert_eq!(frame.string_header(":event-type"), Some("metadata"));
    drop(stream);
    assert!(
        dropped.load(std::sync::atomic::Ordering::SeqCst),
        "dropping the decoded stream cancels the byte source"
    );

    // A framing failure ends the stream and drops the source the same way.
    let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut wire = event_frame("metadata", "{}");
    let last = wire.len() - 1;
    wire[last] ^= 0x01;
    let source = DropWitness {
        chunks: vec![wire].into_iter(),
        dropped: std::sync::Arc::clone(&dropped),
    };
    let mut stream = Box::pin(decode_stream(source));
    let failure = block_on(stream.next())
        .and_then(Result::err)
        .unwrap_or_else(|| panic!("the flipped CRC fails"));
    assert!(matches!(failure, FrameError::Crc { .. }));
    assert!(
        block_on(stream.next()).is_none(),
        "the stream ends after its one failure"
    );
    assert!(
        dropped.load(std::sync::atomic::Ordering::SeqCst),
        "ending the stream releases the byte source"
    );
}

/// A header block holding every typed value once.
fn all_types_block() -> Vec<u8> {
    let mut block = Vec::new();
    block.extend_from_slice(&typed("bool-true", &[0]));
    block.extend_from_slice(&typed("bool-false", &[1]));
    block.extend_from_slice(&typed("byte", &[2, 0xFB]));
    block.extend_from_slice(&typed("short", &[3, 0x12, 0x34]));
    block.extend_from_slice(&typed("int", &[4, 0x12, 0x34, 0x56, 0x78]));
    block.extend_from_slice(&typed("long", &[5, 1, 2, 3, 4, 5, 6, 7, 8]));
    block.extend_from_slice(&typed("blob", &[6, 0, 2, 0xAA, 0xBB]));
    block.extend_from_slice(&typed("text", &[7, 0, 2, b'o', b'k']));
    block.extend_from_slice(&typed("stamp", &[8, 0, 0, 0, 0, 0, 0, 0, 1]));
    block.extend_from_slice(&typed(
        "uuid",
        &[9, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    ));
    block
}

#[test]
fn every_chunking_of_a_mixed_stream_decodes_the_same_frames() {
    let wire = [
        event_frame("messageStart", r#"{"role":"assistant"}"#),
        raw_frame(&all_types_block(), b"typed payload"),
        event_frame("messageStop", r#"{"stopReason":"end_turn"}"#),
    ]
    .concat();
    let whole = decode_all(vec![wire.clone()]).unwrap_or_else(|_| panic!("whole stream decodes"));
    assert_eq!(whole.len(), 3);
    assert_eq!(whole[1].payload, b"typed payload");
    for size in 1..=wire.len() {
        let chunks: Vec<Vec<u8>> = wire.chunks(size).map(<[u8]>::to_vec).collect();
        assert_eq!(
            decode_all(chunks).unwrap_or_else(|_| panic!("chunk size {size} decodes")),
            whole,
            "chunk size {size}"
        );
    }
}

#[test]
fn a_chunk_of_many_frames_leaves_only_the_partial_tail_buffered() {
    let first = event_frame("messageStart", "{}");
    let second = event_frame("metadata", r#"{"usage":{"inputTokens":1}}"#);
    let third = event_frame("messageStop", r#"{"stopReason":"end_turn"}"#);
    let cut = third.len() / 2;
    let mut decoder = FrameDecoder::new();
    let pushed = decoder.push(&[first, second, third[..cut].to_vec()].concat());
    assert_eq!(pushed.frames.len(), 2);
    assert_eq!(pushed.failure, None);
    assert_eq!(
        decoder.buffer.len(),
        cut,
        "only the unfinished frame stays buffered"
    );
    let pushed = decoder.push(&third[cut..]);
    assert_eq!(pushed.frames.len(), 1);
    assert_eq!(decoder.buffer, Vec::<u8>::new());
    assert_eq!(decoder.finish(), Ok(()));
}

#[test]
fn frames_that_arrived_intact_ahead_of_a_corrupt_one_are_delivered_first() {
    let mut corrupt = event_frame("metadata", "{}");
    let last = corrupt.len() - 1;
    corrupt[last] ^= 0x01;
    let wire = [
        event_frame("messageStart", "{}"),
        event_frame(
            "contentBlockDelta",
            r#"{"contentBlockIndex":0,"delta":{"text":"hi"}}"#,
        ),
        corrupt,
        event_frame("messageStop", "{}"),
    ]
    .concat();
    let mut decoder = FrameDecoder::new();
    let pushed = decoder.push(&wire);
    assert_eq!(pushed.frames.len(), 2);
    assert_eq!(
        pushed.frames[1].string_header(":event-type"),
        Some("contentBlockDelta")
    );
    assert!(matches!(
        pushed.failure,
        Some(FrameError::Crc {
            scope: CrcScope::Message,
            ..
        })
    ));
    assert!(decoder.buffer.is_empty(), "a failed decoder keeps nothing");
    assert_eq!(decoder.push(&wire), Pushed::default());

    // The byte stream yields the two frames, the one failure, then ends.
    let mut stream = Box::pin(decode_stream(futures::stream::iter(vec![wire])));
    assert!(block_on(stream.next()).is_some_and(|item| item.is_ok()));
    assert!(block_on(stream.next()).is_some_and(|item| item.is_ok()));
    assert!(block_on(stream.next()).is_some_and(|item| matches!(
        item,
        Err(FrameError::Crc {
            scope: CrcScope::Message,
            ..
        })
    )));
    assert!(block_on(stream.next()).is_none());
}

#[test]
fn a_corrupt_prelude_reports_the_crc_before_any_length() {
    // Garbled length bytes with the CRC left alone: the declared total is
    // absurd, yet the verdict is the checksum, not a length.
    let mut wire = event_frame("metadata", "{}");
    wire[..4].copy_from_slice(&u32::MAX.to_be_bytes());
    let error = decode_all(vec![wire])
        .err()
        .unwrap_or_else(|| panic!("a garbled prelude refuses"));
    assert!(matches!(
        error,
        FrameError::Crc {
            scope: CrcScope::Prelude,
            ..
        }
    ));
    // Fewer than twelve bytes decide nothing, whatever they say.
    let mut decoder = FrameDecoder::new();
    assert_eq!(decoder.push(&[0xFF; PRELUDE_LEN - 1]), Pushed::default());
    assert_eq!(
        decoder.finish(),
        Err(FrameError::Truncated {
            declared: 11,
            available: 11
        })
    );
}

#[test]
fn a_length_pair_the_framing_cannot_produce_is_refused_once_the_prelude_checks_out() {
    let sealed = |total: u32, headers: u32| {
        let mut wire = event_frame("metadata", "{}");
        reseal(&mut wire, total, headers);
        decode_all(vec![wire])
            .err()
            .unwrap_or_else(|| panic!("{total}/{headers} refuses"))
    };
    // A total below the fixed overhead of every frame.
    assert_eq!(
        sealed(15, 0),
        FrameError::Lengths {
            total: 15,
            headers: 0
        }
    );
    // A header block longer than the room the total leaves for it.
    assert_eq!(
        sealed(100, 85),
        FrameError::Lengths {
            total: 100,
            headers: 85
        }
    );
    // A header block over the protocol bound, inside an allowed total.
    let over = MAX_HEADERS_LEN + 1;
    assert!(matches!(
        sealed(over + 1024, over),
        FrameError::HeadersLimit { headers, .. } if headers == over
    ));
}

#[test]
fn header_text_that_is_not_utf8_is_refused() {
    let value = raw_frame(&typed("k", &[7, 0, 2, 0xFF, 0xFE]), b"");
    assert!(matches!(
        decode_all(vec![value]),
        Err(FrameError::Header { .. })
    ));
    let mut block = vec![2, 0xFF, 0xFE];
    block.extend_from_slice(&[7, 0, 0]);
    let name = raw_frame(&block, b"");
    assert!(matches!(
        decode_all(vec![name]),
        Err(FrameError::Header { .. })
    ));
}

#[test]
fn a_declared_value_length_never_reads_past_the_header_block() {
    // Sixty-five thousand bytes follow in the payload, so a missing bound
    // would decode payload bytes as header data instead of refusing.
    for kind in [6, 7] {
        let wire = raw_frame(&typed("k", &[kind, 0xFF, 0xFF]), &vec![0x41; 70_000]);
        assert!(
            matches!(decode_all(vec![wire]), Err(FrameError::Header { .. })),
            "type {kind}"
        );
    }
}

#[test]
fn the_message_crc_is_checked_before_any_header_is_read() {
    let mut wire = raw_frame(&typed("a", &[42]), b"");
    let last = wire.len() - 1;
    wire[last] ^= 0x01;
    assert!(matches!(
        decode_all(vec![wire]),
        Err(FrameError::Crc {
            scope: CrcScope::Message,
            ..
        })
    ));
}
