// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! `[limits.plugins]` cell budgets through the real `dalgona rpc` server.
//!
//! Each gate writes a `dal.toml` with a scripted provider into a temporary
//! home, spawns the real `dalgona rpc` stdio child under a deadline, drives
//! one prompt whose scripted model calls the `eval` tool once with
//! `"uses":[]`, and reads back the stored session journal. The oracle is the
//! journal's `tool_result` record for that call: a SMALL budget must record
//! a failed tool outcome whose envelope names the exact dotted
//! `limits.plugins.cell_*` key, while the roomier control records a
//! successful outcome carrying the workload's known value.
//!
//! Why not the `dalgona eval` report: `report.json` and `runs.jsonl` carry
//! one [`RunLine`] per run with status, reason, and token counts, and the
//! case check sees only `result["text"]`, `result["tool_calls"]` (each call's
//! name and args, never its outcome), `result["exit"]`, and usage. A model
//! that receives a failed tool result and keeps going settles the case on
//! the producer side, so case pass/fail counts cannot prove the cell budget
//! fired. The journal's `tool_result` record is the seam that does: the
//! wire `error` flag plus the eval envelope inside its parts.
//!
//! Seam grounding (names, not copies):
//! - `dalgona rpc` is dispatched by the dalgona binary into the shared
//!   dalgon entry (`dalgona/dalgona/src/lib.rs` `run`); everything else here
//!   speaks its JSONL stdio protocol: `initialize`, `session/open`,
//!   `session/subscribe`, `session/submit`, and `session/update`
//!   notifications, the same flow the rpc gates drive.
//! - The configured `[limits.plugins]` `cell_ticks`/`cell_heap_bytes`/
//!   `stack_depth` reach the `eval` tool cell through
//!   `cli/dalgon/src/product.rs` (`ticks`/`heap_bytes`/`stack_depth`); no
//!   test here builds limits by hand.
//! - The scripted model calls `eval` with `"uses":[]`, so the cell is pure
//!   and needs no approval; no approval question may appear during these
//!   turns, and one fails the gate fail-closed.
//! - Every registered battery is off through the documented
//!   `disabled_batteries` list: any battery-initiated model call (observed:
//!   a judge consuming the first scripted reply, leaving the main turn with
//!   no `eval` call and zero `tool_result` records) would steal fixture
//!   steps from the one real turn this gate drives. The product-provided
//!   `eval` tool is unaffected by this list.
//! - A budget failure classifies to envelope `status: failed` with
//!   `code: eval_error` (`extensions/dal-star/src/eval.rs` `classify` plus
//!   `reply`), delivered as `ToolOutcome::Err` and journaled with
//!   `error: true`; the envelope text must name the exact dotted key, not
//!   just the words `ticks`, `heap`, or `stack`.
//! - The stack workload is a finite chain of DISTINCT functions, never
//!   self-recursion: Starlark may reject a recursive call independent of
//!   depth, while a 25-deep chain overflows `stack_depth = 12` and fits
//!   `stack_depth = 64` with margin. The leaf returns the peak, so the
//!   value proves the whole walk.
//!
//! Environment hygiene: `env_clear` plus a fresh `HOME/XDG_CONFIG_HOME`/
//! `XDG_DATA_HOME` per rig (PATH preserved for the child). No provider, auth,
//! or proxy variable is inherited; the only provider is the scripted one
//! declared in the rig's own `dal.toml` fixture, so the binary never
//! performs live-model discovery.
#![expect(
    clippy::disallowed_methods,
    reason = "the gate spawns the real dalgona binary and pumps its rpc stdio"
)]
#![expect(
    clippy::expect_used,
    reason = "a gate aborts on a setup failure and names it"
)]

use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MODEL: &str = "openai-responses/gpt-6";
const USAGE: &str = r#"{"input_tokens":12,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}"#;
const PROMPT: &str = "Run the bounded eval cell, then reply done.";

/// Every wait below is owned by a deadline. A finite Starlark input never
/// bounds a hung child, so the child always is too.
const CALL_BUDGET: Duration = Duration::from_secs(30);
const TURN_BUDGET: Duration = Duration::from_secs(120);
const REAP_BUDGET: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(20);
const STDOUT_QUEUE: usize = 1024;
const STDERR_KEEP: usize = 128 << 10;
const STDERR_TAIL: usize = 2000;

/// A bounded tick workload: the exact sum of `range(2000)` is 1999000, far
/// above a 1000-tick cell and far below a five-million-tick one.
const TICKS_CODE: &str = "total = 0\nfor i in range(2000):\n    total = total + i\ntotal";

/// A bounded heap workload: a 400000-character string, far above a 65536-byte
/// cell and far below a 64MiB one. Only its length crosses back, so no
/// transport cap is near.
const HEAP_CODE: &str = "s = \"ab\" * 200000\nlen(s)";

/// How deep the distinct-function stack chain runs.
const STACK_DEPTH: u32 = 25;

/// Cell budgets that the tick workload exceeds; heap stays wide.
const SMALL_TICKS: &str = "[limits.plugins]\ncell_ticks = 1000\ncell_heap_bytes = 67108864\n";
/// Cell budgets that fit the tick workload with room to spare.
const LARGE_TICKS: &str = "[limits.plugins]\ncell_ticks = 5000000\ncell_heap_bytes = 67108864\n";
/// Cell budgets that the heap workload exceeds; ticks stay wide.
const SMALL_HEAP: &str = "[limits.plugins]\ncell_ticks = 5000000\ncell_heap_bytes = 65536\n";
/// Cell budgets that fit the heap workload with room to spare.
const LARGE_HEAP: &str = "[limits.plugins]\ncell_ticks = 5000000\ncell_heap_bytes = 67108864\n";
/// Cell budgets that the 25-deep chain overflows; ticks and heap stay wide.
const SMALL_STACK: &str =
    "[limits.plugins]\ncell_ticks = 5000000\ncell_heap_bytes = 67108864\nstack_depth = 12\n";
/// Cell budgets that fit the 25-deep chain with room to spare.
const LARGE_STACK: &str =
    "[limits.plugins]\ncell_ticks = 5000000\ncell_heap_bytes = 67108864\nstack_depth = 64\n";

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

fn text_step(text: &str) -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"text_delta","text":{text:?}}},{{"type":"tool_calls_done","calls":[]}},{{"type":"usage","usage":{USAGE}}},{{"type":"stop","reason":"end_turn"}}]}}"#
    )
}

fn tool_step(id: &str, name: &str, args_json: &str) -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"tool_call_started","id":{id:?},"name":{name:?}}},{{"type":"tool_calls_done","calls":[{{"id":{id:?},"name":{name:?},"args":{{"kind":"parsed","value":{args_json}}}}}]}},{{"type":"usage","usage":{USAGE}}},{{"type":"stop","reason":"tool_use"}}]}}"#
    )
}

/// The `eval` tool arguments for one pure cell: `"uses":[]` removes all
/// effects, so the call needs no approval and the budget is the only gate.
fn eval_args(code: &str) -> String {
    format!("{{\"code\":{code:?},\"uses\":[]}}")
}

/// A finite chain of DISTINCT functions: each calls only the next, so no
/// frame is ever re-entered and no recursion guard can fire independent of
/// depth. The leaf returns the peak, so the value proves the whole walk.
fn chain_code(depth: u32) -> String {
    let mut code = String::new();
    for index in 0..depth {
        let _ = write!(code, "def f{index}():\n    return f{}()\n", index + 1);
    }
    let _ = write!(code, "def f{depth}():\n    return {depth}\nf0()\n");
    code
}

/// JSON-quotes a string with the escaping the stdio frames need.
fn json_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            control if control.is_control() => {
                let _ = write!(out, "\\u{:04x}", u32::from(control));
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// The text after `"key":` in a machine frame, tolerating whitespace.
fn after_key<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let at = line.find(needle.as_str())?;
    let mut rest = &line[at + needle.len()..];
    rest = rest.trim_start().strip_prefix(':')?;
    Some(rest.trim_start())
}

/// The first `"key": "<string>"` of a machine frame, decoding escapes.
fn extract_string(line: &str, key: &str) -> Option<String> {
    let mut rest = after_key(line, key)?.strip_prefix('"')?;
    let mut out = String::new();
    loop {
        let end = rest.find(['"', '\\'])?;
        out.push_str(&rest[..end]);
        if rest[end..].starts_with('"') {
            return Some(out);
        }
        let escape = rest[end + 1..].chars().next()?;
        match escape {
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            '/' => out.push('/'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'u' => {
                let hex = rest.get(end + 2..end + 6)?;
                out.push(char::from_u32(u32::from_str_radix(hex, 16).ok()?)?);
                rest = &rest[end + 6..];
                continue;
            }
            _ => return None,
        }
        rest = &rest[end + 2..];
    }
}

/// The first `"key": <digits>` of a machine frame.
fn extract_u64(line: &str, key: &str) -> Option<u64> {
    let rest = after_key(line, key)?;
    let end = rest.find(|character: char| !character.is_ascii_digit())?;
    rest[..end].parse().ok()
}

/// Whether `line` carries `"turn":<turn>` outside any larger number.
fn has_turn(line: &str, turn: u64) -> bool {
    let needle = format!("\"turn\":{turn}");
    let mut rest = line;
    while let Some(at) = rest.find(needle.as_str()) {
        let after = &rest[at + needle.len()..];
        if after.chars().next().is_none_or(|c| !c.is_ascii_digit()) {
            return true;
        }
        rest = after;
    }
    false
}

/// Whether `line` is the JSON-RPC reply to `id`: it echoes the id and
/// carries no `method`.
fn is_reply(line: &str, id: u64) -> bool {
    line.contains("\"jsonrpc\"")
        && extract_u64(line, "id") == Some(id)
        && !line.contains("\"method\"")
}

/// The array after `"key":` in a machine frame, without its brackets.
fn bracketed<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = after_key(line, key)?.strip_prefix('[')?;
    let end = rest.find(']')?;
    Some(&rest[..end])
}

/// A temporary home with a `dal.toml`, a scripted provider, and a session
/// workspace. No battery is disabled: the scripted model calls only `eval`.
struct Rig {
    root: PathBuf,
}

impl Rig {
    fn new(steps: &[String], extra_toml: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "dalgona-budget-gate-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        let rig = Self { root };
        fs::create_dir_all(rig.data()).expect("data dir");
        fs::create_dir_all(rig.root.join("xdg-config/dalgona")).expect("config dir");
        fs::create_dir_all(rig.root.join("work")).expect("session workspace");
        let fixture = rig.root.join("scripted.jsonl");
        fs::write(&fixture, steps.join("\n") + "\n").expect("fixture");
        // Every battery stays off: any battery-initiated model call (the
        // judge was observed consuming the first scripted reply, which left
        // the main turn with no eval call and zero tool_result records)
        // would steal fixture steps from the one real turn this gate drives.
        // The product-provided `eval` tool is unaffected by this list.
        fs::write(
            rig.root.join("xdg-config/dalgona/dal.toml"),
            format!(
                "model = \"{MODEL}\"\ndisabled_batteries = [\"ask\", \"history\", \"judged\", \"mcp\", \"orchestration\", \"procs\", \"quality\", \"review\", \"skills\", \"ttsr-rules\", \"web\", \"work\"]\n{extra_toml}\n[providers.scripted]\nfixture = {:?}\n",
                fixture.to_string_lossy()
            ),
        )
        .expect("dal.toml");
        rig
    }

    fn data(&self) -> PathBuf {
        self.root.join("xdg-data")
    }

    /// Drives one prompt through the owned `dalgona rpc` child and returns
    /// the session journal.
    fn run_prompt(&self, prompt: &str) -> String {
        let mut rpc = Rpc::spawn(self);
        rpc.hello();
        let session = rpc.open_session(&self.root.join("work").to_string_lossy());
        rpc.subscribe(&session);
        let turn = rpc.submit(&session, prompt);
        rpc.pump_turn(&session, turn);
        rpc.shutdown();
        self.journal()
    }

    /// The one `journal.jsonl` under this rig's data root.
    fn journal(&self) -> String {
        let mut found = Vec::new();
        collect_journals(&self.data().join("dalgona/sessions"), &mut found);
        assert_eq!(
            found.len(),
            1,
            "expected one session journal under {}, found {}",
            self.data().join("dalgona/sessions").display(),
            found.len()
        );
        fs::read_to_string(found.remove(0)).expect("journal.jsonl")
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn collect_journals(dir: &Path, found: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).expect("sessions dir");
    for entry in entries {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            collect_journals(&path, found);
        } else if path.file_name().is_some_and(|name| name == "journal.jsonl") {
            found.push(path);
        }
    }
}

/// One owned `dalgona rpc` child: `Drop` closes stdin, kills, waits, and
/// joins the reader threads, so every launch path cleans up even on an
/// early panic. The normal path uses [`Rpc::shutdown`] instead, whose
/// kill/wait failures are explicit instead of ignored.
struct Rpc {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    frames: mpsc::Receiver<String>,
    dropped_lines: Arc<AtomicU64>,
    stdout: Option<JoinHandle<()>>,
    stderr: Option<JoinHandle<()>>,
    err_log: Arc<Mutex<Vec<u8>>>,
    stashed: Vec<String>,
    next_id: u64,
}

impl Rpc {
    /// Spawns the current-tree binary with piped stdio under a cleared
    /// environment: HOME plus absolute XDG roots inside the rig, PATH
    /// preserved, nothing else. In particular no `*_PROXY`, provider, or
    /// auth variable reaches the child; the scripted provider is the only
    /// route, so startup performs no live-model discovery.
    fn spawn(rig: &Rig) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_dalgona"))
            .arg("rpc")
            .current_dir(&rig.root)
            .env_clear()
            .env("HOME", &rig.root)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("XDG_CONFIG_HOME", rig.root.join("xdg-config"))
            .env("XDG_DATA_HOME", rig.root.join("xdg-data"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn dalgona rpc");
        let stdin = child.stdin.take().expect("the RPC child has piped stdin");
        let stdout = child.stdout.take().expect("the RPC child has piped stdout");
        let stderr = child.stderr.take().expect("the RPC child has piped stderr");
        // The stdout reader never blocks: once the queue fills it counts and
        // drops, so a spewing child turns into an explicit failure instead
        // of a wedged join. Both readers end on EOF after the child is gone.
        let (sender, frames) = mpsc::sync_channel(STDOUT_QUEUE);
        let dropped_lines = Arc::new(AtomicU64::new(0));
        let dropped = Arc::clone(&dropped_lines);
        let stdout = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let line = line.expect("read RPC stdout");
                if sender.try_send(line).is_err() {
                    dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
        let err_log = Arc::new(Mutex::new(Vec::new()));
        let logged = Arc::clone(&err_log);
        let stderr = thread::spawn(move || {
            let mut stderr = stderr;
            let mut chunk = [0_u8; 8192];
            loop {
                match stderr.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => {
                        let mut log = logged.lock().unwrap_or_else(PoisonError::into_inner);
                        log.extend_from_slice(&chunk[..read]);
                        if log.len() > STDERR_KEEP {
                            let excess = log.len() - STDERR_KEEP;
                            log.drain(..excess);
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        });
        Self {
            child: Some(child),
            stdin: Some(stdin),
            frames,
            dropped_lines,
            stdout: Some(stdout),
            stderr: Some(stderr),
            err_log,
            stashed: Vec::new(),
            next_id: 1,
        }
    }

    #[expect(
        clippy::panic,
        reason = "the gate aborts with the rpc stderr tail on failure"
    )]
    fn fail(&self, message: &str) -> ! {
        let log = self.err_log.lock().unwrap_or_else(PoisonError::into_inner);
        let text = String::from_utf8_lossy(&log);
        let mut tail = text.as_ref();
        if tail.len() > STDERR_TAIL {
            let mut at = tail.len() - STDERR_TAIL;
            while !tail.is_char_boundary(at) {
                at += 1;
            }
            tail = &tail[at..];
        }
        panic!("{message}\n--- rpc stderr tail ---\n{tail}");
    }

    fn send_frame(&mut self, frame: &str, method: &str) {
        // The stdin borrow ends before any failure, so `fail` never shares
        // a borrow with the writer.
        let sent = match self.stdin.as_mut() {
            Some(stdin) => stdin
                .write_all(frame.as_bytes())
                .and_then(|()| stdin.flush())
                .is_ok(),
            None => false,
        };
        if !sent {
            self.fail(&format!("sending the {method} frame failed"));
        }
    }

    /// The next frame, `None` on a quiet poll tick.
    fn recv(&self, deadline: Instant) -> Option<String> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match self.frames.recv_timeout(remaining) {
            Ok(line) => Some(line),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.fail("the RPC stdout reader ended");
            }
        }
    }

    /// Sends a call and returns its reply frame, stashing notifications.
    /// Replies echo the request id and carry no `method`; anything else is
    /// a notification for the turn pump, never a reply.
    fn call(&mut self, method: &str, params: &str) -> String {
        let id = self.next_id;
        self.next_id += 1;
        let frame = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":{method:?},\"params\":{params}}}\n"
        );
        self.send_frame(&frame, method);
        let deadline = Instant::now() + CALL_BUDGET;
        loop {
            if Instant::now() >= deadline {
                self.fail(&format!("{method} never answered within its budget"));
            }
            let Some(line) = self.recv(deadline) else {
                continue;
            };
            if is_reply(&line, id) {
                if line.contains("\"error\"") {
                    self.fail(&format!("{method} failed: {line}"));
                }
                return line;
            }
            if self.stashed.len() >= STDOUT_QUEUE {
                self.fail(&format!("{method} drowned in {STDOUT_QUEUE} notifications"));
            }
            self.stashed.push(line);
        }
    }

    fn hello(&mut self) {
        let params = "{\"protocolVersion\":1,\"clientInfo\":{\"name\":\"dalgona-budget-rpc\",\"version\":\"1\"},\"capabilities\":[\"sessions\",\"approval\",\"ask\"]}";
        let line = self.call("initialize", params);
        if !line.contains("\"protocolVersion\":1") {
            self.fail(&format!("unexpected RPC protocol version: {line}"));
        }
        match bracketed(&line, "capabilities") {
            Some(caps) if caps.contains("\"sessions\"") => {}
            _ => self.fail(&format!("unexpected RPC capabilities: {line}")),
        }
    }

    fn open_session(&mut self, workspace: &str) -> String {
        let params = format!(
            "{{\"ref\":{{\"type\":\"new\",\"workspace\":{}}}}}",
            json_escape(workspace)
        );
        let line = self.call("session/open", &params);
        extract_string(&line, "sessionId")
            .unwrap_or_else(|| self.fail(&format!("session/open returned no session id: {line}")))
    }

    fn subscribe(&mut self, session: &str) {
        let params = format!("{{\"sessionId\":{}}}", json_escape(session));
        let line = self.call("session/subscribe", &params);
        if !(line.contains("\"gen\"") && line.contains("\"seq\"")) {
            self.fail(&format!("session/subscribe returned no cursor: {line}"));
        }
    }

    fn submit(&mut self, session: &str, prompt: &str) -> u64 {
        let params = format!(
            "{{\"sessionId\":{},\"command\":{{\"type\":\"prompt\",\"expect\":\"idle\",\"content\":[{{\"type\":\"text\",\"text\":{}}}]}}}}",
            json_escape(session),
            json_escape(prompt)
        );
        let line = self.call("session/submit", &params);
        if !line.contains("\"type\":\"accepted\"") {
            self.fail(&format!("the prompt was not accepted: {line}"));
        }
        extract_u64(&line, "turn")
            .unwrap_or_else(|| self.fail(&format!("the prompt returned no turn: {line}")))
    }

    /// Drains updates until this session's turn ends. A pure `eval` cell
    /// needs no approval, so any approval question fails fail-closed: the
    /// fixture drifted or the product started asking, and silence would
    /// prove nothing.
    fn pump_turn(&mut self, session: &str, turn: u64) {
        let deadline = Instant::now() + TURN_BUDGET;
        loop {
            if Instant::now() >= deadline {
                self.fail(&format!("turn {turn} never ended within its budget"));
            }
            while let Some(line) = self.stashed.pop() {
                if self.observe(&line, session, turn) {
                    return;
                }
            }
            let Some(line) = self.recv(deadline) else {
                continue;
            };
            if self.observe(&line, session, turn) {
                return;
            }
            if self.stashed.len() >= STDOUT_QUEUE {
                self.fail(&format!(
                    "turn {turn} drowned in {STDOUT_QUEUE} notifications"
                ));
            }
            self.stashed.push(line);
        }
    }

    /// Whether `line` ends our turn. Returns `false` for unrelated chatter,
    /// which the pump re-queues.
    fn observe(&self, line: &str, session: &str, turn: u64) -> bool {
        if !line.contains("session/update") || !line.contains(session) {
            return false;
        }
        if line.contains("request_opened") {
            self.fail(&format!(
                "unexpected approval question during a pure eval turn: {line}"
            ));
        }
        line.contains("turn_ended") && has_turn(line, turn)
    }

    /// Closes stdin, reaps the child on every path (kill plus wait when it
    /// outlives its budget, with explicit failures instead of ignored
    /// results), drains both pipes, and joins the readers.
    fn shutdown(mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + REAP_BUDGET;
        loop {
            let status = self
                .child
                .as_mut()
                .expect("the RPC child outlives shutdown")
                .try_wait()
                .expect("poll the RPC child");
            if status.is_some() {
                break;
            }
            if Instant::now() >= deadline {
                let child = self
                    .child
                    .as_mut()
                    .expect("the RPC child outlives its kill");
                child.kill().expect("kill the hung RPC child");
                child.wait().expect("reap the killed RPC child");
                break;
            }
            thread::sleep(POLL);
        }
        if let Some(stdout) = self.stdout.take() {
            stdout.join().expect("join the RPC stdout reader");
        }
        if let Some(stderr) = self.stderr.take() {
            stderr.join().expect("join the RPC stderr reader");
        }
        let dropped = self.dropped_lines.load(Ordering::Relaxed);
        assert_eq!(dropped, 0, "the RPC child spewed past the stdout queue");
    }
}

impl Drop for Rpc {
    fn drop(&mut self) {
        // Best-effort by necessity: this runs on panic paths (the normal
        // path goes through `shutdown`, whose kill/wait/join failures are
        // explicit `expect`s), and panicking here during an active unwind
        // would abort the test binary. The child is still killed, waited,
        // and joined on every path, never leaked.
        drop(self.stdin.take());
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(stdout) = self.stdout.take() {
            let _ = stdout.join();
        }
        if let Some(stderr) = self.stderr.take() {
            let _ = stderr.join();
        }
    }
}

/// The `tool_result` lines for one tool, in journal order.
fn tool_results<'a>(journal: &'a str, tool: &str) -> Vec<&'a str> {
    let name = format!("\"name\":\"{tool}\"");
    journal
        .lines()
        .filter(|line| line.contains("\"type\":\"tool_result\"") && line.contains(name.as_str()))
        .collect()
}

fn contains(haystack: &str, needle: &str) {
    assert!(
        haystack.contains(needle),
        "missing {needle:?} in:\n{haystack}"
    );
}

/// Whether `haystack` carries the escaped `"key":<number>` outside any
/// larger number. The envelope rides inside a journal text part, so its
/// quotes arrive backslash-escaped; the key order is not assumed, so no
/// trailing comma is required.
fn contains_escaped_number(haystack: &str, key: &str, number: u64) -> bool {
    let needle = format!("\\\"{key}\\\":{number}");
    let mut rest = haystack;
    while let Some(at) = rest.find(needle.as_str()) {
        let after = &rest[at + needle.len()..];
        if after.chars().next().is_none_or(|c| !c.is_ascii_digit()) {
            return true;
        }
        rest = after;
    }
    false
}

fn contains_number(haystack: &str, key: &str, number: u64) {
    assert!(
        contains_escaped_number(haystack, key, number),
        "missing {key} {number} in:\n{haystack}"
    );
}

/// Whether the model kept going after the tool call: some later journal
/// line carries the reply text. The prompt record sits before the call, so
/// only the second scripted reply can put `done` after it.
fn assert_continued(journal: &str, tool_line: &str) {
    let at = journal
        .find(tool_line)
        .expect("the tool line comes from the journal");
    let after = &journal[at + tool_line.len()..];
    assert!(
        after.lines().any(|line| line.contains("done")),
        "the model never replied after the tool call:\n{journal}"
    );
}

#[test]
fn cell_tick_budget_names_its_key_in_the_journal() {
    let rig = Rig::new(
        &[
            tool_step("cell-1", "eval", &eval_args(TICKS_CODE)),
            text_step("done"),
        ],
        SMALL_TICKS,
    );
    let journal = rig.run_prompt(PROMPT);
    let results = tool_results(&journal, "eval");
    assert_eq!(results.len(), 1, "one eval tool_result:\n{journal}");
    let line = results[0];
    contains(line, "\"error\":true");
    contains(line, "\\\"status\\\":\\\"failed\\\"");
    contains(line, "\\\"code\\\":\\\"eval_error\\\"");
    contains(line, "Execution duration limit");
    contains(line, "1000");
    contains(line, "raise limits.plugins.cell_ticks to allow more");
    assert_continued(&journal, line);
}

#[test]
fn cell_tick_allowance_returns_its_value() {
    let rig = Rig::new(
        &[
            tool_step("cell-1", "eval", &eval_args(TICKS_CODE)),
            text_step("done"),
        ],
        LARGE_TICKS,
    );
    let journal = rig.run_prompt(PROMPT);
    let results = tool_results(&journal, "eval");
    assert_eq!(results.len(), 1, "one eval tool_result:\n{journal}");
    let line = results[0];
    contains(line, "\"error\":false");
    contains_number(line, "value", 1_999_000);
    assert_continued(&journal, line);
}

#[test]
fn cell_heap_budget_names_its_key_in_the_journal() {
    let rig = Rig::new(
        &[
            tool_step("cell-1", "eval", &eval_args(HEAP_CODE)),
            text_step("done"),
        ],
        SMALL_HEAP,
    );
    let journal = rig.run_prompt(PROMPT);
    let results = tool_results(&journal, "eval");
    assert_eq!(results.len(), 1, "one eval tool_result:\n{journal}");
    let line = results[0];
    contains(line, "\"error\":true");
    contains(line, "\\\"status\\\":\\\"failed\\\"");
    contains(line, "\\\"code\\\":\\\"eval_error\\\"");
    contains(line, "Heap memory limit");
    contains(line, "65536");
    contains(line, "raise limits.plugins.cell_heap_bytes to allow more");
    assert_continued(&journal, line);
}

#[test]
fn cell_heap_allowance_returns_its_value() {
    let rig = Rig::new(
        &[
            tool_step("cell-1", "eval", &eval_args(HEAP_CODE)),
            text_step("done"),
        ],
        LARGE_HEAP,
    );
    let journal = rig.run_prompt(PROMPT);
    let results = tool_results(&journal, "eval");
    assert_eq!(results.len(), 1, "one eval tool_result:\n{journal}");
    let line = results[0];
    contains(line, "\"error\":false");
    contains_number(line, "value", 400_000);
    assert_continued(&journal, line);
}

#[test]
fn cell_stack_budget_names_its_key_in_the_journal() {
    let rig = Rig::new(
        &[
            tool_step("cell-1", "eval", &eval_args(&chain_code(STACK_DEPTH))),
            text_step("done"),
        ],
        SMALL_STACK,
    );
    let journal = rig.run_prompt(PROMPT);
    let results = tool_results(&journal, "eval");
    assert_eq!(results.len(), 1, "one eval tool_result:\n{journal}");
    let line = results[0];
    contains(line, "\"error\":true");
    contains(line, "\\\"status\\\":\\\"failed\\\"");
    contains(line, "\\\"code\\\":\\\"eval_error\\\"");
    contains(line, "Starlark call stack overflow");
    contains(line, "raise limits.plugins.stack_depth to allow more");
    assert_continued(&journal, line);
}

#[test]
fn cell_stack_allowance_returns_its_peak() {
    let rig = Rig::new(
        &[
            tool_step("cell-1", "eval", &eval_args(&chain_code(STACK_DEPTH))),
            text_step("done"),
        ],
        LARGE_STACK,
    );
    let journal = rig.run_prompt(PROMPT);
    let results = tool_results(&journal, "eval");
    assert_eq!(results.len(), 1, "one eval tool_result:\n{journal}");
    let line = results[0];
    contains(line, "\"error\":false");
    contains_number(line, "value", 25);
    assert_continued(&journal, line);
}
