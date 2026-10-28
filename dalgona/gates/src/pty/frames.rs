// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Private frame-reader owner for the Windows probe driver.
//!
//! Cancel-safe owned reader: the header and payload buffers live in `self`,
//! every `read` appends through `AsyncReadExt::read`, and all state advances
//! synchronously after each `await`. Dropping the `read_parsed` future at any
//! byte boundary (including inside `timeout`) loses nothing: the next poll
//! resumes from `header_filled`/`payload_filled`. No extra task, no channel,
//! no `spawn`: one owned `FrameReader` per helper stream. Memory is bounded
//! (5-byte header plus at most `MAX_PAYLOAD` once the declared length
//! validates); an oversized declaration is refused before any payload
//! allocation and poisons the reader so later bytes can never be misread as
//! a phantom header. A clean EOF counts only at a frame boundary; EOF after
//! any header or payload byte is a named truncation failure, never an empty
//! frame and never `Closed`. Wire bytes are untouched: decoding reuses the
//! frozen `protocol` seam (`payload_length` then `Response::decode`), and the
//! raw-vs-`u16` `Write`/`Out` mismatch is held for the parent codec lane, not
//! repaired here. Witness tests use only round-trip-safe frames and pin
//! their literals against the shared encoder.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::time::timeout;

use super::protocol::{self, Response};

/// One bounded read outcome from the helper stream.
pub(super) enum Parsed {
    /// A response frame arrived.
    Frame(Response),
    /// No frame completed within the wait; buffered partial bytes are kept
    /// for the next poll.
    Quiet,
    /// The helper closed its pipes at a frame boundary.
    Closed,
}

/// Owned helper-stream reader with resume state.
pub(super) struct FrameReader<R> {
    /// The helper stream; owned for the probe's whole read lifetime.
    stream: R,
    /// Frame header bytes collected so far (`HEADER_LEN` total).
    header: [u8; protocol::HEADER_LEN],
    /// Header bytes collected so far.
    header_filled: usize,
    /// Payload buffer sized from the validated declaration; empty until the
    /// header completes.
    payload: Vec<u8>,
    /// Payload bytes collected so far.
    payload_filled: usize,
    /// Whether the payload buffer was sized from the current header.
    payload_ready: bool,
    /// Terminal refusal: once set, every later read reports the same reason
    /// instead of misreading trailing bytes as a new header.
    failed: Option<String>,
}

impl<R> FrameReader<R>
where
    R: AsyncRead + Unpin,
{
    /// Owns `stream` for the probe's whole read lifetime.
    pub(super) fn new(stream: R) -> Self {
        Self {
            stream,
            header: [0_u8; protocol::HEADER_LEN],
            header_filled: 0,
            payload: Vec::new(),
            payload_filled: 0,
            payload_ready: false,
            failed: None,
        }
    }

    /// Reads one response frame; `None` means the helper closed its pipes at
    /// a frame boundary.
    ///
    /// # Errors
    /// Returns named truncation failures for EOF after any header or payload
    /// byte, the bounded oversize and invalid-tag refusals before payload
    /// allocation, transport failures, and the frozen protocol decode failures.
    pub(super) async fn read_frame(&mut self) -> Result<Option<Response>, String> {
        if let Some(reason) = self.failed.clone() {
            return Err(reason);
        }
        while self.header_filled < protocol::HEADER_LEN {
            let end = self.header_filled;
            let free = &mut self.header[end..];
            let received = match self.stream.read(free).await {
                Ok(received) => received,
                Err(error) => {
                    let reason = format!("reading a frame header failed: {error}");
                    self.failed = Some(reason.clone());
                    return Err(reason);
                }
            };
            if received == 0 {
                return self.eof_in_header();
            }
            self.header_filled += received;
        }
        if !(protocol::TAG_SPAWNED..=protocol::TAG_FINAL).contains(&self.header[0]) {
            let reason = match Response::decode(self.header[0], &[]) {
                Err(reason) => reason,
                Ok(_) => format!("helper frame tag {} is not a response tag", self.header[0]),
            };
            self.failed = Some(reason.clone());
            return Err(reason);
        }
        if !self.payload_ready {
            let length = match protocol::payload_length(&self.header) {
                Ok(length) => length,
                Err(reason) => {
                    self.failed = Some(reason.clone());
                    return Err(reason);
                }
            };
            self.payload.resize(length, 0);
            self.payload_ready = true;
        }
        while self.payload_filled < self.payload.len() {
            let end = self.payload_filled;
            let free = &mut self.payload[end..];
            let received = match self.stream.read(free).await {
                Ok(received) => received,
                Err(error) => {
                    let tag = self.header[0];
                    let reason = format!(
                        "reading a {} frame payload failed: {error}",
                        protocol::tag_name(tag)
                    );
                    self.failed = Some(reason.clone());
                    return Err(reason);
                }
            };
            if received == 0 {
                return self.eof_in_payload();
            }
            self.payload_filled += received;
        }
        let tag = self.header[0];
        match Response::decode(tag, &self.payload) {
            Ok(response) => {
                self.header_filled = 0;
                self.payload = Vec::new();
                self.payload_filled = 0;
                self.payload_ready = false;
                Ok(Some(response))
            }
            Err(reason) => {
                self.failed = Some(reason.clone());
                Err(reason)
            }
        }
    }

    /// Reads one response under a bounded wait and maps the outcome.
    /// Partial bytes survive the timeout inside `self` for the next poll.
    ///
    /// # Errors
    /// Propagates [`FrameReader::read_frame`] failures.
    pub(super) async fn read_parsed(&mut self, wait: Duration) -> Result<Parsed, String> {
        match timeout(wait, self.read_frame()).await {
            Err(_elapsed) => Ok(Parsed::Quiet),
            Ok(Ok(None)) => Ok(Parsed::Closed),
            Ok(Ok(Some(frame))) => Ok(Parsed::Frame(frame)),
            Ok(Err(error)) => Err(error),
        }
    }

    /// EOF while filling the header: clean only with zero header bytes;
    /// otherwise a named truncation that poisons the reader.
    fn eof_in_header(&mut self) -> Result<Option<Response>, String> {
        if self.header_filled == 0 {
            return Ok(None);
        }
        let reason = format!(
            "frame header truncated: received {} of {} bytes before EOF",
            self.header_filled,
            protocol::HEADER_LEN
        );
        self.failed = Some(reason.clone());
        Err(reason)
    }

    /// EOF while filling the payload: always a named truncation, never an
    /// empty frame and never `Closed`.
    fn eof_in_payload(&mut self) -> Result<Option<Response>, String> {
        let tag = self.header[0];
        let reason = format!(
            "{} frame payload truncated: received {} of {} bytes before EOF",
            protocol::tag_name(tag),
            self.payload_filled,
            self.payload.len()
        );
        self.failed = Some(reason.clone());
        Err(reason)
    }
}

#[cfg(test)]
mod frames_witness {
    //! Synthetic duplex counterexamples at the shared codec seam. No `ConPTY`,
    //! no helper process, no native I/O: `tokio::io::duplex` feeds immutable
    //! literal bytes that are pinned against `protocol::Response::encode`,
    //! so the inputs cannot drift from the frozen wire. The duplex channel is
    //! held open across every timeout, proving cancellation keeps partial
    //! bytes. Uses only round-trip-safe frames; `Write`/`Out` are excluded
    //! here and held for the parent codec lane.

    use super::super::protocol::Response;
    use super::{FrameReader, Parsed};

    use tokio::io::AsyncWriteExt;

    /// Fallible witness result over the standard error trait.
    type WitnessResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    /// `Response::Spawned { pid: 1234 }` as frozen literal bytes.
    const SPAWNED_1234: &[u8] = &[6, 4, 0, 0, 0, 0xD2, 0x04, 0x00, 0x00];
    /// Nonsymmetric write stamp `0x1122_3344_5566_7788`, little-endian on the
    /// wire: a byte swap or a 32-bit read cannot pass unseen.
    const STAMP_NS: u64 = 0x1122_3344_5566_7788;
    /// `Response::Wrote { t_ns: STAMP_NS }` as frozen literal bytes.
    const WROTE_STAMP: &[u8] = &[
        7, 8, 0, 0, 0, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11,
    ];
    /// `Response::Exited { code: 0 }` as frozen literal bytes.
    const EXITED_0: &[u8] = &[9, 4, 0, 0, 0, 0, 0, 0, 0];
    /// `Response::Failed { reason: "boom" }` as frozen literal bytes:
    /// tag 10, length 6, then `u16` length 4 plus `boom`.
    const FAILED_BOOM: &[u8] = &[10, 6, 0, 0, 0, 4, 0, b'b', b'o', b'o', b'm'];
    /// `Response::Final { unjoined: [] }` as frozen literal bytes.
    const FINAL_EMPTY: &[u8] = &[11, 2, 0, 0, 0, 0, 0];

    /// Maps a typed reader refusal into the witness error trait.
    fn other(reason: String) -> std::io::Error {
        std::io::Error::other(reason)
    }

    /// Pins the immutable literals against the shared encoder: the test
    /// inputs below are the literal bytes, and this test proves the literals
    /// equal what the frozen protocol encodes. Uses only round-trip-safe
    /// frames.
    #[test]
    fn literals_match_shared_encoder() -> WitnessResult<()> {
        let mut encoded = Vec::new();
        Response::Spawned { pid: 1234 }
            .encode(&mut encoded)
            .map_err(other)?;
        if encoded != SPAWNED_1234 {
            return Err(other(format!(
                "Spawned literal drift: got {encoded:?}, want {SPAWNED_1234:?}"
            ))
            .into());
        }
        encoded.clear();
        Response::Wrote { t_ns: STAMP_NS }
            .encode(&mut encoded)
            .map_err(other)?;
        if encoded != WROTE_STAMP {
            return Err(other(format!(
                "Wrote literal drift: got {encoded:?}, want {WROTE_STAMP:?}"
            ))
            .into());
        }
        encoded.clear();
        Response::Exited { code: 0 }
            .encode(&mut encoded)
            .map_err(other)?;
        if encoded != EXITED_0 {
            return Err(other(format!(
                "Exited literal drift: got {encoded:?}, want {EXITED_0:?}"
            ))
            .into());
        }
        encoded.clear();
        Response::Failed {
            reason: "boom".to_owned(),
        }
        .encode(&mut encoded)
        .map_err(other)?;
        if encoded != FAILED_BOOM {
            return Err(other(format!(
                "Failed literal drift: got {encoded:?}, want {FAILED_BOOM:?}"
            ))
            .into());
        }
        encoded.clear();
        Response::Final {
            unjoined: Vec::new(),
        }
        .encode(&mut encoded)
        .map_err(other)?;
        if encoded != FINAL_EMPTY {
            return Err(other(format!(
                "Final literal drift: got {encoded:?}, want {FINAL_EMPTY:?}"
            ))
            .into());
        }
        Ok(())
    }

    /// Reads the next frame under a 50ms virtual wait, failing on `Quiet` or
    /// `Closed` so callers observe the exact decoded message.
    async fn next_frame(
        reader: &mut FrameReader<tokio::io::DuplexStream>,
    ) -> WitnessResult<Response> {
        match reader
            .read_parsed(std::time::Duration::from_millis(50))
            .await
            .map_err(other)?
        {
            Parsed::Frame(frame) => Ok(frame),
            Parsed::Quiet => Err(other("expected a frame, got Quiet".to_owned()).into()),
            Parsed::Closed => Err(other("expected a frame, got Closed".to_owned()).into()),
        }
    }

    /// Requires `Quiet` under a 10ms virtual wait: no bytes completed a frame.
    async fn require_quiet(reader: &mut FrameReader<tokio::io::DuplexStream>) -> WitnessResult<()> {
        match reader
            .read_parsed(std::time::Duration::from_millis(10))
            .await
            .map_err(other)?
        {
            Parsed::Quiet => Ok(()),
            Parsed::Frame(_) => Err(other("expected Quiet, got Frame".to_owned()).into()),
            Parsed::Closed => Err(other("expected Quiet, got Closed".to_owned()).into()),
        }
    }

    /// Re-encodes a decoded frame and requires the frozen literal bytes, so
    /// the tests below observe both the exact message values and the exact
    /// wire bytes without new trait derives on the frozen types.
    fn require_wire(frame: &Response, want: &[u8]) -> WitnessResult<()> {
        let mut encoded = Vec::new();
        frame.encode(&mut encoded).map_err(other)?;
        if encoded != want {
            return Err(other(format!("wire mismatch: got {encoded:?}, want {want:?}")).into());
        }
        Ok(())
    }

    /// Baseline sanity: silence is `Quiet`, then a complete frame reads with
    /// its exact exit code and wire bytes.
    #[tokio::test(start_paused = true)]
    async fn quiet_then_frame() -> WitnessResult<()> {
        let (mut writer, reader) = tokio::io::duplex(32);
        let mut reader = FrameReader::new(reader);
        require_quiet(&mut reader).await?;
        writer.write_all(EXITED_0).await?;
        let frame = next_frame(&mut reader).await?;
        match frame {
            Response::Exited { code } => {
                if code != 0 {
                    return Err(other(format!("Exited code mismatch: got {code}")).into());
                }
            }
            _ => return Err(other("expected Exited, got another response".to_owned()).into()),
        }
        require_wire(&frame, EXITED_0)?;
        Ok(())
    }

    /// Counterexample 1: 2 of 5 header bytes arrive, the wait expires, then
    /// the remaining 7 bytes of the 9-byte `Exited` frame arrive. The frame
    /// must recover whole with no duplication or loss.
    #[tokio::test(start_paused = true)]
    async fn partial_header_resumes_after_timeout() -> WitnessResult<()> {
        let (mut writer, reader) = tokio::io::duplex(32);
        let mut reader = FrameReader::new(reader);
        writer.write_all(&EXITED_0[..2]).await?;
        require_quiet(&mut reader).await?;
        writer.write_all(&EXITED_0[2..]).await?;
        let frame = next_frame(&mut reader).await?;
        match frame {
            Response::Exited { code } => {
                if code != 0 {
                    return Err(other(format!("Exited code mismatch: got {code}")).into());
                }
            }
            _ => return Err(other("expected Exited, got another response".to_owned()).into()),
        }
        require_wire(&frame, EXITED_0)?;
        Ok(())
    }

    /// Counterexample 2: the full 5-byte header plus 1 of 4 payload bytes of
    /// `Spawned` arrive, the wait expires, then the last 3 payload bytes
    /// arrive. The frame must recover whole with its exact pid.
    #[tokio::test(start_paused = true)]
    async fn partial_payload_resumes_after_timeout() -> WitnessResult<()> {
        let (mut writer, reader) = tokio::io::duplex(32);
        let mut reader = FrameReader::new(reader);
        writer.write_all(&SPAWNED_1234[..6]).await?;
        require_quiet(&mut reader).await?;
        writer.write_all(&SPAWNED_1234[6..]).await?;
        let frame = next_frame(&mut reader).await?;
        match frame {
            Response::Spawned { pid } => {
                if pid != 1234 {
                    return Err(other(format!("Spawned pid mismatch: got {pid}")).into());
                }
            }
            _ => return Err(other("expected Spawned, got another response".to_owned()).into()),
        }
        require_wire(&frame, SPAWNED_1234)?;
        Ok(())
    }

    /// Counterexample 2b: cancellation inside the first of two back-to-back
    /// frames. After the resume, the first frame reads whole with its exact
    /// code and the second frame follows with its exact nonsymmetric stamp,
    /// proving the reader did not consume bytes belonging to the next frame.
    #[tokio::test(start_paused = true)]
    async fn two_frames_recover_in_order_after_cancellation() -> WitnessResult<()> {
        let (mut writer, reader) = tokio::io::duplex(32);
        let mut reader = FrameReader::new(reader);
        writer.write_all(&EXITED_0[..3]).await?;
        require_quiet(&mut reader).await?;
        writer.write_all(&EXITED_0[3..]).await?;
        writer.write_all(WROTE_STAMP).await?;
        let first = next_frame(&mut reader).await?;
        match first {
            Response::Exited { code } => {
                if code != 0 {
                    return Err(other(format!("first code mismatch: got {code}")).into());
                }
            }
            _ => {
                return Err(other("expected Exited first, got another response".to_owned()).into());
            }
        }
        require_wire(&first, EXITED_0)?;
        let second = next_frame(&mut reader).await?;
        match second {
            Response::Wrote { t_ns } => {
                if t_ns != STAMP_NS {
                    return Err(other(format!("second stamp mismatch: got {t_ns:#x}")).into());
                }
            }
            _ => {
                return Err(other("expected Wrote second, got another response".to_owned()).into());
            }
        }
        require_wire(&second, WROTE_STAMP)?;
        Ok(())
    }

    /// Counterexample 3: a 13-byte `Wrote` frame with the nonsymmetric stamp
    /// drips one byte per wait. Every incomplete prefix is `Quiet`; the
    /// completed frame reads whole with its exact stamp, proving
    /// cancellation safety at every byte boundary.
    #[tokio::test(start_paused = true)]
    async fn byte_drip_recovers_at_every_boundary() -> WitnessResult<()> {
        let (mut writer, reader) = tokio::io::duplex(32);
        let mut reader = FrameReader::new(reader);
        for end in 1..WROTE_STAMP.len() {
            writer.write_all(&WROTE_STAMP[end - 1..end]).await?;
            require_quiet(&mut reader).await?;
        }
        writer
            .write_all(&WROTE_STAMP[WROTE_STAMP.len() - 1..])
            .await?;
        let frame = next_frame(&mut reader).await?;
        match frame {
            Response::Wrote { t_ns } => {
                if t_ns != STAMP_NS {
                    return Err(other(format!("dripped stamp mismatch: got {t_ns:#x}")).into());
                }
            }
            _ => return Err(other("expected Wrote, got another response".to_owned()).into()),
        }
        require_wire(&frame, WROTE_STAMP)?;
        Ok(())
    }

    /// Counterexample 4: the writer closes after 3 of 5 header bytes. This is
    /// truncation, never a clean `Closed`: the error must name the 3-of-5
    /// header shortfall.
    #[tokio::test(start_paused = true)]
    async fn eof_after_partial_header_is_truncation_not_closed() -> WitnessResult<()> {
        let (mut writer, reader) = tokio::io::duplex(32);
        let mut reader = FrameReader::new(reader);
        writer.write_all(&EXITED_0[..3]).await?;
        drop(writer);
        match reader
            .read_parsed(std::time::Duration::from_millis(50))
            .await
        {
            Ok(Parsed::Closed) => {
                return Err(other(
                    "partial header EOF reported Closed, want truncation".to_owned(),
                )
                .into());
            }
            Ok(_) => {
                return Err(other(
                    "partial header EOF gave a frame or Quiet, want truncation".to_owned(),
                )
                .into());
            }
            Err(reason) => {
                if !reason.contains("truncat") || !reason.contains('3') || !reason.contains('5') {
                    return Err(other(format!(
                        "header truncation must name 3 of 5 bytes, got: {reason}"
                    ))
                    .into());
                }
                Ok(())
            }
        }
    }

    /// Counterexample 5: the writer closes after the header plus 2 of 4
    /// payload bytes of `Spawned` with its exact pid prefix. The error must
    /// name the payload truncation, never `Closed` and never an empty frame.
    #[tokio::test(start_paused = true)]
    async fn eof_after_partial_payload_is_truncation() -> WitnessResult<()> {
        let (mut writer, reader) = tokio::io::duplex(32);
        let mut reader = FrameReader::new(reader);
        writer.write_all(&SPAWNED_1234[..7]).await?;
        drop(writer);
        match reader
            .read_parsed(std::time::Duration::from_millis(50))
            .await
        {
            Ok(Parsed::Closed) => {
                return Err(other(
                    "partial payload EOF reported Closed, want truncation".to_owned(),
                )
                .into());
            }
            Ok(_) => {
                return Err(other(
                    "partial payload EOF gave a frame or Quiet, want truncation".to_owned(),
                )
                .into());
            }
            Err(reason) => {
                if !reason.contains("truncat")
                    || !reason.contains("Spawned")
                    || !reason.contains('2')
                    || !reason.contains('4')
                {
                    return Err(other(format!(
                        "payload truncation must name Spawned 2 of 4 bytes, got: {reason}"
                    ))
                    .into());
                }
                Ok(())
            }
        }
    }

    /// Counterexample 6: a header declaring `65_537` payload bytes (one past
    /// the `65_536` cap) is refused without consuming any payload, and the
    /// refusal is terminal: a second read reports the same refusal instead
    /// of misreading the following bytes as a phantom header. The
    /// before-allocation order is source-audited (`payload_length` validates
    /// before `resize`); the test pins the observable half: refusal plus
    /// terminality with trailing bytes unconsumed.
    #[tokio::test(start_paused = true)]
    async fn oversized_length_is_refused_and_terminal() -> WitnessResult<()> {
        let (mut writer, reader) = tokio::io::duplex(32);
        let mut reader = FrameReader::new(reader);
        let oversize: u32 = 65_537;
        let mut header = [0_u8; 5];
        header[0] = 9;
        header[1..].copy_from_slice(&oversize.to_le_bytes());
        writer.write_all(&header).await?;
        writer.write_all(EXITED_0).await?;
        let Err(first) = reader
            .read_parsed(std::time::Duration::from_millis(50))
            .await
        else {
            return Err(other("oversized length must fail".to_owned()).into());
        };
        if !(first.contains("65_537") || first.contains("65537")) {
            return Err(other(format!(
                "oversize refusal must name the declared 65537 bytes, got: {first}"
            ))
            .into());
        }
        if !first.contains("65536") || !first.contains("cap") || !first.contains("Exited") {
            return Err(other(format!(
                "oversize refusal must name the Exited tag and the 65536 cap, got: {first}"
            ))
            .into());
        }
        if reader.payload.capacity() != 0 || !reader.payload.is_empty() {
            return Err(other(format!(
                "oversize must refuse before allocation: capacity {}, len {}",
                reader.payload.capacity(),
                reader.payload.len()
            ))
            .into());
        }
        let Err(second) = reader
            .read_parsed(std::time::Duration::from_millis(50))
            .await
        else {
            return Err(other("oversize refusal must be terminal".to_owned()).into());
        };
        if first != second {
            return Err(other(format!(
                "oversize refusal must repeat identically, got {first:?} then {second:?}"
            ))
            .into());
        }
        Ok(())
    }

    /// Invalid tags are refused from the header alone, before any payload
    /// allocation: a header with tag `0xFF` and a 4-byte declaration carries
    /// no payload bytes, yet the first poll must fail (not `Quiet`) with the
    /// frozen decode text, allocate nothing, and repeat identically.
    #[tokio::test(start_paused = true)]
    async fn invalid_tag_is_refused_before_allocation() -> WitnessResult<()> {
        let (mut writer, reader) = tokio::io::duplex(32);
        let mut reader = FrameReader::new(reader);
        writer.write_all(&[0xFF, 4, 0, 0, 0]).await?;
        let Err(first) = reader
            .read_parsed(std::time::Duration::from_millis(50))
            .await
        else {
            return Err(other("invalid tag must fail, not Quiet".to_owned()).into());
        };
        if !first.contains("255") || !first.contains("response tag") {
            return Err(other(format!(
                "invalid-tag refusal must name tag 255 as not a response tag, got: {first}"
            ))
            .into());
        }
        if reader.payload.capacity() != 0 || !reader.payload.is_empty() {
            return Err(other(format!(
                "invalid tag must refuse before allocation: capacity {}, len {}",
                reader.payload.capacity(),
                reader.payload.len()
            ))
            .into());
        }
        let Err(second) = reader
            .read_parsed(std::time::Duration::from_millis(50))
            .await
        else {
            return Err(other("invalid-tag refusal must be terminal".to_owned()).into());
        };
        if first != second {
            return Err(other(format!(
                "invalid-tag refusal must repeat identically, got {first:?} then {second:?}"
            ))
            .into());
        }
        Ok(())
    }

    /// A clean EOF after a whole frame is `Closed`, never a truncation: the
    /// frame reads whole with its exact code, then the dropped writer yields
    /// `Closed` at the next frame boundary.
    #[tokio::test(start_paused = true)]
    async fn whole_frame_then_eof_is_closed() -> WitnessResult<()> {
        let (mut writer, reader) = tokio::io::duplex(32);
        let mut reader = FrameReader::new(reader);
        writer.write_all(EXITED_0).await?;
        let frame = next_frame(&mut reader).await?;
        match frame {
            Response::Exited { code } => {
                if code != 0 {
                    return Err(other(format!("Exited code mismatch: got {code}")).into());
                }
            }
            _ => return Err(other("expected Exited, got another response".to_owned()).into()),
        }
        require_wire(&frame, EXITED_0)?;
        drop(writer);
        match reader
            .read_parsed(std::time::Duration::from_millis(50))
            .await
            .map_err(other)?
        {
            Parsed::Closed => Ok(()),
            Parsed::Quiet => Err(other("clean EOF gave Quiet, want Closed".to_owned()).into()),
            Parsed::Frame(_) => Err(other("clean EOF gave Frame, want Closed".to_owned()).into()),
        }
    }

    /// Counterexample 7: a valid-tag frame with a malformed payload must
    /// poison the reader. An `Exited` frame declaring 5 payload bytes (a
    /// 4-byte code plus one trailing byte) validates under the cap, so the
    /// refusal comes from `Response::decode`. A following valid `Exited`
    /// frame must never resync as a phantom success: the second read reports
    /// the same terminal refusal.
    #[tokio::test(start_paused = true)]
    async fn malformed_payload_is_terminal_without_resync() -> WitnessResult<()> {
        let (mut writer, reader) = tokio::io::duplex(32);
        let mut reader = FrameReader::new(reader);
        let malformed: &[u8] = &[9, 5, 0, 0, 0, 0, 0, 0, 0, 0xFF];
        writer.write_all(malformed).await?;
        writer.write_all(EXITED_0).await?;
        let Err(first) = reader
            .read_parsed(std::time::Duration::from_millis(50))
            .await
        else {
            return Err(other("malformed payload must fail, not resync".to_owned()).into());
        };
        if !first.contains("trailing") {
            return Err(other(format!(
                "decode refusal must name trailing bytes, got: {first}"
            ))
            .into());
        }
        let Err(second) = reader
            .read_parsed(std::time::Duration::from_millis(50))
            .await
        else {
            return Err(
                other("decode refusal must be terminal, not a phantom resync".to_owned()).into(),
            );
        };
        if first != second {
            return Err(other(format!(
                "decode refusal must repeat identically, got {first:?} then {second:?}"
            ))
            .into());
        }
        Ok(())
    }
}
