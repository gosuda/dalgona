// Copyright (c) Cognition Inc. and other dal contributors.
// SPDX-License-Identifier: MIT
#![cfg(unix)]
#![expect(
    unreachable_pub,
    reason = "transcript helpers are public only within private test modules"
)]

//! Committable PTY transcripts and terminal snapshot diffs.
//!
//! A `Transcript` records framed terminal traffic (child output, keyboard
//! input, resizes) beside the PTY driver; `replay` feeds the output half
//! through the same `vt.rs` parser the live gates use, so a committed
//! transcript + a committed screen snapshot becomes a diffable terminal
//! artifact instead of manual xterm verification.
//!
//! Snapshot artifacts live under `tests/snapshots/<gate>/<name>.snap`.
//! Regenerate or bless a changed screen with `DAL_SNAPSHOT_UPDATE=1`; a
//! mismatch writes `<name>.actual` next to it and fails naming both paths.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use crate::vt::VtRecorder;

/// One framed entry in a transcript.
#[derive(Debug)]
enum Entry {
    /// Bytes the child wrote to the terminal.
    Out(Vec<u8>),
    /// Bytes the driver typed at the child.
    In(Vec<u8>),
    /// A resize observed by the driver.
    Resize { columns: u16, rows: u16 },
}

/// A recorded PTY conversation in wall-clock order.
pub struct Transcript {
    entries: Vec<(u64, Entry)>,
    started: Instant,
    /// How much of the bound `PtyProcess::output()` has been absorbed.
    absorbed: usize,
}

impl Transcript {
    /// Starts an empty transcript clock.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            started: Instant::now(),
            absorbed: 0,
        }
    }

    fn stamp(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Appends child output observed since the last absorb. Call after each
    /// `collect_for`/`wait_for` on the bound PTY process.
    pub fn absorb(&mut self, output: &[u8]) {
        if output.len() > self.absorbed {
            let fresh = output[self.absorbed..].to_vec();
            self.absorbed = output.len();
            let stamp = self.stamp();
            self.entries.push((stamp, Entry::Out(fresh)));
        }
    }

    /// Records keyboard input sent to the child.
    pub fn input(&mut self, bytes: &[u8]) {
        let stamp = self.stamp();
        self.entries.push((stamp, Entry::In(bytes.to_vec())));
    }

    /// Records a resize the driver issued.
    #[expect(
        dead_code,
        reason = "resize frames round-trip for gates that drive resizes"
    )]
    pub fn resize(&mut self, columns: u16, rows: u16) {
        let stamp = self.stamp();
        self.entries.push((stamp, Entry::Resize { columns, rows }));
    }

    /// Serializes the transcript: one `{"ms","dir","data"}` line per entry with
    /// hex-encoded payloads so the file stays greppable and lossless.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = io::BufWriter::new(fs::File::create(path)?);
        for (ms, entry) in &self.entries {
            match entry {
                Entry::Out(data) => {
                    writeln!(
                        file,
                        "{{\"ms\":{ms},\"dir\":\"out\",\"data\":\"{}\"}}",
                        hex(data)
                    )?;
                }
                Entry::In(data) => {
                    writeln!(
                        file,
                        "{{\"ms\":{ms},\"dir\":\"in\",\"data\":\"{}\"}}",
                        hex(data)
                    )?;
                }
                Entry::Resize { columns, rows } => {
                    writeln!(
                        file,
                        "{{\"ms\":{ms},\"dir\":\"resize\",\"cols\":{columns},\"rows\":{rows}}}"
                    )?;
                }
            }
        }
        file.flush()
    }

    /// Parses a transcript file written by [`Transcript::save`].
    pub fn load(path: &Path) -> io::Result<Self> {
        let text = fs::read_to_string(path)?;
        let mut entries = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let bad = || {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("transcript line {} is malformed", index + 1),
                )
            };
            let ms = field_u64(line, "\"ms\":").ok_or_else(bad)?;
            let entry = if line.contains("\"dir\":\"out\"") {
                Entry::Out(unhex(&field_str(line, "\"data\":\"").ok_or_else(bad)?)?)
            } else if line.contains("\"dir\":\"in\"") {
                Entry::In(unhex(&field_str(line, "\"data\":\"").ok_or_else(bad)?)?)
            } else if line.contains("\"dir\":\"resize\"") {
                Entry::Resize {
                    columns: u16::try_from(field_u64(line, "\"cols\":").ok_or_else(bad)?)
                        .map_err(|_| bad())?,
                    rows: u16::try_from(field_u64(line, "\"rows\":").ok_or_else(bad)?)
                        .map_err(|_| bad())?,
                }
            } else {
                return Err(bad());
            };
            entries.push((ms, entry));
        }
        Ok(Self {
            entries,
            started: Instant::now(),
            absorbed: 0,
        })
    }

    /// Feeds the transcript's `out` frames through `vt.rs` in order, honoring
    /// recorded resizes, and returns the settled recorder.
    #[must_use]
    pub fn replay(&self, columns: u16, rows: u16) -> VtRecorder {
        let mut recorder = VtRecorder::new(columns, rows);
        for (_, entry) in &self.entries {
            match entry {
                Entry::Out(bytes) => recorder.feed(bytes),
                Entry::Resize { columns, rows } => recorder.resize(*columns, *rows),
                Entry::In(_) => {}
            }
        }
        recorder
    }
}

/// Snapshots visible rows of a replayed transcript against
/// `tests/snapshots/<gate>/<name>.snap`. With `DAL_SNAPSHOT_UPDATE=1` the file
/// is (re)written; otherwise a drift writes `<name>.actual` and errors naming
/// the first differing row.
pub fn assert_snapshot(gate: &str, name: &str, rows: &[String]) -> io::Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
        .join(gate);
    let expected = root.join(format!("{name}.snap"));
    let normalized: Vec<String> = rows.iter().map(|row| row.trim_end().to_owned()).collect();
    let rendered = normalized.join("\n") + "\n";

    if std::env::var_os("DAL_SNAPSHOT_UPDATE").is_some() {
        fs::create_dir_all(&root)?;
        fs::write(&expected, &rendered)?;
        return Ok(());
    }

    let blessed = fs::read_to_string(&expected).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "missing snapshot {} (run with DAL_SNAPSHOT_UPDATE=1 to bless): {error}",
                expected.display()
            ),
        )
    })?;
    if blessed == rendered {
        return Ok(());
    }

    let actual = root.join(format!("{name}.actual"));
    fs::create_dir_all(&root)?;
    fs::write(&actual, &rendered)?;

    let blessed_rows: Vec<&str> = blessed.lines().collect();
    let first_diff = blessed_rows
        .iter()
        .zip(normalized.iter())
        .position(|(want, got)| want != got)
        .map(|index| index + 1)
        .map_or_else(
            || {
                let shared = blessed_rows.len().min(normalized.len());
                if normalized.len() > shared {
                    format!(
                        "length: {} extra actual row(s) starting at row {}: {:?}",
                        normalized.len() - shared,
                        shared + 1,
                        &normalized[shared..]
                    )
                } else {
                    format!(
                        "length: {} missing row(s) starting at row {}: expected {:?}",
                        shared - normalized.len(),
                        normalized.len() + 1,
                        &blessed_rows[normalized.len()..]
                    )
                }
            },
            |line| format!("row {line}"),
        );
    Err(io::Error::other(format!(
        "snapshot drift at {first_diff}: {} vs {} — bless with DAL_SNAPSHOT_UPDATE=1 if intended",
        expected.display(),
        actual.display(),
    )))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

fn unhex(text: &str) -> io::Result<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "hex payload has odd length",
        ));
    }
    let value = |nibble: u8| -> io::Result<u8> {
        match nibble {
            b'0'..=b'9' => Ok(nibble - b'0'),
            b'a'..=b'f' => Ok(nibble - b'a' + 10),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "hex payload has a non-hex digit",
            )),
        }
    };
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| Ok((value(pair[0])? << 4) | value(pair[1])?))
        .collect()
}

/// Extracts `"key":` followed by a decimal integer.
fn field_u64(line: &str, key: &str) -> Option<u64> {
    let start = line.find(key)? + key.len();
    let digits: String = line[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// Extracts `"key":"` followed by a value terminated by the closing quote.
fn field_str(line: &str, key: &str) -> Option<String> {
    let start = line.find(key)? + key.len();
    let end = line[start..].find('"')? + start;
    Some(line[start..end].to_owned())
}
