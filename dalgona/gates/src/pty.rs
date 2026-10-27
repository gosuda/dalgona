// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Keypress echo latency probe over a real pseudo-terminal, for the stress gate.
#![cfg_attr(
    unix,
    expect(
        clippy::disallowed_methods,
        reason = "the probe drives a real child process through a pseudo-terminal"
    )
)]

#[cfg(any(windows, test))]
mod frames;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::keypress_echo_latencies;

/// Binary frame protocol: a tag byte, a little-endian `u32` payload length, then the payload.
///
/// Byte contract:
/// - A frame whose declared payload length exceeds `MAX_PAYLOAD` is refused from its five
///   header bytes alone.
/// - `Write` carries the input bytes raw, at most `MAX_BYTES` of them, with no inner length
///   prefix.
/// - `Out` carries a little-endian `u64` stamp followed by the output bytes raw, at most
///   `MAX_BYTES` of them, with no inner length prefix.
/// - Strings and counts start with a little-endian `u16` length or count; strings are UTF-8 and
///   at most `MAX_STRING` bytes, and every count stays within its cap.
/// - All other fields are fixed-width little-endian integers.
/// - Bytes left after the last field are refused.
pub mod protocol {
    /// Header size: the tag byte plus the little-endian `u32` payload length.
    pub const HEADER_LEN: usize = 5;
    /// Largest encoded payload, enforced on every frame in both directions.
    pub const MAX_PAYLOAD: usize = 65_536;
    /// Largest single program path, working directory, argument, key, value, reason, or
    /// thread name string.
    pub const MAX_STRING: usize = 16_384;
    /// Largest `Write` payload and largest single output chunk.
    pub const MAX_BYTES: usize = 4_096;
    /// Largest argument count one `Spawn` frame may carry.
    pub const MAX_ARGC: usize = 32;
    /// Largest environment variable count one `Spawn` frame may carry.
    pub const MAX_ENV: usize = 128;
    /// Largest thread-name count one `Final` frame may carry.
    pub const MAX_NAMES: usize = 16;

    /// Tag of `Request::Spawn`.
    pub const TAG_SPAWN: u8 = 1;
    /// Tag of `Request::Write`.
    pub const TAG_WRITE: u8 = 2;
    /// Tag of `Request::Resize`.
    pub const TAG_RESIZE: u8 = 3;
    /// Tag of `Request::Kill`.
    pub const TAG_KILL: u8 = 4;
    /// Tag of `Request::Shutdown`.
    pub const TAG_SHUTDOWN: u8 = 5;
    /// Tag of `Response::Spawned`.
    pub const TAG_SPAWNED: u8 = 6;
    /// Tag of `Response::Wrote`.
    pub const TAG_WROTE: u8 = 7;
    /// Tag of `Response::Out`.
    pub const TAG_OUT: u8 = 8;
    /// Tag of `Response::Exited`.
    pub const TAG_EXITED: u8 = 9;
    /// Tag of `Response::Failed`.
    pub const TAG_FAILED: u8 = 10;
    /// Tag of `Response::Final`.
    pub const TAG_FINAL: u8 = 11;

    /// The child launch a `Spawn` frame carries; nothing joins or adds to these parts.
    pub struct SpawnRequest {
        /// Executable path.
        pub program: String,
        /// Arguments, applied one by one.
        pub args: Vec<String>,
        /// The complete child environment as key and value pairs.
        pub env: Vec<(String, String)>,
        /// Working directory for the child.
        pub cwd: String,
        /// Initial console columns.
        pub cols: u16,
        /// Initial console rows.
        pub rows: u16,
    }

    /// One request frame.
    pub enum Request {
        /// Launch the child described by the request.
        Spawn(SpawnRequest),
        /// Keypress bytes for the child's console input, at most [`MAX_BYTES`] long, sent
        /// raw with no inner length prefix.
        Write {
            /// The input bytes.
            bytes: Vec<u8>,
        },
        /// Resize the console.
        Resize {
            /// New column count.
            cols: u16,
            /// New row count.
            rows: u16,
        },
        /// Terminate the child tree.
        Kill,
        /// Finish the session; answered by [`Response::Final`].
        Shutdown,
    }

    /// One response frame.
    pub enum Response {
        /// The child is live; `pid` is the child's own process id.
        Spawned {
            /// Process id of the child.
            pid: u32,
        },
        /// Input reached the console; the stamp was taken immediately before the write.
        Wrote {
            /// Nanoseconds since the sender's start epoch.
            t_ns: u64,
        },
        /// Console output: the stamp, taken immediately after the read returned, as a
        /// little-endian `u64`, then the bytes read raw with no inner length prefix.
        Out {
            /// Nanoseconds since the sender's start epoch.
            t_ns: u64,
            /// The bytes read, at most [`MAX_BYTES`] long.
            bytes: Vec<u8>,
        },
        /// The child tree exited with the platform exit code.
        Exited {
            /// Exit code of the terminating process.
            code: u32,
        },
        /// A fatal error; the sender cleans up and exits.
        Failed {
            /// Bounded failure text.
            reason: String,
        },
        /// The last frame of a shutdown; `unjoined` names threads that missed their join
        /// deadline.
        Final {
            /// Names of threads that missed their join deadline.
            unjoined: Vec<String>,
        },
    }

    impl Request {
        /// The frame's tag byte.
        #[must_use]
        pub fn tag(&self) -> u8 {
            match self {
                Self::Spawn(_) => TAG_SPAWN,
                Self::Write { .. } => TAG_WRITE,
                Self::Resize { .. } => TAG_RESIZE,
                Self::Kill => TAG_KILL,
                Self::Shutdown => TAG_SHUTDOWN,
            }
        }

        /// Encodes the frame behind its header into `frame`.
        ///
        /// # Errors
        /// Names the violated bound when a field exceeds its cap.
        pub fn encode(&self, frame: &mut Vec<u8>) -> Result<(), String> {
            let mut payload = Vec::new();
            match self {
                Self::Spawn(spawn) => {
                    push_str(&mut payload, "program", &spawn.program)?;
                    push_str(&mut payload, "cwd", &spawn.cwd)?;
                    payload.extend_from_slice(&spawn.cols.to_le_bytes());
                    payload.extend_from_slice(&spawn.rows.to_le_bytes());
                    push_count(&mut payload, spawn.args.len(), MAX_ARGC, "argument count")?;
                    for argument in &spawn.args {
                        push_str(&mut payload, "argument", argument)?;
                    }
                    push_count(
                        &mut payload,
                        spawn.env.len(),
                        MAX_ENV,
                        "environment variable count",
                    )?;
                    for (key, value) in &spawn.env {
                        push_str(&mut payload, "environment key", key)?;
                        push_str(&mut payload, "environment value", value)?;
                    }
                }
                Self::Write { bytes } => {
                    if bytes.len() > MAX_BYTES {
                        return Err(format!(
                            "input payload has {} bytes, over the {MAX_BYTES} cap",
                            bytes.len()
                        ));
                    }
                    payload.extend_from_slice(bytes);
                }
                Self::Resize { cols, rows } => {
                    payload.extend_from_slice(&cols.to_le_bytes());
                    payload.extend_from_slice(&rows.to_le_bytes());
                }
                Self::Kill | Self::Shutdown => {}
            }
            push_header(frame, self.tag(), &payload)
        }

        /// Decodes a request from its tag byte and payload.
        ///
        /// # Errors
        /// Names the violated bound or the malformed field; trailing bytes are never
        /// accepted.
        pub fn decode(tag: u8, payload: &[u8]) -> Result<Self, String> {
            let mut reader = Reader {
                bytes: payload,
                pos: 0,
            };
            let request = match tag {
                TAG_SPAWN => {
                    let program = reader.read_string("program")?;
                    let cwd = reader.read_string("cwd")?;
                    let cols = reader.read_u16("columns")?;
                    let rows = reader.read_u16("rows")?;
                    let argument_count = reader.read_count("argument count", MAX_ARGC)?;
                    let mut args = Vec::with_capacity(argument_count);
                    for _ in 0..argument_count {
                        args.push(reader.read_string("argument")?);
                    }
                    let env_count = reader.read_count("environment variable count", MAX_ENV)?;
                    let mut env = Vec::with_capacity(env_count);
                    for _ in 0..env_count {
                        let key = reader.read_string("environment key")?;
                        let value = reader.read_string("environment value")?;
                        env.push((key, value));
                    }
                    Self::Spawn(SpawnRequest {
                        program,
                        args,
                        env,
                        cwd,
                        cols,
                        rows,
                    })
                }
                TAG_WRITE => Self::Write {
                    bytes: reader.read_remaining("input payload", MAX_BYTES)?,
                },
                TAG_RESIZE => Self::Resize {
                    cols: reader.read_u16("columns")?,
                    rows: reader.read_u16("rows")?,
                },
                TAG_KILL => Self::Kill,
                TAG_SHUTDOWN => Self::Shutdown,
                other => {
                    return Err(format!("parent frame tag {other} is not a request tag"));
                }
            };
            reader.finish("request payload")?;
            Ok(request)
        }
    }

    impl Response {
        /// The frame's tag byte.
        #[must_use]
        pub fn tag(&self) -> u8 {
            match self {
                Self::Spawned { .. } => TAG_SPAWNED,
                Self::Wrote { .. } => TAG_WROTE,
                Self::Out { .. } => TAG_OUT,
                Self::Exited { .. } => TAG_EXITED,
                Self::Failed { .. } => TAG_FAILED,
                Self::Final { .. } => TAG_FINAL,
            }
        }

        /// Encodes the frame behind its header into `frame`.
        ///
        /// # Errors
        /// Names the violated bound when a field exceeds its cap.
        pub fn encode(&self, frame: &mut Vec<u8>) -> Result<(), String> {
            let mut payload = Vec::new();
            match self {
                Self::Spawned { pid } => payload.extend_from_slice(&pid.to_le_bytes()),
                Self::Wrote { t_ns } => payload.extend_from_slice(&t_ns.to_le_bytes()),
                Self::Out { t_ns, bytes } => {
                    if bytes.len() > MAX_BYTES {
                        return Err(format!(
                            "output chunk has {} bytes, over the {MAX_BYTES} cap",
                            bytes.len()
                        ));
                    }
                    payload.extend_from_slice(&t_ns.to_le_bytes());
                    payload.extend_from_slice(bytes);
                }
                Self::Exited { code } => payload.extend_from_slice(&code.to_le_bytes()),
                Self::Failed { reason } => push_str(&mut payload, "failure reason", reason)?,
                Self::Final { unjoined } => {
                    push_count(&mut payload, unjoined.len(), MAX_NAMES, "thread name count")?;
                    for name in unjoined {
                        push_str(&mut payload, "thread name", name)?;
                    }
                }
            }
            push_header(frame, self.tag(), &payload)
        }

        /// Decodes a response from its tag byte and payload.
        ///
        /// # Errors
        /// Names the violated bound or the malformed field; trailing bytes are never
        /// accepted.
        pub fn decode(tag: u8, payload: &[u8]) -> Result<Self, String> {
            let mut reader = Reader {
                bytes: payload,
                pos: 0,
            };
            let response = match tag {
                TAG_SPAWNED => Self::Spawned {
                    pid: reader.read_u32("child process id")?,
                },
                TAG_WROTE => Self::Wrote {
                    t_ns: reader.read_u64("write stamp")?,
                },
                TAG_OUT => {
                    let t_ns = reader.read_u64("output stamp")?;
                    let bytes = reader.read_remaining("output bytes", MAX_BYTES)?;
                    Self::Out { t_ns, bytes }
                }
                TAG_EXITED => Self::Exited {
                    code: reader.read_u32("exit code")?,
                },
                TAG_FAILED => Self::Failed {
                    reason: reader.read_string("failure reason")?,
                },
                TAG_FINAL => {
                    let name_count = reader.read_count("thread name count", MAX_NAMES)?;
                    let mut unjoined = Vec::with_capacity(name_count);
                    for _ in 0..name_count {
                        unjoined.push(reader.read_string("thread name")?);
                    }
                    Self::Final { unjoined }
                }
                other => {
                    return Err(format!("helper frame tag {other} is not a response tag"));
                }
            };
            reader.finish("response payload")?;
            Ok(response)
        }
    }

    /// The tag's short name, for bounded protocol errors.
    #[must_use]
    pub fn tag_name(tag: u8) -> &'static str {
        match tag {
            TAG_SPAWN => "Spawn",
            TAG_WRITE => "Write",
            TAG_RESIZE => "Resize",
            TAG_KILL => "Kill",
            TAG_SHUTDOWN => "Shutdown",
            TAG_SPAWNED => "Spawned",
            TAG_WROTE => "Wrote",
            TAG_OUT => "Out",
            TAG_EXITED => "Exited",
            TAG_FAILED => "Failed",
            TAG_FINAL => "Final",
            _ => "Unknown",
        }
    }

    /// Validates one frame header and returns its payload length.
    ///
    /// # Errors
    /// Names the bound when the declared length exceeds the payload cap.
    pub fn payload_length(header: &[u8; HEADER_LEN]) -> Result<usize, String> {
        let mut encoded = [0_u8; 4];
        encoded.copy_from_slice(&header[1..]);
        let length = usize::try_from(u32::from_le_bytes(encoded))
            .map_err(|_| "frame length exceeds the address space".to_owned())?;
        if length > MAX_PAYLOAD {
            return Err(format!(
                "a {} frame declares {length} payload bytes, over the {MAX_PAYLOAD} cap",
                tag_name(header[0])
            ));
        }
        Ok(length)
    }

    fn push_header(frame: &mut Vec<u8>, tag: u8, payload: &[u8]) -> Result<(), String> {
        if payload.len() > MAX_PAYLOAD {
            return Err(format!(
                "{} payload has {} bytes, over the {MAX_PAYLOAD} cap",
                tag_name(tag),
                payload.len()
            ));
        }
        frame.push(tag);
        let length = u32::try_from(payload.len())
            .map_err(|_| format!("{} payload length exceeds u32", tag_name(tag)))?;
        frame.extend_from_slice(&length.to_le_bytes());
        frame.extend_from_slice(payload);
        Ok(())
    }

    fn push_str(payload: &mut Vec<u8>, field: &str, value: &str) -> Result<(), String> {
        let bytes = value.as_bytes();
        if bytes.len() > MAX_STRING {
            return Err(format!(
                "{field} has {} bytes, over the {MAX_STRING} cap",
                bytes.len()
            ));
        }
        let length =
            u16::try_from(bytes.len()).map_err(|_| format!("{field} does not fit a u16 length"))?;
        payload.extend_from_slice(&length.to_le_bytes());
        payload.extend_from_slice(bytes);
        Ok(())
    }

    fn push_count(
        payload: &mut Vec<u8>,
        count: usize,
        cap: usize,
        field: &str,
    ) -> Result<(), String> {
        if count > cap {
            return Err(format!("{field} is {count}, over the {cap} cap"));
        }
        let encoded =
            u16::try_from(count).map_err(|_| format!("{field} does not fit a u16 count"))?;
        payload.extend_from_slice(&encoded.to_le_bytes());
        Ok(())
    }

    /// Cursor over one frame payload; every read is bounds-checked.
    struct Reader<'a> {
        bytes: &'a [u8],
        pos: usize,
    }

    impl Reader<'_> {
        fn take(&mut self, count: usize, field: &str) -> Result<&[u8], String> {
            let end = self
                .pos
                .checked_add(count)
                .ok_or_else(|| format!("{field} length overflows the frame"))?;
            if end > self.bytes.len() {
                return Err(format!(
                    "{field} needs {count} bytes but only {} remain in the frame",
                    self.bytes.len() - self.pos
                ));
            }
            let slice = &self.bytes[self.pos..end];
            self.pos = end;
            Ok(slice)
        }

        fn read_u16(&mut self, field: &str) -> Result<u16, String> {
            let mut encoded = [0_u8; 2];
            encoded.copy_from_slice(self.take(2, field)?);
            Ok(u16::from_le_bytes(encoded))
        }

        fn read_u32(&mut self, field: &str) -> Result<u32, String> {
            let mut encoded = [0_u8; 4];
            encoded.copy_from_slice(self.take(4, field)?);
            Ok(u32::from_le_bytes(encoded))
        }

        fn read_u64(&mut self, field: &str) -> Result<u64, String> {
            let mut encoded = [0_u8; 8];
            encoded.copy_from_slice(self.take(8, field)?);
            Ok(u64::from_le_bytes(encoded))
        }

        fn read_count(&mut self, field: &str, cap: usize) -> Result<usize, String> {
            let count = usize::from(self.read_u16(field)?);
            if count > cap {
                return Err(format!("{field} is {count}, over the {cap} cap"));
            }
            Ok(count)
        }

        fn read_string(&mut self, field: &str) -> Result<String, String> {
            let bytes = self.read_rest(field, MAX_STRING)?;
            String::from_utf8(bytes).map_err(|_| format!("{field} is not UTF-8"))
        }

        fn read_rest(&mut self, field: &str, cap: usize) -> Result<Vec<u8>, String> {
            let length = usize::from(self.read_u16(field)?);
            if length > cap {
                return Err(format!("{field} is {length} bytes, over the {cap} cap"));
            }
            self.take(length, field).map(<[u8]>::to_vec)
        }

        fn read_remaining(&mut self, field: &str, cap: usize) -> Result<Vec<u8>, String> {
            let length = self.bytes.len() - self.pos;
            if length > cap {
                return Err(format!("{field} is {length} bytes, over the {cap} cap"));
            }
            self.take(length, field).map(<[u8]>::to_vec)
        }

        fn finish(&mut self, field: &str) -> Result<(), String> {
            if self.pos == self.bytes.len() {
                Ok(())
            } else {
                Err(format!(
                    "{field} carries {} trailing bytes",
                    self.bytes.len() - self.pos
                ))
            }
        }
    }
}

/// Witness for the shared wire codec: every frame is an independent literal of outer bytes
/// (tag, little-endian `u32` length, payload) decoded through the shared functions. Encoder
/// checks compare exact bytes; refusal checks compare the decoded payload bytes or require
/// the refusal to name the stable count and cap facts, never a bare length or a bare `Ok`.
/// `Write` carries raw bytes and `Out` carries a `u64` stamp plus raw bytes, so a payload
/// shaped like a length prefix is still data. The `u16` prefix belongs to string and count
/// fields only.
#[cfg(test)]
mod protocol_witness {
    use super::protocol::{self, Request, Response, SpawnRequest};

    /// Nonsymmetric output stamp `0x1122_3344_5566_7788`, little-endian on the wire: a byte
    /// swap or a 32-bit read cannot pass unseen.
    const STAMP: [u8; 8] = [0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11];

    /// The value of [`STAMP`].
    const STAMP_NS: u64 = 0x1122_3344_5566_7788;

    /// Joins byte slices into one synthetic frame.
    fn join(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    /// A deterministic payload of `len` bytes starting `00 01 02`: its first two bytes read
    /// as the `u16` length 256, which no frame carrying it declares.
    fn pattern(len: usize) -> Vec<u8> {
        (0..=250_u8).cycle().take(len).collect()
    }

    /// Splits one complete frame into its tag and payload: the header goes through the shared
    /// [`protocol::payload_length`] bound and the payload must fill the declared length.
    fn split(frame: &[u8]) -> Result<(u8, &[u8]), String> {
        let (header, payload) = frame
            .split_first_chunk::<{ protocol::HEADER_LEN }>()
            .ok_or_else(|| format!("frame has {} bytes, short of its header", frame.len()))?;
        let length = protocol::payload_length(header)?;
        if payload.len() != length {
            return Err(format!(
                "header declares {length} payload bytes but the frame carries {}",
                payload.len()
            ));
        }
        Ok((header[0], payload))
    }

    /// Encodes `request` through the shared encoder.
    fn encode_request(request: &Request) -> Result<Vec<u8>, String> {
        let mut frame = Vec::new();
        request.encode(&mut frame)?;
        Ok(frame)
    }

    /// Encodes `response` through the shared encoder.
    fn encode_response(response: &Response) -> Result<Vec<u8>, String> {
        let mut frame = Vec::new();
        response.encode(&mut frame)?;
        Ok(frame)
    }

    /// Decodes `frame` as a request and encodes the result again.
    fn reencode_request(frame: &[u8]) -> Result<Vec<u8>, String> {
        let (tag, payload) = split(frame)?;
        encode_request(&Request::decode(tag, payload)?)
    }

    /// Decodes `frame` as a response and encodes the result again.
    fn reencode_response(frame: &[u8]) -> Result<Vec<u8>, String> {
        let (tag, payload) = split(frame)?;
        encode_response(&Response::decode(tag, payload)?)
    }

    /// The bytes a complete `Write` frame decodes to.
    fn decode_write(frame: &[u8]) -> Result<Vec<u8>, String> {
        let (tag, payload) = split(frame)?;
        let Request::Write { bytes } = Request::decode(tag, payload)? else {
            return Err("the frame did not decode as Write".to_owned());
        };
        Ok(bytes)
    }

    /// The stamp and bytes a complete `Out` frame decodes to.
    fn decode_out(frame: &[u8]) -> Result<(u64, Vec<u8>), String> {
        let (tag, payload) = split(frame)?;
        let Response::Out { t_ns, bytes } = Response::decode(tag, payload)? else {
            return Err("the frame did not decode as Out".to_owned());
        };
        Ok((t_ns, bytes))
    }

    /// Requires a refusal whose text names every stable fact in `facts`.
    fn refused<T>(outcome: Result<T, String>, facts: &[&str]) {
        match outcome {
            Ok(_) => panic!("expected a refusal, the frame decoded successfully"),
            Err(error) => {
                for fact in facts {
                    assert!(
                        error.contains(*fact),
                        "refusal {error:?} lacks stable fact {fact:?}"
                    );
                }
            }
        }
    }

    /// Pins one `Write` frame both ways: the encoder emits exactly `frame` for `bytes`, and
    /// the decoder reads exactly `bytes` out of `frame`.
    fn assert_write(frame: &[u8], bytes: &[u8]) {
        let request = Request::Write {
            bytes: bytes.to_vec(),
        };
        assert_eq!(encode_request(&request), Ok(frame.to_vec()), "encoded");
        assert_eq!(decode_write(frame), Ok(bytes.to_vec()), "decoded");
    }

    /// Pins one `Out` frame both ways with the nonsymmetric [`STAMP`].
    fn assert_out(frame: &[u8], bytes: &[u8]) {
        let response = Response::Out {
            t_ns: STAMP_NS,
            bytes: bytes.to_vec(),
        };
        assert_eq!(encode_response(&response), Ok(frame.to_vec()), "encoded");
        let expected = Ok((STAMP_NS, bytes.to_vec()));
        assert_eq!(decode_out(frame), expected, "decoded");
    }

    /// Pins one fixed-layout request: encoder bytes, then decode and encode.
    fn assert_request(request: &Request, frame: &[u8]) {
        assert_eq!(encode_request(request), Ok(frame.to_vec()), "encoded");
        assert_eq!(reencode_request(frame), Ok(frame.to_vec()), "re-encoded");
    }

    /// Pins one fixed-layout response: encoder bytes, then decode and encode.
    fn assert_response(response: &Response, frame: &[u8]) {
        assert_eq!(encode_response(response), Ok(frame.to_vec()), "encoded");
        assert_eq!(reencode_response(frame), Ok(frame.to_vec()), "re-encoded");
    }

    /// One ASCII byte, the smallest keypress, is the whole payload.
    #[test]
    fn write_one_ascii_byte_is_the_whole_payload() {
        let frame = join(&[&[0x02, 0x01, 0x00, 0x00, 0x00], b"A"]);
        assert_write(&frame, b"A");
    }

    /// Multibyte UTF-8 input crosses the wire as its raw bytes.
    #[test]
    fn write_multibyte_utf8_is_raw_bytes() {
        let cjk = join(&[&[0x02, 0x03, 0x00, 0x00, 0x00], b"\xe4\xb8\x80"]);
        assert_write(&cjk, b"\xe4\xb8\x80");
        let emoji = join(&[&[0x02, 0x04, 0x00, 0x00, 0x00], b"\xf0\x9f\x98\x80"]);
        assert_write(&emoji, b"\xf0\x9f\x98\x80");
    }

    /// A payload shaped like a length prefix is still data.
    #[test]
    fn write_payload_shaped_like_a_prefix_is_data() {
        let two = join(&[&[0x02, 0x04, 0x00, 0x00, 0x00], b"\x02\x00AB"]);
        assert_write(&two, b"\x02\x00AB");
        let one = join(&[&[0x02, 0x03, 0x00, 0x00, 0x00], b"\x01\x00a"]);
        assert_write(&one, b"\x01\x00a");
    }

    /// Leading control bytes survive: an arrow-key escape sequence, a CRLF pair, and leading
    /// zeros each decode to exactly the bytes sent.
    #[test]
    fn write_leading_escape_crlf_and_zeros_are_raw() {
        let arrow = join(&[&[0x02, 0x03, 0x00, 0x00, 0x00], b"\x1b[A"]);
        assert_write(&arrow, b"\x1b[A");
        let enter = join(&[&[0x02, 0x02, 0x00, 0x00, 0x00], b"\r\n"]);
        assert_write(&enter, b"\r\n");
        let zeros = join(&[&[0x02, 0x03, 0x00, 0x00, 0x00], b"\x00\x00A"]);
        assert_write(&zeros, b"\x00\x00A");
    }

    /// An empty write is a header with no payload and decodes to no bytes.
    #[test]
    fn write_empty_payload_is_no_bytes() {
        assert_write(&[0x02, 0x00, 0x00, 0x00, 0x00], b"");
    }

    /// The payload is binary, never text: bytes that are not valid UTF-8 pass through.
    #[test]
    fn write_bytes_that_are_not_utf8_pass_through() {
        let frame = join(&[&[0x02, 0x03, 0x00, 0x00, 0x00], b"\xff\xfe\x80"]);
        assert_write(&frame, b"\xff\xfe\x80");
    }

    /// The cap is the raw byte count: exactly `MAX_BYTES` bytes round-trip.
    #[test]
    fn write_exact_cap_round_trips() {
        let payload = pattern(protocol::MAX_BYTES);
        let head: [u8; 5] = [0x02, 0x00, 0x10, 0x00, 0x00];
        let frame = join(&[&head, payload.as_slice()]);
        assert_write(&frame, &payload);
    }

    /// One byte over the cap is refused by the encoder and by the decoder, and the refusals
    /// name the actual handed count and the cap.
    #[test]
    fn write_one_byte_over_the_cap_is_refused() {
        let payload = pattern(protocol::MAX_BYTES + 1);
        let request = Request::Write {
            bytes: payload.clone(),
        };
        refused(encode_request(&request), &["4097", "4096"]);
        let head: [u8; 5] = [0x02, 0x01, 0x10, 0x00, 0x00];
        let frame = join(&[&head, payload.as_slice()]);
        refused(decode_write(&frame), &["4097", "4096"]);
    }

    /// A frame filled to the `MAX_PAYLOAD` header bound passes the header check and is then
    /// refused by its actual `Write` byte count.
    #[test]
    fn write_frame_at_the_payload_cap_is_refused_by_byte_count() {
        let payload = pattern(protocol::MAX_PAYLOAD);
        let head: [u8; 5] = [0x02, 0x00, 0x00, 0x01, 0x00];
        let frame = join(&[&head, payload.as_slice()]);
        refused(decode_write(&frame), &["65536", "4096"]);
    }

    /// Ordinary console text follows the stamp.
    #[test]
    fn out_ascii_bytes_follow_the_nonsymmetric_stamp() {
        let head: [u8; 5] = [0x08, 0x0a, 0x00, 0x00, 0x00];
        let frame = join(&[&head, &STAMP, b"Hi"]);
        assert_out(&frame, b"Hi");
    }

    /// Multibyte UTF-8 output is raw bytes, including a character cut in half by read chunking.
    #[test]
    fn out_multibyte_utf8_is_raw_bytes_even_cut_mid_character() {
        let whole: [u8; 5] = [0x08, 0x0b, 0x00, 0x00, 0x00];
        let glyph = join(&[&whole, &STAMP, b"\xe4\xb8\x80"]);
        assert_out(&glyph, b"\xe4\xb8\x80");
        let half: [u8; 5] = [0x08, 0x0a, 0x00, 0x00, 0x00];
        let cut = join(&[&half, &STAMP, b"\xe4\xb8"]);
        assert_out(&cut, b"\xe4\xb8");
    }

    /// Leading control bytes survive: a VT escape, a CRLF pair, and leading zeros each decode
    /// to exactly the bytes read.
    #[test]
    fn out_leading_escape_crlf_and_zeros_are_raw() {
        let head: [u8; 5] = [0x08, 0x0c, 0x00, 0x00, 0x00];
        let escape = join(&[&head, &STAMP, b"\x1b[2J"]);
        assert_out(&escape, b"\x1b[2J");
        let crlf = join(&[&head, &STAMP, b"\r\nHi"]);
        assert_out(&crlf, b"\r\nHi");
        let zeros = join(&[&head, &STAMP, b"\x00\x00A\x00"]);
        assert_out(&zeros, b"\x00\x00A\x00");
    }

    /// An empty chunk is the stamp alone and decodes to no bytes.
    #[test]
    fn out_empty_bytes_are_the_stamp_alone() {
        let head: [u8; 5] = [0x08, 0x08, 0x00, 0x00, 0x00];
        let frame = join(&[&head, &STAMP]);
        assert_out(&frame, b"");
    }

    /// A chunk shaped like a length prefix is still data.
    #[test]
    fn out_payload_shaped_like_a_prefix_is_data() {
        let head: [u8; 5] = [0x08, 0x0d, 0x00, 0x00, 0x00];
        let frame = join(&[&head, &STAMP, b"\x03\x00abc"]);
        assert_out(&frame, b"\x03\x00abc");
    }

    /// The cap counts the raw bytes after the stamp: exactly `MAX_BYTES` bytes round-trip.
    #[test]
    fn out_exact_cap_round_trips() {
        let bytes = pattern(protocol::MAX_BYTES);
        let head: [u8; 5] = [0x08, 0x08, 0x10, 0x00, 0x00];
        let frame = join(&[&head, &STAMP, bytes.as_slice()]);
        assert_out(&frame, &bytes);
    }

    /// One byte over the cap is refused by the encoder and by the decoder, and the refusals
    /// name the actual byte count after the stamp and the cap.
    #[test]
    fn out_one_byte_over_the_cap_is_refused() {
        let bytes = pattern(protocol::MAX_BYTES + 1);
        let response = Response::Out {
            t_ns: STAMP_NS,
            bytes: bytes.clone(),
        };
        refused(encode_response(&response), &["4097", "4096"]);
        let head: [u8; 5] = [0x08, 0x09, 0x10, 0x00, 0x00];
        let frame = join(&[&head, &STAMP, bytes.as_slice()]);
        refused(decode_out(&frame), &["4097", "4096"]);
    }

    /// A frame filled to the `MAX_PAYLOAD` header bound passes the header check and is then
    /// refused by its byte count after the stamp.
    #[test]
    fn out_frame_at_the_payload_cap_is_refused_by_byte_count() {
        let bytes = pattern(protocol::MAX_PAYLOAD - STAMP.len());
        let head: [u8; 5] = [0x08, 0x00, 0x00, 0x01, 0x00];
        let frame = join(&[&head, &STAMP, bytes.as_slice()]);
        refused(decode_out(&frame), &["65528", "4096"]);
    }

    /// A payload shorter than the stamp keeps its truncation refusal.
    #[test]
    fn out_truncated_stamp_is_refused() {
        let head: [u8; 5] = [0x08, 0x07, 0x00, 0x00, 0x00];
        let frame = join(&[&head, &STAMP[..7]]);
        refused(decode_out(&frame), &["output stamp", "8", "7"]);
    }

    /// String and count fields keep their `u16` length prefix, in every message that carries
    /// them.
    #[test]
    fn string_fields_keep_the_u16_prefix() {
        let boom = join(&[&[0x0a, 0x06, 0x00, 0x00, 0x00], b"\x04\x00boom"]);
        let failed = Response::Failed {
            reason: "boom".to_owned(),
        };
        assert_response(&failed, &boom);
        let blank: [u8; 7] = [0x0a, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00];
        let silent = Response::Failed {
            reason: String::new(),
        };
        assert_response(&silent, &blank);
        let names = join(&[
            &[0x0b, 0x09, 0x00, 0x00, 0x00],
            b"\x02\x00\x01\x00a\x02\x00bc",
        ]);
        let listed = Response::Final {
            unjoined: vec!["a".to_owned(), "bc".to_owned()],
        };
        assert_response(&listed, &names);
        let head: [u8; 5] = [0x01, 0x17, 0x00, 0x00, 0x00];
        let body = b"\x01\x00a\x01\x00b\x78\x00\x28\x00\x01\x00\x01\x00c\x01\x00\x01\x00k\x01\x00v";
        let spawn = Request::Spawn(SpawnRequest {
            program: "a".to_owned(),
            args: vec!["c".to_owned()],
            env: vec![("k".to_owned(), "v".to_owned())],
            cwd: "b".to_owned(),
            cols: 120,
            rows: 40,
        });
        assert_request(&spawn, &join(&[&head, body]));
    }

    /// String bounds stay enforced: a prefix longer than the frame, a prefix over the string
    /// cap, and non-UTF-8 text are each refused.
    #[test]
    fn string_bounds_and_utf8_stay_enforced() {
        let head: [u8; 5] = [0x0a, 0x04, 0x00, 0x00, 0x00];
        let lie = join(&[&head, b"\x04\x00bo"]);
        refused(reencode_response(&lie), &["failure reason", "4", "2"]);
        let over = join(&[&head, b"\x01\x40ab"]);
        refused(
            reencode_response(&over),
            &["failure reason", "16385", "16384"],
        );
        let binary = join(&[&head, b"\x02\x00\xff\xfe"]);
        refused(reencode_response(&binary), &["failure reason", "UTF-8"]);
    }

    /// Trailing bytes stay refused on every fixed-layout and string frame.
    #[test]
    fn trailing_bytes_stay_refused() {
        let failed = join(&[&[0x0a, 0x07, 0x00, 0x00, 0x00], b"\x04\x00boom!"]);
        let resize: [u8; 10] = [0x03, 0x05, 0x00, 0x00, 0x00, 0x78, 0x00, 0x28, 0x00, 0xff];
        let kill: [u8; 6] = [0x04, 0x01, 0x00, 0x00, 0x00, 0x00];
        let exited: [u8; 10] = [0x09, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff];
        refused(reencode_response(&failed), &["trailing", "1"]);
        refused(reencode_request(&resize), &["trailing", "1"]);
        refused(reencode_request(&kill), &["trailing", "1"]);
        refused(reencode_response(&exited), &["trailing", "1"]);
    }

    /// Count caps stay enforced.
    #[test]
    fn count_caps_stay_enforced() {
        let names = join(&[&[0x0b, 0x02, 0x00, 0x00, 0x00], &[0x11, 0x00]]);
        refused(reencode_response(&names), &["17", "16"]);
        let head: [u8; 5] = [0x01, 0x0a, 0x00, 0x00, 0x00];
        let spawn = join(&[&head, &[0; 8], &[0x21, 0x00]]);
        refused(reencode_request(&spawn), &["33", "32"]);
    }

    /// Fixed-width frames keep their exact bytes in both directions.
    #[test]
    fn fixed_width_frames_keep_their_bytes() {
        let resize = Request::Resize {
            cols: 120,
            rows: 40,
        };
        let resize_frame: [u8; 9] = [0x03, 0x04, 0x00, 0x00, 0x00, 0x78, 0x00, 0x28, 0x00];
        assert_request(&resize, &resize_frame);
        assert_request(&Request::Kill, &[0x04, 0x00, 0x00, 0x00, 0x00]);
        assert_request(&Request::Shutdown, &[0x05, 0x00, 0x00, 0x00, 0x00]);
        let spawned = Response::Spawned { pid: 1234 };
        let spawned_frame: [u8; 9] = [0x06, 0x04, 0x00, 0x00, 0x00, 0xd2, 0x04, 0x00, 0x00];
        assert_response(&spawned, &spawned_frame);
        let wrote = Response::Wrote { t_ns: 42 };
        let wrote_frame: [u8; 13] = [
            0x07, 0x08, 0x00, 0x00, 0x00, 0x2a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        assert_response(&wrote, &wrote_frame);
        let exited = Response::Exited { code: 0 };
        let exited_frame: [u8; 9] = [0x09, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_response(&exited, &exited_frame);
    }

    /// The frame header is bounded from its five bytes alone, before any payload exists.
    #[test]
    fn header_bound_is_refused_from_five_bytes_alone() {
        let at_cap: [u8; 5] = [0x02, 0x00, 0x00, 0x01, 0x00];
        assert_eq!(protocol::payload_length(&at_cap), Ok(65_536));
        let over: [u8; 5] = [0x08, 0x01, 0x00, 0x01, 0x00];
        refused(protocol::payload_length(&over), &["65537", "65536"]);
    }

    /// `Write` is a request tag only and `Out` a response tag only.
    #[test]
    fn tags_stay_directional() {
        refused(Request::decode(8, &STAMP), &["8", "request"]);
        refused(Response::decode(2, b"A"), &["2", "response"]);
    }

    /// The documented bounds are the frozen wire constants.
    #[test]
    fn documented_bounds_are_the_frozen_constants() {
        assert_eq!(protocol::HEADER_LEN, 5);
        assert_eq!(protocol::MAX_PAYLOAD, 65_536);
        assert_eq!(protocol::MAX_BYTES, 4_096);
    }
}

/// Structural VT paint-batch classifier.
///
/// A glyph counts as application paint only when a completed render batch carries it: a
/// cursor-addressed region the renderer cleared with an erase and has not closed yet, bounded
/// by the erase that follows the region's paint, or a synchronized-update envelope observed as
/// a whole. Region state never leaks across the batch boundary: after the closing erase the
/// region is done, and later text inherits nothing. Bare console echo is text with no renderer
/// addressing behind it at all.
pub mod paint {
    /// One scanned element of a VT stream.
    #[derive(Debug, PartialEq, Eq)]
    enum Element<'a> {
        /// Cursor addressing (CUP/HVP): opens a paint region.
        Cup,
        /// Erase in display or line: arms the open addressed region, or closes the region
        /// after its paint.
        Erase,
        /// Synchronized-update envelope boundary; the payload says whether it begins or ends.
        Sync(bool),
        /// Any other escape sequence.
        Other,
        /// A run of bytes outside every sequence.
        Text(&'a [u8]),
    }

    /// Whether `needle` occurs in `stream` inside an application paint batch: text in a
    /// cursor-addressed region the renderer erased and has not closed yet, or text bracketed
    /// by a completed synchronized-update envelope. An empty needle is never found.
    #[must_use]
    pub fn contains_painted(stream: &[u8], needle: &[u8]) -> bool {
        let (painted, _) = classify(stream, needle);
        painted
    }

    /// Whether `needle` occurs in `stream` as unaddressed text: bytes with no renderer cursor
    /// addressing before them and no synchronized-update envelope around them. An empty needle
    /// is never found.
    #[must_use]
    pub fn contains_unaddressed(stream: &[u8], needle: &[u8]) -> bool {
        let (_, unaddressed) = classify(stream, needle);
        unaddressed
    }

    /// Computes both occurrence classes for `needle` in one scan: a cursor address opens a
    /// paint region; the first erase in a region arms it; text in the armed region is that
    /// region's paint; the next erase after painted text closes the region, and only a fresh
    /// cursor address reopens one. A synchronized-update envelope contributes only when the
    /// batch is observed complete, with both boundaries inside the window.
    fn classify(stream: &[u8], needle: &[u8]) -> (bool, bool) {
        if needle.is_empty() {
            return (false, false);
        }
        let elements = scan(stream);
        let mut painted = false;
        let mut unaddressed = false;
        let mut addressed = false;
        let mut region_armed = false;
        let mut region_text_seen = false;
        let mut region_closed = false;
        let mut sync_open = false;
        let mut sync_hit = false;
        for element in &elements {
            match element {
                Element::Cup => {
                    addressed = true;
                    region_armed = false;
                    region_text_seen = false;
                    region_closed = false;
                }
                Element::Erase => {
                    if !region_closed {
                        if region_armed && region_text_seen {
                            region_closed = true;
                            region_armed = false;
                            region_text_seen = false;
                        } else {
                            region_armed = true;
                        }
                    }
                }
                Element::Sync(begins) => {
                    if *begins {
                        sync_open = true;
                        sync_hit = false;
                    } else if sync_open {
                        if sync_hit {
                            painted = true;
                        }
                        sync_open = false;
                        sync_hit = false;
                    }
                }
                Element::Other => {}
                Element::Text(bytes) => {
                    if region_armed {
                        region_text_seen = true;
                    }
                    if bytes.windows(needle.len()).any(|window| window == needle) {
                        if sync_open {
                            sync_hit = true;
                        }
                        if region_armed {
                            painted = true;
                        } else if !addressed && !sync_open {
                            unaddressed = true;
                        }
                    }
                }
            }
        }
        (painted, unaddressed)
    }

    /// Splits `stream` into elements. A truncated or malformed sequence at the edge degrades
    /// to text, which can only ever reject a sample, never promote one.
    fn scan(stream: &[u8]) -> Vec<Element<'_>> {
        let mut elements = Vec::new();
        let mut run_start: Option<usize> = None;
        let mut index = 0;
        while index < stream.len() {
            if stream[index] == 0x1B
                && let Some((length, element)) = parse_sequence(&stream[index..])
            {
                if let Some(start) = run_start.take() {
                    elements.push(Element::Text(&stream[start..index]));
                }
                elements.push(element);
                index += length;
                continue;
            }
            if run_start.is_none() {
                run_start = Some(index);
            }
            index += 1;
        }
        if let Some(start) = run_start.take() {
            elements.push(Element::Text(&stream[start..]));
        }
        elements
    }

    /// Parses one escape sequence at the front of `bytes` into its length and class, or
    /// returns `None` when the sequence is truncated or invalid.
    fn parse_sequence(bytes: &[u8]) -> Option<(usize, Element<'_>)> {
        if bytes.len() < 2 {
            return None;
        }
        match bytes[1] {
            b'[' => {
                let mut index = 2;
                while index < bytes.len() && (0x30..=0x3F).contains(&bytes[index]) {
                    index += 1;
                }
                let params_end = index;
                while index < bytes.len() && (0x20..=0x2F).contains(&bytes[index]) {
                    index += 1;
                }
                let final_byte = *bytes.get(index)?;
                if !(0x40..=0x7E).contains(&final_byte) {
                    return None;
                }
                let params = &bytes[2..params_end];
                let element = match final_byte {
                    b'H' | b'f' => Element::Cup,
                    b'J' | b'K' => Element::Erase,
                    b'h' if params == b"?2026" => Element::Sync(true),
                    b'l' if params == b"?2026" => Element::Sync(false),
                    _ => Element::Other,
                };
                Some((index + 1, element))
            }
            b']' => {
                let mut index = 2;
                while index < bytes.len() {
                    match bytes[index] {
                        0x07 => break,
                        0x1B if bytes.get(index + 1) == Some(&b'\\') => {
                            index += 1;
                            break;
                        }
                        _ => index += 1,
                    }
                }
                if index >= bytes.len() {
                    return None;
                }
                Some((index + 1, Element::Other))
            }
            _ => Some((2, Element::Other)),
        }
    }
}

/// Witness for the paint classifier: synthetic streams pin the classifier's verdicts. Every
/// test drives the classifier with the stream bytes themselves, so each pinned verdict is an
/// observed behavior of the paint grammar.
#[cfg(test)]
mod paint_witness {
    use super::paint;

    /// Joins byte slices into one synthetic VT stream.
    fn join(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    /// The sample glyph, CJK U+4E00, as raw UTF-8.
    const GLYPH: &[u8] = "\u{4e00}".as_bytes();

    /// The never-painted needle of the leak witness, CJK U+4E01.
    const NEW_GLYPH: &[u8] = "\u{4e01}".as_bytes();

    /// The needle inside the truncated envelope, CJK U+4E02.
    const SYNC_GLYPH: &[u8] = "\u{4e02}".as_bytes();

    /// The needle before a midstream envelope close, CJK U+4E03.
    const TAIL_GLYPH: &[u8] = "\u{4e03}".as_bytes();

    /// Stale cursor addressing from an earlier frame sits right before bare echo. The glyph's
    /// region carries no erase and is never re-addressed, so the classifier neither promotes
    /// it to paint nor calls it echo.
    #[test]
    fn stale_addressing_with_bare_echo_is_never_paint() {
        let stream = join(&[b"\x1b[2;1H\x1b[38;5;9mstale frame\r\n", GLYPH, b"\r\n"]);
        assert!(!paint::contains_painted(&stream, GLYPH));
        assert!(!paint::contains_unaddressed(&stream, GLYPH));
    }

    /// Escape bytes with no cursor addressing at all, then bare echo: the glyph lands in the
    /// echo class.
    #[test]
    fn bare_echo_without_addressing_is_the_echo_class() {
        let stream = join(&[b"\x1b[38;5;9mold sgr state plain text\r\n", GLYPH, b"\r\n"]);
        assert!(!paint::contains_painted(&stream, GLYPH));
        assert!(paint::contains_unaddressed(&stream, GLYPH));
    }

    /// A stale addressed region later re-addressed still does not promote the echoed glyph:
    /// without an erase in the region the batch was never a completed repaint.
    #[test]
    fn stale_region_readdressed_without_erase_stays_rejected() {
        let stream = join(&[b"\x1b[2;1H", GLYPH, b"\r\n\x1b[1;1H"]);
        assert!(!paint::contains_painted(&stream, GLYPH));
        assert!(!paint::contains_unaddressed(&stream, GLYPH));
    }

    /// A real input-frame repaint: cursor address, erase, style, the glyph, a trailing erase.
    #[test]
    fn addressed_repaint_with_erase_is_paint() {
        let stream = join(&[b"\x1b[12;7H\x1b[K\x1b[0m", GLYPH, b"\x1b[K"]);
        assert!(paint::contains_painted(&stream, GLYPH));
        assert!(!paint::contains_unaddressed(&stream, GLYPH));
    }

    /// A synchronized-update envelope proves the application frame by itself.
    #[test]
    fn synchronized_update_batch_is_paint() {
        let stream = join(&[b"\x1b[?2026h", GLYPH, b"\x1b[?2026l"]);
        assert!(paint::contains_painted(&stream, GLYPH));
        assert!(!paint::contains_unaddressed(&stream, GLYPH));
    }

    /// A sequence truncated by the window edge degrades to text, so it can neither create
    /// paint nor hide echo.
    #[test]
    fn truncated_sequence_is_conservative() {
        let stream = join(&[b"\x1b[12;7", GLYPH]);
        assert!(!paint::contains_painted(&stream, GLYPH));
        assert!(paint::contains_unaddressed(&stream, GLYPH));
    }

    /// A genuine repaint of one glyph closes at its erase, and a different glyph printed after
    /// that closing erase inherits nothing from the closed region.
    #[test]
    fn closed_region_state_does_not_leak_to_a_new_glyph() {
        let stream = join(&[b"\x1b[12;7H\x1b[K", GLYPH, b"\x1b[K", NEW_GLYPH]);
        assert!(paint::contains_painted(&stream, GLYPH));
        assert!(!paint::contains_painted(&stream, NEW_GLYPH));
        assert!(!paint::contains_unaddressed(&stream, NEW_GLYPH));
    }

    /// A synchronized-update envelope that opens but never closes inside the window is an
    /// incomplete render batch: the glyph inside is neither paint nor echo.
    #[test]
    fn sync_start_without_end_is_not_a_complete_frame() {
        let stream = join(&[b"\x1b[?2026h", SYNC_GLYPH]);
        assert!(!paint::contains_painted(&stream, SYNC_GLYPH));
        assert!(!paint::contains_unaddressed(&stream, SYNC_GLYPH));
    }

    /// A window that starts midstream can open on a bare close whose start predates the
    /// capture: with no in-window start the close promotes nothing.
    #[test]
    fn sync_close_without_in_window_start_promotes_nothing() {
        let stream = join(&[TAIL_GLYPH, b"\x1b[?2026l"]);
        assert!(!paint::contains_painted(&stream, TAIL_GLYPH));
        assert!(paint::contains_unaddressed(&stream, TAIL_GLYPH));
    }
}

#[cfg(unix)]
pub use unix::keypress_echo_latencies;

#[cfg(unix)]
mod unix {
    use std::os::fd::{AsFd, OwnedFd};
    use std::path::Path;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    use tokio::io::Interest;
    use tokio::io::unix::AsyncFd;
    use tokio::process::Command;
    use tokio::time::timeout;

    use crate::support::TestResult;

    /// Columns of the pseudo-terminal the child runs in.
    const COLUMNS: u16 = 120;
    /// Rows of the pseudo-terminal the child runs in.
    const ROWS: u16 = 40;
    /// First code point of the sample glyph range (CJK Unified Ideographs).
    const GLYPH_BASE: u32 = 0x4E00;
    /// Last code point of the sample glyph range.
    const GLYPH_LAST: u32 = 0x9FFF;
    /// Largest number of distinct sample glyphs: `0x9FFF - 0x4E00 + 1`.
    const GLYPH_COUNT: usize = 0x5200;
    /// Read buffer size for one master readiness slice.
    const READ_CHUNK: usize = 4096;
    /// Cap on the rolling match window retained between master reads.
    const WINDOW_CAP: usize = 16 * READ_CHUNK;
    /// Bytes kept across window trims so a glyph split by a trim still matches.
    const SPLIT_KEEP: usize = 3;
    /// Bound on the wait for a process-group kill to be reaped.
    const REAP_LIMIT: Duration = Duration::from_secs(5);

    /// Master output read in one readiness slice, stamped on arrival.
    struct Chunk {
        /// When the read syscall returned with these bytes.
        arrived: Instant,
        /// The bytes themselves.
        bytes: Vec<u8>,
    }

    /// One bounded master read outcome.
    enum Output {
        /// Output arrived.
        Chunk(Chunk),
        /// No output for the whole wait.
        Quiet,
        /// The child side of the terminal closed: end of file or `EIO` on the master.
        Closed,
    }

    /// Sample count and wait budgets for one probe run.
    #[derive(Clone, Copy)]
    struct Tuning {
        /// Distinct glyphs to feed and measure.
        samples: usize,
        /// Quiet time that separates phases of child output.
        settle: Duration,
        /// Deadline for startup, one keypress write, and one glyph appearance.
        limit: Duration,
    }

    /// Bounded rolling window of recent master output used for glyph matching.
    #[derive(Default)]
    struct Rolling {
        bytes: Vec<u8>,
    }

    impl Rolling {
        /// Appends a chunk and trims the window to [`WINDOW_CAP`], always keeping the
        /// last [`SPLIT_KEEP`] bytes so a glyph split across a trim still matches.
        fn absorb(&mut self, chunk: &Chunk) {
            self.bytes.extend_from_slice(&chunk.bytes);
            let excess = self.bytes.len().saturating_sub(WINDOW_CAP);
            if excess > 0 {
                let droppable = self.bytes.len().saturating_sub(SPLIT_KEEP);
                self.bytes.drain(..excess.min(droppable));
            }
        }

        /// Whether `needle` occurs anywhere in the current window.
        fn contains(&self, needle: &[u8]) -> bool {
            self.bytes
                .windows(needle.len())
                .any(|window| window == needle)
        }
    }

    /// Owns the spawned child's process group on every path through the probe.
    ///
    /// `Drop` sends `SIGKILL` to the child and to its whole group immediately and
    /// without waiting; whether those signals take effect or the child was reaped is
    /// never observed on that path. The reap after a drop falls back to tokio's
    /// runtime-owned orphan reaper via `kill_on_drop`. `terminate` is the only path
    /// that observes completion: it reaps the direct child under [`REAP_LIMIT`] and
    /// errors out naming cleanup as failed when it cannot be observed.
    struct GroupOwner {
        /// The direct child, killed and reaped through tokio's bounded reaper.
        child: tokio::process::Child,
        /// The child's process group, disarmed once the child was reaped.
        group: Option<rustix::process::Pid>,
    }

    impl GroupOwner {
        /// Kills the whole process group, reaps the child within [`REAP_LIMIT`], and
        /// disarms the drop path once the reap succeeded.
        ///
        /// Success means all three happened: the group `SIGKILL` was accepted by the
        /// kernel (or `ESRCH` showed the group already gone), `wait` returned a final
        /// status within [`REAP_LIMIT`], and `group` was disarmed before yielding. Any
        /// other outcome is a typed error that names the cleanup as failed or
        /// unverified; it never claims a reaped child or a dead group.
        async fn terminate(mut self, binary: &Path) -> TestResult<()> {
            let label = binary.display();
            if let Err(error) = Self::kill_group(self.group)
                && error != rustix::io::Errno::SRCH
            {
                return Err(format!(
                    "cleanup failed for {label}: sending SIGKILL to its process group \
                     returned {error}; whether the child or the group is gone is unverified"
                )
                .into());
            }
            match timeout(REAP_LIMIT, self.child.wait()).await {
                Ok(Ok(_status)) => {
                    self.group = None;
                    Ok(())
                }
                Ok(Err(error)) => {
                    self.group = None;
                    Err(format!(
                        "cleanup failed for {label}: the process-group kill was sent but \
                         reaping returned {error}; the reap is unverified and the recorded \
                         group id is surrendered instead of signalling it blind"
                    )
                    .into())
                }
                Err(_elapsed) => Err(format!(
                    "cleanup failed for {label}: still alive {REAP_LIMIT:?} after a SIGKILL \
                     to its process group; the reap was not observed and whether the group \
                     is gone is unverified"
                )
                .into()),
            }
        }

        /// Sends `SIGKILL` to the whole group; nothing is sent without a recorded id.
        fn kill_group(group: Option<rustix::process::Pid>) -> rustix::io::Result<()> {
            match group {
                Some(group) => {
                    rustix::process::kill_process_group(group, rustix::process::Signal::KILL)
                }
                None => Ok(()),
            }
        }
    }

    impl Drop for GroupOwner {
        fn drop(&mut self) {
            let _ = self.child.start_kill();
            let _ = Self::kill_group(self.group);
        }
    }

    /// Starts `binary` in a pseudo-terminal, lets it run, writes one unique glyph per
    /// sample, and returns the latency from write to the glyph's first appearance in
    /// the terminal output.
    ///
    /// The child runs in a 120 by 40 pseudo-terminal as the leader of its own process
    /// group. Kernel echo must be disabled by the child before sampling, so every
    /// echoed glyph comes from the child. The caller runs this under a tokio runtime
    /// with the I/O and time drivers enabled; the master is driven asynchronously, so
    /// no extra thread, task, or channel exists anywhere in the probe.
    ///
    /// # Errors
    /// Returns an error when `samples` is zero, the pseudo-terminal cannot be opened,
    /// the child cannot be spawned, the child produces no output within `limit`,
    /// kernel echo is still on before a sample, a glyph does not appear within
    /// `limit`, or the teardown cannot be observed (a group-kill error other than
    /// `ESRCH`, a reap error, or a reap overrun). Teardown errors name the cleanup as
    /// failed or unverified, so a caller can never claim a reaped child, a dead
    /// group, or a returned fd baseline while cleanup failed.
    pub async fn keypress_echo_latencies(
        binary: &Path,
        args: &[&str],
        env: &[(&str, &str)],
        cwd: &Path,
        samples: usize,
        settle: Duration,
        limit: Duration,
    ) -> TestResult<Vec<Duration>> {
        if samples == 0 {
            return Err(
                "the keypress probe needs a positive sample count; the stress caller \
                 requires 100"
                    .into(),
            );
        }
        let master =
            rustix::pty::openpt(rustix::pty::OpenptFlags::RDWR | rustix::pty::OpenptFlags::NOCTTY)?;
        rustix::io::fcntl_setfd(&master, rustix::io::FdFlags::CLOEXEC)?;
        let nonblocking = rustix::fs::fcntl_getfl(&master)? | rustix::fs::OFlags::NONBLOCK;
        rustix::fs::fcntl_setfl(&master, nonblocking)?;
        rustix::pty::grantpt(&master)?;
        rustix::pty::unlockpt(&master)?;
        let slave_path = rustix::pty::ptsname(&master, Vec::<u8>::new())?;
        let slave = rustix::fs::open(
            &slave_path,
            rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NOCTTY,
            rustix::fs::Mode::empty(),
        )?;
        rustix::io::fcntl_setfd(&slave, rustix::io::FdFlags::CLOEXEC)?;
        rustix::termios::tcsetwinsize(
            &slave,
            rustix::termios::Winsize {
                ws_row: ROWS,
                ws_col: COLUMNS,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )?;
        // Parent-side handle on the same terminal, read only for the echo check.
        let echo_view = slave.as_fd().try_clone_to_owned()?;
        let stdin = Stdio::from(slave.as_fd().try_clone_to_owned()?);
        let stdout = Stdio::from(slave.as_fd().try_clone_to_owned()?);
        let stderr = Stdio::from(slave);
        let mut command = Command::new(binary);
        command
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .envs(env.iter().copied())
            .stdin(stdin)
            .stdout(stdout)
            .stderr(stderr)
            // The child leads its own session and process group, so the whole group
            // can be killed and the group id is known without waiting.
            .process_group(0)
            .kill_on_drop(true);
        let child = command.spawn().map_err(|error| {
            format!(
                "spawning {} in the pseudo-terminal failed: {error}",
                binary.display()
            )
        })?;
        let pid = child
            .id()
            .ok_or_else(|| format!("{} left no process id to own", binary.display()))?;
        let raw = i32::try_from(pid)
            .map_err(|_| format!("process id {pid} of {} exceeds pid_t", binary.display()))?;
        let group = rustix::process::Pid::from_raw(raw)
            .ok_or_else(|| format!("process id {pid} of {} is zero", binary.display()))?;
        let owner = GroupOwner {
            child,
            group: Some(group),
        };

        // `AsyncFd::new` is deprecated because tokio wants the `unsafe` `register`
        // instead; `unsafe_code = "forbid"` excludes that route, and this owned
        // nonblocking master upholds the documented descriptor-lifetime contract.
        #[expect(
            deprecated,
            reason = "the safe replacement `AsyncFd::register` is `unsafe`, which this crate forbids"
        )]
        let mut master = AsyncFd::new(master).map_err(|error| {
            format!("registering the pseudo-terminal master with the async runtime failed: {error}")
        })?;

        let mut window = Rolling::default();
        let tuning = Tuning {
            samples,
            settle,
            limit,
        };
        let outcome = run_samples(&mut master, &mut window, &echo_view, binary, tuning).await;
        owner.terminate(binary).await?;
        outcome
    }

    /// Waits for the child to start, checks kernel echo off, writes one glyph per
    /// sample, and measures write-to-paint latencies. Every wait is deadline-bounded.
    async fn run_samples(
        master: &mut AsyncFd<OwnedFd>,
        window: &mut Rolling,
        echo_view: &OwnedFd,
        binary: &Path,
        tuning: Tuning,
    ) -> TestResult<Vec<Duration>> {
        let startup_deadline = Instant::now() + tuning.limit;
        match read_master(master, tuning.limit).await? {
            Output::Chunk(chunk) => window.absorb(&chunk),
            Output::Quiet => {
                return Err(format!(
                    "{} produced no output within {:?}",
                    binary.display(),
                    tuning.limit
                )
                .into());
            }
            Output::Closed => {
                return Err(format!(
                    "{} produced no output: the child side of the terminal closed",
                    binary.display()
                )
                .into());
            }
        }
        drain_until_quiet(master, window, tuning.settle, startup_deadline).await?;
        let mut latencies = Vec::with_capacity(tuning.samples.min(GLYPH_COUNT));
        for index in 0..tuning.samples {
            let ended =
                drain_until_quiet(master, window, tuning.settle, Instant::now() + tuning.limit)
                    .await?;
            if ended {
                return Err(format!("the child closed its terminal before sample {index}").into());
            }
            if kernel_echo_enabled(echo_view)? {
                return Err(format!(
                    "kernel echo is still on before sample {index}: the child must disable it, \
                     or the probe would measure the line discipline instead of the child"
                )
                .into());
            }
            let glyph = glyph_for(index)?;
            let mut encoded = [0_u8; 4];
            let needle = glyph.encode_utf8(&mut encoded).as_bytes();
            let started = Instant::now();
            write_master(master, needle, tuning.limit).await?;
            let painted =
                await_glyph(master, window, needle, glyph, started + tuning.limit).await?;
            latencies.push(painted.saturating_duration_since(started));
        }
        Ok(latencies)
    }

    /// Appends output until `settle` passes with nothing new or until `deadline`;
    /// returns whether the terminal closed meanwhile.
    async fn drain_until_quiet(
        master: &mut AsyncFd<OwnedFd>,
        window: &mut Rolling,
        settle: Duration,
        deadline: Instant,
    ) -> TestResult<bool> {
        loop {
            let wait = settle.min(deadline.saturating_duration_since(Instant::now()));
            match read_master(master, wait).await? {
                Output::Chunk(chunk) => {
                    window.absorb(&chunk);
                    if Instant::now() >= deadline {
                        return Ok(false);
                    }
                }
                Output::Quiet => return Ok(false),
                Output::Closed => return Ok(true),
            }
        }
    }

    /// Reads until `needle` appears in the window, bounded by `deadline`; returns the
    /// arrival instant of the chunk that completed the glyph.
    async fn await_glyph(
        master: &mut AsyncFd<OwnedFd>,
        window: &mut Rolling,
        needle: &[u8],
        glyph: char,
        deadline: Instant,
    ) -> TestResult<Instant> {
        loop {
            if Instant::now() >= deadline {
                return Err(format!(
                    "glyph {glyph} (U+{:04X}) did not appear in the child output within the limit",
                    u32::from(glyph)
                )
                .into());
            }
            match read_master(master, deadline.saturating_duration_since(Instant::now())).await? {
                Output::Chunk(chunk) => {
                    let arrived = chunk.arrived;
                    window.absorb(&chunk);
                    if window.contains(needle) {
                        return Ok(arrived);
                    }
                }
                Output::Quiet => {}
                Output::Closed => {
                    return Err(format!(
                        "glyph {glyph} (U+{:04X}) did not appear: the child closed its terminal",
                        u32::from(glyph)
                    )
                    .into());
                }
            }
        }
    }

    /// Performs one bounded nonblocking read from the master.
    async fn read_master(master: &AsyncFd<OwnedFd>, wait: Duration) -> TestResult<Output> {
        let mut buffer = [0_u8; READ_CHUNK];
        let step = master.async_io(Interest::READABLE, |fd| {
            rustix::io::read(fd, &mut buffer).map_err(errno_to_io)
        });
        match timeout(wait, step).await {
            Ok(Ok(0)) => Ok(Output::Closed),
            Ok(Ok(read)) => Ok(Output::Chunk(Chunk {
                arrived: Instant::now(),
                bytes: buffer[..read].to_vec(),
            })),
            Ok(Err(error)) if is_child_eof(&error) => Ok(Output::Closed),
            Ok(Err(error)) => {
                Err(format!("reading the pseudo-terminal master failed: {error}").into())
            }
            Err(_elapsed) => Ok(Output::Quiet),
        }
    }

    /// Writes every byte of `bytes` to the master, bounded by `limit`.
    async fn write_master(
        master: &AsyncFd<OwnedFd>,
        mut bytes: &[u8],
        limit: Duration,
    ) -> TestResult<()> {
        let deadline = Instant::now() + limit;
        while !bytes.is_empty() {
            let wait = deadline.saturating_duration_since(Instant::now());
            let step = master.async_io(Interest::WRITABLE, |fd| {
                rustix::io::write(fd, bytes).map_err(errno_to_io)
            });
            match timeout(wait, step).await {
                Ok(Ok(0)) => return Err("the pseudo-terminal accepted no bytes".into()),
                Ok(Ok(written)) => bytes = &bytes[written..],
                Ok(Err(error)) => {
                    return Err(format!(
                        "writing the keypress to the pseudo-terminal failed: {error}"
                    )
                    .into());
                }
                Err(_elapsed) => {
                    return Err(format!(
                        "writing the keypress to the pseudo-terminal exceeded {limit:?}"
                    )
                    .into());
                }
            }
        }
        Ok(())
    }

    /// Whether the terminal line discipline still echoes input on its own.
    fn kernel_echo_enabled(slave: &OwnedFd) -> TestResult<bool> {
        let attributes = rustix::termios::tcgetattr(slave)?;
        Ok(attributes
            .local_modes
            .contains(rustix::termios::LocalModes::ECHO))
    }

    /// The `index`-th distinct sample glyph, drawn from the CJK Unified Ideographs block.
    fn glyph_for(index: usize) -> TestResult<char> {
        u32::try_from(index)
            .ok()
            .and_then(|offset| GLYPH_BASE.checked_add(offset))
            .filter(|code| *code <= GLYPH_LAST)
            .and_then(char::from_u32)
            .ok_or_else(|| "the keypress probe ran out of distinct sample glyphs".into())
    }

    /// Whether a master read error means the child side closed: `EIO`.
    fn is_child_eof(error: &std::io::Error) -> bool {
        error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error())
    }

    /// Converts a rustix errno into the matching `std::io::Error`.
    fn errno_to_io(error: rustix::io::Errno) -> std::io::Error {
        std::io::Error::from_raw_os_error(error.raw_os_error())
    }
}

/// Reports that this platform has no native PTY backend.
///
/// # Errors
/// Returns an error outside the supported Unix and Windows platforms.
#[cfg(not(any(unix, windows)))]
#[expect(
    clippy::unused_async,
    reason = "unsupported platforms retain the async probe signature"
)]
pub async fn keypress_echo_latencies(
    binary: &std::path::Path,
    args: &[&str],
    env: &[(&str, &str)],
    cwd: &std::path::Path,
    samples: usize,
    settle: std::time::Duration,
    limit: std::time::Duration,
) -> crate::support::TestResult<Vec<std::time::Duration>> {
    let _ = (binary, args, env, cwd, samples, settle, limit);
    Err("this platform has no native PTY backend".into())
}
