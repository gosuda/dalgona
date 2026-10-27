//! Assistant prose rendering: headings and `**strong**` spans bold, inline code in the
//! accent role, dashed lists with hanging indents, fenced code at full width behind a
//! faint ASCII rule, and tables as measured ASCII columns with a bold header row,
//! a `|`/`-` rule, delimiter-driven alignment, and `...` cell truncation. Nothing
//! renders in italics.

use std::ops::Range;

use crate::diagram::{DiagramKind, DiagramSettings, RenderCache, WiredBlock, wire_source};
use crate::render::{
    RenderLink, RenderRow, RenderSpan, RichLine, indent_row, linked_text, wired_rows,
    wrap_rich_line,
};
use crate::theme::Role;
use crate::width::{WidthMode, escape, take_cells, wrap};

/// Renders markdown `text`: prose wraps at `cap` cells, code and tables at
/// `full`, with routable completed fences offered to the diagram
/// engine. `wire = Some((settings, cache))` enables cards; `None` (or
/// `settings.enabled == false`, a non-routable fence, or an unparseable info
/// string) materializes each kept fence as today's info-row + `code_rows`
/// fallback, byte-identical to the unwired render.
pub(crate) fn markdown_wired_rows(
    text: &str,
    cap: usize,
    full: usize,
    mode: WidthMode,
    wire: Option<(&DiagramSettings, &RenderCache)>,
) -> Vec<RenderRow> {
    let defer = |info: &str| {
        wire.is_some_and(|(settings, _)| settings.enabled) && DiagramKind::parse(info).is_some()
    };
    let mut pieces = Vec::new();
    let mut state = LineState::default();
    let mut offset = 0;
    for logical in text.split('\n') {
        let next = offset + logical.len() + 1;
        let rest = text.get(next..).unwrap_or("");
        state.push(logical, rest, cap, full, mode, &mut pieces, &defer);
        offset = next;
    }
    state.finish(cap, full, mode, &mut pieces);
    state.flush_open(cap, full, mode, wire, &mut pieces);
    let mut rows = materialize(&pieces, cap, full, mode, wire);
    if rows.is_empty() {
        rows.push(RenderRow::plain(String::new(), Role::Text));
    }
    rows
}

/// Turns kept pieces back into rows: committed rows clone through, and each
/// kept fence either schedules its card through `wire_source` or falls back
/// to the same code rows an unwired render emits.
fn materialize(
    pieces: &[Piece],
    cap: usize,
    full: usize,
    mode: WidthMode,
    wire: Option<(&DiagramSettings, &RenderCache)>,
) -> Vec<RenderRow> {
    let mut rows = Vec::new();
    for piece in pieces {
        match piece {
            Piece::Row(row) => rows.push(row.clone()),
            Piece::Fence { info, body } => {
                rows.extend(fence_rows(info, body, cap, full, mode, wire));
            }
        }
    }
    rows
}

/// The card of one kept fence, or the info-row + code fallback the unwired
/// pipeline renders. `routable` and a parseable `info` were proven before the
/// piece existed; `enabled` is re-checked here so a mid-stream settings toggle
/// degrades a stored piece to code rows.
///
/// The card body drops one trailing `'\r'` — the byte [`fence_body`] strips
/// for expansion — so render and expand share one cache key; the stored
/// body stays raw, so the disabled fallback stays byte-identical to the
/// never-deferred code rows.
fn fence_rows(
    info: &str,
    body: &str,
    cap: usize,
    full: usize,
    mode: WidthMode,
    wire: Option<(&DiagramSettings, &RenderCache)>,
) -> Vec<RenderRow> {
    let card = wire.and_then(|(settings, cache)| {
        let source = body.strip_suffix('\r').unwrap_or(body);
        wire_source(settings, cache, DiagramKind::parse(info)?, source, cap)
    });
    if let Some(wired) = card {
        wired_rows(wired, cap, mode)
    } else {
        let mut rows = Vec::new();
        if !info.is_empty() {
            rows.push(RenderRow::plain(escape(info), Role::Dim));
        }
        for line in body.split('\n') {
            rows.extend(code_rows(line, full, mode));
        }
        rows
    }
}

/// The pending placeholder for an unclosed CLI-backed fence, or `None` when
/// the fence keeps code rows: unwired renders, unparseable info, and sync
/// kinds (mermaid/dot render inline, never pending). Only D2/nomnoml
/// schedule async workers, so only they can hold a card pending. Never
/// touches the cache — the source is partial, so no worker runs and the
/// render budget is untouched.
fn pending_placeholder(
    info: &str,
    cap: usize,
    mode: WidthMode,
    wire: Option<(&DiagramSettings, &RenderCache)>,
) -> Option<Vec<RenderRow>> {
    if !wire.is_some_and(|(settings, _)| settings.enabled) {
        return None;
    }
    let kind = DiagramKind::parse(info)?;
    if !matches!(kind, DiagramKind::D2 | DiagramKind::Nomnoml) {
        return None;
    }
    Some(wired_rows(WiredBlock::Pending { kind }, cap, mode))
}

/// One completed fenced block inside a text: byte ranges into the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FenceSpan {
    /// Open line start .. close line end (inclusive of both fence lines).
    pub span: Range<usize>,
    /// Byte range of the trimmed info string on the open line.
    pub info: Range<usize>,
    /// Byte range of the fence body (between open and close lines,
    /// exclusive of both). A single trailing '\r' of the final body line is
    /// inside the range; callers strip it via [`fence_body`].
    pub body: Range<usize>,
    /// True when the fence satisfies the frozen routing grammar — the
    /// `fenced_source` line rules applied to one fence: the open line starts
    /// at column 0 with exactly a 3-backtick run (no indent, backtick
    /// marker, run length 3) and the close line is byte-exactly three backticks.
    /// `DiagramKind::parse(info)` is applied by consumers, keeping the
    /// grammar check free of diagram kinds.
    pub routable: bool,
}

/// Whether a raw open line satisfies the frozen routing grammar: a column-0
/// exactly-3 backtick run (a longer run leaves a backtick in the info, which
/// `DiagramKind::parse` never accepts — the rule is spelled out so the gate
/// stays explicit, not accidental).
fn routable_open(line: &str) -> bool {
    line.starts_with("```") && line.as_bytes().get(3) != Some(&b'`')
}

/// Whether the close candidate `line` reads as the byte-exact ```` ``` ````
/// close once the line ends the text: a literal `"```"`, or `"```\r"` where
/// the whole-text trim removes the byte. The tolerant arm accepts exactly
/// the shapes `fenced_source` accepted on the whole text — every byte after
/// the close is a `\r`/`\n` terminator run, as in `"```\r\n"`. An interior
/// `"```\r"` followed by more prose stays a non-exact close, so the frozen
/// grammar never widens beyond the text-end exception.
fn routable_close(line: &str, rest: &str) -> bool {
    if line == "```" {
        return true;
    }
    let Some(stripped) = line.strip_suffix('\r') else {
        return false;
    };
    stripped == "```" && rest.bytes().all(|byte| byte == b'\r' || byte == b'\n')
}

/// Indexes every completed fenced block in `text` using the same
/// `Fence::opens`/`closes`/`indent_trimmed` rules the row pipeline applies.
/// Unclosed trailing fences produce no span. Cheap, zero-alloc, deterministic.
///
/// The one tolerant close is the text end: a final `"```\r"` reads as
/// `"```"` once the whole-text trim removes the byte — the same place
/// `fenced_source` tolerated it. An interior `"```\r"` stays a non-exact
/// close.
pub(crate) fn fence_spans(text: &str) -> Vec<FenceSpan> {
    struct Pending {
        fence: Fence,
        open_start: usize,
        info: Range<usize>,
        body_start: usize,
        routable: bool,
    }
    let mut spans = Vec::new();
    let mut open = None::<Pending>;
    let mut offset = 0;
    for line in text.split('\n') {
        let next = offset + line.len() + 1;
        let rest = text.get(next..).unwrap_or("");
        if open
            .as_ref()
            .is_some_and(|pending| pending.fence.closes(line))
        {
            if let Some(pending) = open.take() {
                spans.push(FenceSpan {
                    span: pending.open_start..offset + line.len(),
                    info: pending.info,
                    body: pending.body_start..offset.saturating_sub(1).max(pending.body_start),
                    routable: pending.routable && routable_close(line, rest),
                });
            }
        } else if open.is_none()
            && let Some((fence, info)) = Fence::opens(line)
        {
            // `info` is a suffix of `line.trim_end()`, so the range is plain
            // subtraction — no searching and no pointer arithmetic.
            let info_end = offset + line.trim_end().len();
            open = Some(Pending {
                fence,
                open_start: offset,
                info: info_end - info.len()..info_end,
                body_start: next,
                routable: routable_open(line),
            });
        }
        offset = next;
    }
    spans
}

/// The fenced body bytes both renderer and expansion agree on: the body
/// range with one trailing '\r' removed (byte-parity with `fenced_source`).
pub(crate) fn fence_body<'a>(text: &'a str, span: &FenceSpan) -> &'a str {
    let body = &text[span.body.clone()];
    body.strip_suffix('\r').unwrap_or(body)
}

/// What one completed line of a markdown text contributes: already-rendered
/// rows, or a completed diagram-candidate fence kept symbolic so its card
/// materializes at commit and again on a cache signal — a `Pending` card
/// resolves to its art instead of freezing `rendering...`.
#[derive(Debug, Clone)]
enum Piece {
    /// Already-rendered row (prose, table, heading, code-fallback output).
    Row(RenderRow),
    /// A completed diagram-candidate fence kept as source until
    /// materialization. Only ever constructed when the open line passed the
    /// routing grammar, `DiagramKind::parse(info)` held, and the wire context
    /// was enabled at open.
    Fence { info: Box<str>, body: Box<str> },
}

/// A deferred open fence: the open line passed the routing grammar and the
/// caller's `defer` predicate, so body lines buffer until an exact close
/// commits the piece or end-of-input flushes them as code.
#[derive(Debug)]
struct OpenFence {
    /// The structural fence the open line parsed as; its tolerant `closes`
    /// marks close candidates, which the deferred path then checks byte-exact.
    fence: Fence,
    /// The trimmed info string of the open line.
    info: String,
    /// Buffered body lines, kept unrendered until the close line resolves.
    /// One append-only string, `line` + `'\n'` per line, so the streaming
    /// tail previews it by reference — the trailing newline pops at commit.
    body: String,
}

impl OpenFence {
    /// The unfinished `tail` line's preview against this deferred fence,
    /// borrowing the buffer — nothing is written or committed. A mid-body
    /// tail renders the same rows `flush_open` will commit: the info row,
    /// then the buffered lines with the tail joined to the last one — the
    /// buffer's `'\n'` separator sits between them exactly as it will once
    /// the tail completes. A tail the fence's tolerant `closes` accepts
    /// settles the way `close_deferred` will.
    fn preview(
        &self,
        tail: &str,
        cap: usize,
        full: usize,
        mode: WidthMode,
        wire: Option<(&DiagramSettings, &RenderCache)>,
    ) -> Vec<RenderRow> {
        if self.fence.closes(tail) {
            return self.close_rows(tail, cap, full, mode, wire);
        }
        if let Some(placeholder) = pending_placeholder(&self.info, cap, mode, wire) {
            return placeholder;
        }
        let mut out = Vec::new();
        if !self.info.is_empty() {
            out.push(RenderRow::plain(escape(&self.info), Role::Dim));
        }
        out.extend(tail_body_rows(self, tail, full, mode));
        out
    }

    /// A close-candidate tail settles the way `close_deferred` will: an
    /// exact close yields the fence rows — the committed card's preview —
    /// and any other tolerant close fails the frozen grammar, is consumed
    /// as the close (never a code row), and leaves the stash as the info
    /// row plus the buffered body's code rows.
    fn close_rows(
        &self,
        tail: &str,
        cap: usize,
        full: usize,
        mode: WidthMode,
        wire: Option<(&DiagramSettings, &RenderCache)>,
    ) -> Vec<RenderRow> {
        // The tail previews the close as the text end: no prose follows an
        // unfinished line, so `rest` is empty exactly as the committed
        // frame will see it.
        if routable_close(tail, "") {
            let body = self.body.strip_suffix('\n').unwrap_or(&self.body);
            return fence_rows(&self.info, body, cap, full, mode, wire);
        }
        let mut out = Vec::new();
        if !self.info.is_empty() {
            out.push(RenderRow::plain(escape(&self.info), Role::Dim));
        }
        out.extend(buffered_code_rows(&self.body, full, mode));
        out
    }
}

/// What the lines read so far leave open for the next one: a fenced block, plus the
/// run of `|` lines held back until a delimiter row proves them a table or a
/// different line ends the run. Buffered lines keep their parsed cells so the
/// streaming tail re-lays out the run without re-parsing every line per frame.
#[derive(Debug, Default)]
struct LineState {
    /// A fence rendering as code, exactly as the unwired pipeline emits it.
    open: Option<Fence>,
    /// A deferred routable fence whose lines buffer until an exact close.
    deferred: Option<OpenFence>,
    table: Vec<TableLine>,
}

impl LineState {
    /// Renders one source line into `pieces` and advances the fence state.
    /// A routable open line defers when `defer(info)` accepts it; deferred
    /// fences buffer their body and commit one `Piece::Fence` on an exact
    /// close, while every other fence keeps today's incremental code rows.
    fn push(
        &mut self,
        logical: &str,
        rest: &str,
        cap: usize,
        full: usize,
        mode: WidthMode,
        pieces: &mut Vec<Piece>,
        defer: &dyn Fn(&str) -> bool,
    ) {
        let closing = self
            .deferred
            .as_ref()
            .is_some_and(|open| open.fence.closes(logical));
        if closing {
            self.close_deferred(logical, rest, full, mode, pieces);
            return;
        }
        if let Some(open) = &mut self.deferred {
            open.body.push_str(logical);
            open.body.push('\n');
            return;
        }
        if let Some(fence) = &self.open {
            if fence.closes(logical) {
                self.open = None;
            } else {
                pieces.extend(code_rows(logical, full, mode).into_iter().map(Piece::Row));
            }
            return;
        }
        if logical.trim_start().starts_with('|') {
            self.table.push(TableLine::of(logical, mode));
            return;
        }
        self.finish(cap, full, mode, pieces);
        if let Some((fence, info)) = Fence::opens(logical) {
            if routable_open(logical) && defer(info) {
                self.deferred = Some(OpenFence {
                    fence,
                    info: info.to_owned(),
                    body: String::new(),
                });
            } else {
                self.open = Some(fence);
                if !info.is_empty() {
                    pieces.push(Piece::Row(RenderRow::plain(escape(info), Role::Dim)));
                }
            }
        } else {
            pieces.extend(
                block_rows(logical, cap, full, mode)
                    .into_iter()
                    .map(Piece::Row),
            );
        }
    }

    /// Settles a deferred fence at its close candidate. A byte-exact ```` ``` ````
    /// close commits the buffered body as one `Piece::Fence`; anything else the
    /// tolerant `closes` accepted — an indented close, a longer run, or a close
    /// with trailing text — fails the frozen grammar, so the stash renders as
    /// the same code rows a never-deferred fence emits. Either way the fence
    /// is closed, exactly as the unwired pipeline closes it here.
    fn close_deferred(
        &mut self,
        logical: &str,
        rest: &str,
        full: usize,
        mode: WidthMode,
        pieces: &mut Vec<Piece>,
    ) {
        let Some(open) = self.deferred.take() else {
            return;
        };
        let mut body = open.body;
        if routable_close(logical, rest) {
            // The trailing '\n' the buffer carried between lines goes; the
            // piece's body bytes then equal the `fence_body` slice of the
            // same fence, so render and expand share one cache key.
            if body.ends_with('\n') {
                body.pop();
            }
            pieces.push(Piece::Fence {
                info: open.info.into(),
                body: body.into(),
            });
            return;
        }
        if !open.info.is_empty() {
            pieces.push(Piece::Row(RenderRow::plain(escape(&open.info), Role::Dim)));
        }
        pieces.extend(
            buffered_code_rows(&body, full, mode)
                .into_iter()
                .map(Piece::Row),
        );
    }

    /// Emits the buffered `|` run: a table when the second line is a delimiter row,
    /// otherwise each line as ordinary prose.
    fn finish(&mut self, cap: usize, full: usize, mode: WidthMode, pieces: &mut Vec<Piece>) {
        if !self.table.is_empty() {
            pieces.extend(
                table_rows(&self.table, None, cap, full, mode)
                    .into_iter()
                    .map(Piece::Row),
            );
            self.table.clear();
        }
    }

    /// Emits a deferred fence still open at end-of-input: the pending
    /// placeholder for CLI-backed fences (the same row [`OpenFence::preview`]
    /// shows mid-stream, so replay and render agree), otherwise today's exact
    /// fallback rows — the info row (when non-empty) plus `code_rows` per
    /// buffered body line. Non-deferred open fences need no flush — their
    /// rows committed incrementally as the lines arrived.
    fn flush_open(
        &mut self,
        cap: usize,
        full: usize,
        mode: WidthMode,
        wire: Option<(&DiagramSettings, &RenderCache)>,
        pieces: &mut Vec<Piece>,
    ) {
        let Some(open) = self.deferred.take() else {
            return;
        };
        if let Some(placeholder) = pending_placeholder(&open.info, cap, mode, wire) {
            pieces.extend(placeholder.into_iter().map(Piece::Row));
            return;
        }
        if !open.info.is_empty() {
            pieces.push(Piece::Row(RenderRow::plain(escape(&open.info), Role::Dim)));
        }
        pieces.extend(
            buffered_code_rows(&open.body, full, mode)
                .into_iter()
                .map(Piece::Row),
        );
    }

    /// The rows the unfinished `tail` line would emit, borrowing the state —
    /// nothing is cloned, written or committed, exactly the invariant the
    /// pre-diagram pipeline proved. Fence and table branches mirror
    /// [`LineState::push`] on the shared input; `wire` stays the caller's
    /// context. A deferred fence previews through [`OpenFence::preview`]:
    /// mid-body tails show the info row plus the buffered lines with the
    /// tail joined on, and close candidates settle as `close_deferred` will.
    fn tail_rows(
        &self,
        tail: &str,
        cap: usize,
        full: usize,
        mode: WidthMode,
        wire: Option<(&DiagramSettings, &RenderCache)>,
    ) -> Vec<RenderRow> {
        if let Some(open) = &self.deferred {
            return open.preview(tail, cap, full, mode, wire);
        }
        if let Some(fence) = &self.open {
            if fence.closes(tail) {
                return Vec::new();
            }
            return code_rows(tail, full, mode);
        }
        if tail.trim_start().starts_with('|') {
            let line = TableLine::of(tail, mode);
            return table_rows(&self.table, Some(&line), cap, full, mode);
        }
        let mut out = Vec::new();
        if !self.table.is_empty() {
            out.extend(table_rows(&self.table, None, cap, full, mode));
        }
        if let Some((_, info)) = Fence::opens(tail) {
            if !info.is_empty() {
                out.push(RenderRow::plain(escape(info), Role::Dim));
            }
        } else {
            out.extend(block_rows(tail, cap, full, mode));
        }
        out
    }
}

/// The code rows of a deferred fence's buffered body, which carries one
/// `'\n'` separator per buffered line. Stripping it keeps the split
/// identical to the `close_deferred`/`flush_open` commit paths; an empty
/// buffer emits nothing.
fn buffered_code_rows(body: &str, full: usize, mode: WidthMode) -> Vec<RenderRow> {
    let stripped = body.strip_suffix('\n').unwrap_or(body);
    if stripped.is_empty() {
        return Vec::new();
    }
    stripped
        .split('\n')
        .flat_map(|line| code_rows(line, full, mode))
        .collect()
}

/// The tail joined to a deferred fence's buffered body, previewing the
/// split `flush_open` performs once the tail completes: the buffer gains
/// `tail` + `'\n'`, the flush pops that separator, and the join `body` +
/// `tail` splits — the buffered lines in order, then the tail as the last
/// piece. Every split piece emits, including empty ones (a blank buffered
/// line is a `|` row here even though a lone `'\n'` buffer commits
/// nothing: the separator belongs to its line, the tail fills the piece
/// after it). The only empty join is an empty buffer with an empty tail,
/// which emits nothing. Borrowing only; a CRLF line keeps its `'\r'`.
fn tail_body_rows(open: &OpenFence, tail: &str, full: usize, mode: WidthMode) -> Vec<RenderRow> {
    if open.body.is_empty() && tail.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    if !open.body.is_empty() {
        let completed = open.body.strip_suffix('\n').unwrap_or(&open.body);
        for line in completed.split('\n') {
            out.extend(code_rows(line, full, mode));
        }
    }
    out.extend(code_rows(tail, full, mode));
    out
}

/// Rows of a markdown text that only grows, rendered once per completed line.
///
/// Rendering is per line with a carried fence state, so the rows of a completed
/// line never change; only the trailing unfinished line is parsed again on each
/// call, while a buffered table run re-lays out from its cached cells. A change
/// of layout starts over.
///
/// Committed work is cached, not replayed: every completed line renders once
/// into `committed_rows` when its piece arrives, and each re-render copies at
/// most the last `limit` rows. A fence piece renders at commit; while its
/// card is still pending, a `RenderCache::generation` roll renders just that
/// piece again and splices its rows into place, so a landed card never
/// copies the rows around it. Only a wire-context change re-materializes
/// every committed piece. Nothing of `LineState` or the committed rows is
/// cloned per frame: the unfinished tail renders against the borrowed state.
#[derive(Debug, Default)]
pub(crate) struct MarkdownStream {
    layout: Option<(usize, usize, WidthMode)>,
    consumed: usize,
    state: LineState,
    pieces: Vec<Piece>,
    /// The committed pieces' rows, in piece order: rows of `Piece::Row`
    /// copied once at commit, fence cards materialized by [`Self::refresh`].
    committed_rows: Vec<RenderRow>,
    /// Pieces already accounted for in `committed_rows`.
    committed_pieces: usize,
    /// The fence pieces whose card is still the pending placeholder, in
    /// piece order, with the rows they occupy in `committed_rows`.
    pending_fences: Vec<PendingFence>,
    /// The wire settings the committed rows were rendered under; a change
    /// re-materializes fence pieces, so a mid-stream settings toggle
    /// re-routes stored cards to code rows.
    wired: Option<DiagramSettings>,
    /// The `RenderCache::generation` those rows were rendered under.
    generation: u64,
}

/// A deferred fence piece still showing its pending placeholder.
#[derive(Debug)]
struct PendingFence {
    piece: usize,
    rows: Range<usize>,
}

impl MarkdownStream {
    /// Forgets every rendered line; the next text starts a new stream.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// The last `limit` rows [`markdown_wired_rows`] gives for `text`, which
    /// must extend the text of the previous call. `wire` offers completed
    /// routable fences to the diagram engine exactly as the batch render does.
    pub(crate) fn rows(
        &mut self,
        text: &str,
        cap: usize,
        full: usize,
        mode: WidthMode,
        limit: usize,
        wire: Option<(&DiagramSettings, &RenderCache)>,
    ) -> Vec<RenderRow> {
        if self.layout != Some((cap, full, mode)) {
            self.reset();
            self.layout = Some((cap, full, mode));
        }
        let defer = |info: &str| {
            wire.is_some_and(|(settings, _)| settings.enabled) && DiagramKind::parse(info).is_some()
        };
        while let Some(end) = text[self.consumed..].find('\n') {
            let line = &text[self.consumed..self.consumed + end];
            let rest = &text[self.consumed + end + 1..];
            self.state
                .push(line, rest, cap, full, mode, &mut self.pieces, &defer);
            self.consumed += end + 1;
        }
        self.refresh(cap, full, mode, wire);
        // The unfinished tail renders against the borrowed state — nothing is
        // cloned, written or committed — so a close tail materializes early
        // and a mid-body tail previews the same info plus code rows the
        // commit path emits, while `self.state` stays untouched.
        let tail = self
            .state
            .tail_rows(&text[self.consumed..], cap, full, mode, wire);
        if self.committed_rows.is_empty() && tail.is_empty() {
            return vec![RenderRow::plain(String::new(), Role::Text)];
        }
        let kept = limit
            .saturating_sub(tail.len())
            .min(self.committed_rows.len());
        let start = self.committed_rows.len() - kept;
        let mut out = Vec::with_capacity(kept + tail.len());
        out.extend_from_slice(&self.committed_rows[start..]);
        out.extend(tail);
        // A tail longer than the limit — a buffered table run still growing —
        // keeps its last rows, exactly as the whole render ends.
        out.drain(..out.len().saturating_sub(limit));
        out
    }

    /// Accounts for newly committed pieces after the cache signals are
    /// settled. A `Piece::Row` renders once, at commit; a `Piece::Fence`
    /// renders at commit and, while its card is pending, again when
    /// `RenderCache::generation` rolls — the existing cache signal, no
    /// second cache. That roll renders only the pending pieces and splices
    /// their rows in place; the committed rows around them are never copied.
    /// A wire-context change is the one rebuild of every committed piece.
    fn refresh(
        &mut self,
        cap: usize,
        full: usize,
        mode: WidthMode,
        wire: Option<(&DiagramSettings, &RenderCache)>,
    ) {
        let generation = wire.map_or(self.generation, |(_, cache)| cache.generation());
        let settings = wire.map(|(settings, _)| *settings);
        let rewired = settings != self.wired;
        let landed = generation != self.generation;
        self.generation = generation;
        self.wired = settings;
        if rewired {
            self.committed_rows.clear();
            self.pending_fences.clear();
            self.committed_pieces = 0;
        } else if landed {
            self.resolve_pending(cap, full, mode, wire);
        }
        while self.committed_pieces < self.pieces.len() {
            self.commit_piece(cap, full, mode, wire);
        }
    }

    /// Commits the next piece: a prose row copies once, a fence renders its
    /// card and, still pending, is remembered for [`Self::resolve_pending`].
    fn commit_piece(
        &mut self,
        cap: usize,
        full: usize,
        mode: WidthMode,
        wire: Option<(&DiagramSettings, &RenderCache)>,
    ) {
        let at = self.committed_pieces;
        let start = self.committed_rows.len();
        match &self.pieces[at] {
            Piece::Row(row) => self.committed_rows.push(row.clone()),
            Piece::Fence { .. } => {
                let rows = self.fence_piece_rows(at, cap, full, mode, wire);
                if rows.iter().any(|row| row.pending_diagram) {
                    self.pending_fences.push(PendingFence {
                        piece: at,
                        rows: start..start + rows.len(),
                    });
                }
                self.committed_rows.extend(rows);
            }
        }
        self.committed_pieces += 1;
    }

    /// Renders again each fence still showing its placeholder and splices
    /// the new rows over the old ones. A card that is still pending stays
    /// listed at its shifted position; every other committed row keeps its
    /// buffer and only moves when an earlier card changes height.
    fn resolve_pending(
        &mut self,
        cap: usize,
        full: usize,
        mode: WidthMode,
        wire: Option<(&DiagramSettings, &RenderCache)>,
    ) {
        let mut added = 0;
        let mut removed = 0;
        let mut still = Vec::new();
        for fence in std::mem::take(&mut self.pending_fences) {
            let start = fence.rows.start + added - removed;
            let old = start..start + fence.rows.len();
            let rows = self.fence_piece_rows(fence.piece, cap, full, mode, wire);
            if rows.iter().any(|row| row.pending_diagram) {
                still.push(PendingFence {
                    piece: fence.piece,
                    rows: start..start + rows.len(),
                });
            }
            removed += old.len();
            added += rows.len();
            self.committed_rows.splice(old, rows).for_each(drop);
        }
        self.pending_fences = still;
    }

    /// The rows one piece occupies: the committed row itself, or the fence's
    /// card or code fallback. Called at commit and on cache signals only.
    fn fence_piece_rows(
        &self,
        piece: usize,
        cap: usize,
        full: usize,
        mode: WidthMode,
        wire: Option<(&DiagramSettings, &RenderCache)>,
    ) -> Vec<RenderRow> {
        match &self.pieces[piece] {
            Piece::Row(row) => vec![row.clone()],
            Piece::Fence { info, body } => fence_rows(info, body, cap, full, mode, wire),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Fence {
    marker: char,
    length: usize,
}

impl Fence {
    fn opens(line: &str) -> Option<(Self, &str)> {
        let trimmed = indent_trimmed(line)?;
        let marker = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'))?;
        let length = trimmed.chars().take_while(|c| *c == marker).count();
        if length < 3 {
            return None;
        }
        let info = trimmed[length..].trim();
        if marker == '`' && info.contains('`') {
            return None;
        }
        Some((Self { marker, length }, info))
    }

    fn closes(&self, line: &str) -> bool {
        let Some(trimmed) = indent_trimmed(line) else {
            return false;
        };
        let run = trimmed.chars().take_while(|c| *c == self.marker).count();
        run >= self.length && trimmed[run..].trim().is_empty()
    }
}

/// Strips up to three leading spaces; four or more make an indented code line.
fn indent_trimmed(line: &str) -> Option<&str> {
    let trimmed = line.trim_start_matches(' ');
    (line.len() - trimmed.len() <= 3).then_some(trimmed)
}

/// One source line of a fenced block: a faint `|` rule, then the line wrapped at `full`.
fn code_rows(line: &str, full: usize, mode: WidthMode) -> Vec<RenderRow> {
    let content = full.saturating_sub(2).max(1);
    let escaped = escape(line);
    wrap(&escaped, content, mode)
        .into_iter()
        .map(|piece| {
            let mut row = RenderRow::plain(
                if piece.is_empty() {
                    "|".to_owned()
                } else {
                    format!("| {piece}")
                },
                Role::Text,
            );
            row.spans.push(RenderSpan {
                range: 0..1,
                role: Role::Faint,
                bold: false,
            });
            row
        })
        .collect()
}

/// One prose line: block prefix, inline spans, wrap, and hanging indent.
fn block_rows(line: &str, cap: usize, full: usize, mode: WidthMode) -> Vec<RenderRow> {
    let (visible, links) = linked_text(line);
    if visible.is_empty() {
        return vec![RenderRow::plain(String::new(), Role::Text)];
    }
    let block = Block::of(&visible);
    let width = if block.table { full } else { cap };
    let indent = crate::width::width(&block.marker, mode);
    let body = &visible[block.consumed..];
    let links = links
        .into_iter()
        .filter(|link| link.range.end > block.consumed)
        .map(|link| RenderLink {
            range: link.range.start.saturating_sub(block.consumed)..link.range.end - block.consumed,
            url: link.url,
        })
        .collect();
    let base = Style {
        role: Role::Text,
        bold: block.bold,
    };
    let line = inline(body, links, base);
    let continuation = " ".repeat(indent);
    wrap_rich_line(&line, width.saturating_sub(indent), mode)
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            let prefix = if index == 0 {
                block.marker.as_str()
            } else {
                continuation.as_str()
            };
            indent_row(row, prefix)
        })
        .collect()
}

/// The rows of a buffered `|` run plus an optional unfinished tail line joining it.
/// When the second line is a delimiter row the run is a table and renders as
/// measured ASCII columns; anything else stays ordinary prose wrapping at `full`,
/// exactly as a lone `|` line always did.
fn table_rows(
    lines: &[TableLine],
    tail: Option<&TableLine>,
    cap: usize,
    full: usize,
    mode: WidthMode,
) -> Vec<RenderRow> {
    let mut run: Vec<&TableLine> = lines.iter().collect();
    run.extend(tail);
    let Some(table) = Table::of(&run, full) else {
        return run
            .iter()
            .flat_map(|line| block_rows(&line.text, cap, full, mode))
            .collect();
    };
    table.render(mode)
}

/// One cell of a table line: the unescaped source text, its styled form under a
/// plain base, and the styled form's cell width. Parsed once when the line arrives
/// so streamed re-renders of a buffered run only re-measure the tail line.
#[derive(Debug)]
struct TableCell {
    raw: String,
    rich: RichLine,
    natural: usize,
}

/// One buffered `|` line with its cells already split and parsed.
#[derive(Debug)]
struct TableLine {
    text: String,
    cells: Vec<TableCell>,
}

impl TableLine {
    fn of(text: &str, mode: WidthMode) -> Self {
        let plain = Style {
            role: Role::Text,
            bold: false,
        };
        let cells = table_cells(text)
            .into_iter()
            .map(|raw| {
                let (visible, links) = linked_text(&raw);
                let rich = inline(&visible, links, plain);
                let natural = crate::width::width(&rich.text, mode);
                TableCell { raw, rich, natural }
            })
            .collect();
        Self {
            text: text.to_owned(),
            cells,
        }
    }
}

/// One delimiter directive: `:--` left, `--:` right, `:-:` center, `---` default left.
#[derive(Clone, Copy)]
enum Align {
    Left,
    Right,
    Center,
}

/// A parsed table: styled cells, delimiter alignment, and per-column cell widths
/// chosen so the rendered row never exceeds the full-width measure.
struct Table<'a> {
    head: Vec<RichLine>,
    body: Vec<Vec<Option<&'a RichLine>>>,
    align: Vec<Align>,
    widths: Vec<usize>,
}

impl<'a> Table<'a> {
    /// Reads `lines` as a table: a header line, a delimiter line with the same cell
    /// count, and zero or more body lines. Returns `None` when the run is not one —
    /// or when not every column fits the minimum `| x ` shape, so the caller falls
    /// back to lossless prose wrapping and no column is silently dropped.
    fn of(lines: &[&'a TableLine], full: usize) -> Option<Self> {
        let head = &lines.first()?.cells;
        let columns = head.len();
        let align: Vec<Align> = lines.get(1).map(|line| {
            line.cells
                .iter()
                .map(|cell| rule_align(&cell.raw))
                .collect::<Option<Vec<_>>>()
        })??;
        if columns == 0 || columns != align.len() || 4 * columns + 1 > full {
            return None;
        }
        let header = Style {
            role: Role::Text,
            bold: true,
        };
        let head: Vec<RichLine> = head
            .iter()
            .map(|cell| {
                let (visible, cell_links) = linked_text(&cell.raw);
                inline(&visible, cell_links, header)
            })
            .collect();
        let mut natural: Vec<usize> = lines[0].cells.iter().map(|cell| cell.natural).collect();
        let mut body = Vec::new();
        for &line in &lines[2..] {
            let mut row: Vec<Option<&'a RichLine>> = Vec::with_capacity(columns);
            for (at, cell) in line.cells.iter().take(columns).enumerate() {
                natural[at] = natural[at].max(cell.natural);
                row.push(Some(&cell.rich));
            }
            row.resize(columns, None);
            body.push(row);
        }
        Some(Self {
            head,
            body,
            align,
            widths: column_widths(&natural, full),
        })
    }

    /// Emits the header row (bold), the `-` rule row, and each body row, all at
    /// the column widths chosen in [`Table::of`].
    fn render(&self, mode: WidthMode) -> Vec<RenderRow> {
        let mut rows = Vec::with_capacity(self.body.len() + 2);
        rows.push(self.row(self.head.iter().map(Some), mode));
        rows.push(RenderRow::plain(self.rule_row(), Role::Faint));
        rows.extend(
            self.body
                .iter()
                .map(|cells| self.row(cells.iter().copied(), mode)),
        );
        rows
    }

    /// The rule row under the header: `-` per column and `|` separators so every
    /// painted character stays ASCII.
    fn rule_row(&self) -> String {
        let mut text = String::from("|");
        for width in &self.widths {
            for _ in 0..width + 2 {
                text.push('-');
            }
            text.push('|');
        }
        text
    }

    /// One rendered line of the table: `| ` opens every column and ` |` closes it.
    /// Body cells missing at this row's end paint as empty columns.
    fn row<'c>(
        &self,
        cells: impl IntoIterator<Item = Option<&'c RichLine>>,
        mode: WidthMode,
    ) -> RenderRow {
        let mut row = RenderRow::plain(String::new(), Role::Text);
        for (at, cell) in cells.into_iter().take(self.widths.len()).enumerate() {
            row.text.push_str("| ");
            let width = self.widths[at];
            let Some(cell) = cell else {
                row.text.extend(std::iter::repeat_n(' ', width + 1));
                continue;
            };
            let (content, spans, links) = clip_cell(cell, width, mode);
            let pad = width.saturating_sub(crate::width::width(&content, mode));
            let before = match self.align[at] {
                Align::Left => 0,
                Align::Center => pad / 2,
                Align::Right => pad,
            };
            let offset = row.text.len() + before;
            row.text.extend(std::iter::repeat_n(' ', before));
            row.text.push_str(&content);
            row.spans.extend(spans.into_iter().map(|mut span| {
                span.range.start += offset;
                span.range.end += offset;
                span
            }));
            row.links.extend(links.into_iter().map(|mut link| {
                link.range.start += offset;
                link.range.end += offset;
                link
            }));
            row.text.extend(std::iter::repeat_n(' ', pad - before + 1));
        }
        row.text.push('|');
        row
    }
}

/// The cells of one table line: split at each unescaped `|`, at most one leading
/// and one trailing delimiter pipe stripped, `\|` unescaped to `|`, and the
/// surrounding space dropped. Empty cells, including edge ones, are preserved.
fn table_cells(line: &str) -> Vec<String> {
    let line = line.trim();
    let mut start = usize::from(line.starts_with('|'));
    let mut end = line.len();
    if end > start && line.as_bytes()[end - 1] == b'|' && !escaped_pipe(line, end - 1) {
        end -= 1;
    }
    let mut cells = Vec::new();
    let mut from = start;
    let bytes = line.as_bytes();
    while start < end {
        if bytes[start] == b'|' && !escaped_pipe(line, start) {
            cells.push(unescape_cell(line[from..start].trim()));
            from = start + 1;
        }
        start += 1;
    }
    cells.push(unescape_cell(line[from..end].trim()));
    cells
}

/// Whether the `|` at `index` is escaped by an odd-length run of backslashes.
fn escaped_pipe(line: &str, index: usize) -> bool {
    let mut backslashes = 0;
    for byte in line.as_bytes()[..index].iter().rev() {
        if *byte != b'\\' {
            break;
        }
        backslashes += 1;
    }
    backslashes % 2 == 1
}

/// One cell with `\|` unescaped to `|`; the escape backslash is consumed.
fn unescape_cell(cell: &str) -> String {
    if !cell.contains("\\|") {
        return cell.to_owned();
    }
    let mut out = String::with_capacity(cell.len());
    let mut backslashes = 0;
    for ch in cell.chars() {
        if ch == '|' && backslashes % 2 == 1 {
            out.pop();
        }
        backslashes = if ch == '\\' { backslashes + 1 } else { 0 };
        out.push(ch);
    }
    out
}

/// The delimiter directive for one cell, or `None` when the cell is not `:?-+:?`.
fn rule_align(cell: &str) -> Option<Align> {
    let cell = cell.trim();
    if cell.is_empty() {
        return None;
    }
    let core = cell.trim_matches(':');
    if core.is_empty() || !core.chars().all(|c| c == '-') {
        return None;
    }
    Some(match (cell.starts_with(':'), cell.ends_with(':')) {
        (true, true) => Align::Center,
        (false, true) => Align::Right,
        _ => Align::Left,
    })
}

/// Shares `budget` cells across columns: every column gets its natural width when
/// the whole table fits; otherwise columns that fit under an equal share keep their
/// width and the remaining columns split what is left. The caller only invokes
/// this when every column affords at least one content cell; a table that small
/// is prose, so no column is ever dropped.
fn column_widths(natural: &[usize], budget: usize) -> Vec<usize> {
    let columns = natural.len();
    let mut widths = vec![0; columns];
    if columns == 0 {
        return widths;
    }
    // The caller guarantees `4 * columns + 1 <= budget`, so `usable >= columns`:
    // every column keeps at least one content cell and none is dropped.
    let usable = (budget - (3 * columns + 1)).max(columns);
    let mut open: Vec<usize> = (0..columns).collect();
    let mut left = usable;
    loop {
        let share = (left / open.len()).max(1);
        let mut fixed_any = false;
        open.retain(|column| {
            if natural[*column] <= share {
                widths[*column] = natural[*column].min(left);
                left -= widths[*column];
                fixed_any = true;
                false
            } else {
                true
            }
        });
        if open.is_empty() {
            break;
        }
        if !fixed_any {
            let extra = left % open.len();
            for (index, column) in open.iter().enumerate() {
                widths[*column] = left / open.len() + usize::from(index < extra);
            }
            break;
        }
    }
    widths
}

/// The visible form of one cell at `cells` columns: unchanged when it fits, else a
/// grapheme prefix plus the ASCII `...` overflow cue. Spans and links clip to the
/// shown prefix.
fn clip_cell(
    cell: &RichLine,
    cells: usize,
    mode: WidthMode,
) -> (String, Vec<RenderSpan>, Vec<RenderLink>) {
    if crate::width::width(&cell.text, mode) <= cells {
        return (cell.text.clone(), cell.spans.clone(), cell.links.clone());
    }
    // Under four cells there is no room for the overflow cue: clip at the grapheme
    // boundary only. Spans and links clip to the shown prefix either way, so a
    // narrow bold header cell stays bold and its links stay clickable.
    let cue = if cells >= 4 { 3 } else { 0 };
    let cut = take_cells(&cell.text, cells - cue, mode).len();
    let mut text = cell.text[..cut].to_owned();
    text.push_str(&"..."[..cue]);
    let spans = cell
        .spans
        .iter()
        .filter(|span| span.range.start < cut)
        .map(|span| RenderSpan {
            range: span.range.start..span.range.end.min(cut),
            ..span.clone()
        })
        .collect();
    let links = cell
        .links
        .iter()
        .filter(|link| link.range.start < cut)
        .map(|link| RenderLink {
            range: link.range.start..link.range.end.min(cut),
            url: link.url.clone(),
        })
        .collect();
    (text, spans, links)
}

/// The block-level reading of one line: what to strip and what to show instead.
struct Block {
    consumed: usize,
    marker: String,
    bold: bool,
    table: bool,
}

impl Block {
    fn of(line: &str) -> Self {
        let plain = |consumed| Self {
            consumed,
            marker: String::new(),
            bold: false,
            table: line.trim_start().starts_with('|'),
        };
        let hashes = line.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') {
            return Self {
                consumed: hashes + 1,
                marker: String::new(),
                bold: true,
                table: false,
            };
        }
        let spaces = line.len() - line.trim_start_matches(' ').len();
        let rest = &line[spaces..];
        let indent = " ".repeat(spaces);
        let mut chars = rest.chars();
        if let (Some('-' | '*' | '+'), Some(' ')) = (chars.next(), chars.next()) {
            let after = &rest[2..];
            let (mark, extra) = match after.get(..4) {
                Some("[ ] ") => ("[ ] ", 4),
                Some("[x] " | "[X] ") => ("[x] ", 4),
                _ => ("- ", 0),
            };
            return Self {
                consumed: spaces + 2 + extra,
                marker: format!("{indent}{mark}"),
                bold: false,
                table: false,
            };
        }
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if (1..=9).contains(&digits) {
            let tail = &rest[digits..];
            if tail.starts_with(". ") || tail.starts_with(") ") {
                return Self {
                    consumed: spaces + digits + 2,
                    marker: format!("{indent}{}", &rest[..digits + 2]),
                    bold: false,
                    table: false,
                };
            }
        }
        plain(0)
    }
}

/// How one run of text is drawn.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Style {
    role: Role,
    bold: bool,
}

/// Resolves `**strong**` and `` `code` `` in `visible`, removing the markers and
/// moving link ranges onto the shortened text. Every run starts from `base`.
fn inline(visible: &str, links: Vec<RenderLink>, base: Style) -> RichLine {
    let mut line = RichLine {
        text: String::with_capacity(visible.len()),
        links: Vec::new(),
        spans: Vec::new(),
    };
    // `moved[i]` is where byte `i` of `visible` lands in `line.text`.
    let mut moved = vec![0; visible.len() + 1];
    let runs = tick_runs(visible);
    let mut next_run = 0;
    let mut strong = false;
    let mut index = 0;
    while index < visible.len() {
        let rest = &visible[index..];
        let (consumed, lead, content, style) = if rest.starts_with('`') {
            while runs[next_run].start < index {
                next_run += 1;
            }
            tick_step(
                visible,
                &runs,
                next_run,
                Style {
                    bold: base.bold || strong,
                    ..base
                },
            )
        } else if strong_marker(visible, index, strong) {
            strong = !strong;
            (2, 0, "", base)
        } else {
            let len = rest.chars().next().map_or(1, char::len_utf8);
            let style = Style {
                role: base.role,
                bold: base.bold || strong,
            };
            (len, 0, &rest[..len], style)
        };
        moved[index..index + lead].fill(line.text.len());
        for offset in 0..content.len() {
            moved[index + lead + offset] = line.text.len() + offset;
        }
        push(&mut line, content, style);
        moved[index + lead + content.len()..index + consumed].fill(line.text.len());
        index += consumed;
    }
    moved[visible.len()] = line.text.len();
    line.links = links
        .into_iter()
        .filter_map(|link| {
            let range = moved[link.range.start]..moved[link.range.end];
            (range.start < range.end).then_some(RenderLink {
                range,
                url: link.url,
            })
        })
        .collect();
    line
}

/// Reads the backtick run `runs[at]`: a code span through its closing run, or the bare run.
/// Returns bytes consumed, the opening bytes to drop, the text shown, and its style.
fn tick_step<'a>(
    visible: &'a str,
    runs: &[TickRun],
    at: usize,
    base: Style,
) -> (usize, usize, &'a str, Style) {
    let open = &runs[at];
    let Some(closer) = open.closer.map(|closer| &runs[closer]) else {
        return (
            open.len,
            0,
            &visible[open.start..open.start + open.len],
            base,
        );
    };
    let style = Style {
        role: Role::Accent,
        ..base
    };
    let content = &visible[open.start + open.len..closer.start];
    (
        closer.start + closer.len - open.start,
        open.len,
        content,
        style,
    )
}

/// A maximal run of backticks in a line, and the next run of the same length.
struct TickRun {
    start: usize,
    len: usize,
    closer: Option<usize>,
}

/// Every backtick run in `text`, each linked to the next run of equal length.
///
/// A run with no later equal-length run is not an opener: it stays literal as a whole.
fn tick_runs(text: &str) -> Vec<TickRun> {
    let bytes = text.as_bytes();
    let mut runs = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'`' {
            index += 1;
            continue;
        }
        let start = index;
        while bytes.get(index) == Some(&b'`') {
            index += 1;
        }
        runs.push(TickRun {
            start,
            len: index - start,
            closer: None,
        });
    }
    let mut later: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for at in (0..runs.len()).rev() {
        runs[at].closer = later.insert(runs[at].len, at);
    }
    runs
}

/// Whether the `**` at `index` closes strong text (`strong`) or opens a span that closes later.
fn strong_marker(whole: &str, index: usize, strong: bool) -> bool {
    let rest = &whole[index..];
    if !rest.starts_with("**") {
        return false;
    }
    if strong {
        return whole[..index]
            .chars()
            .next_back()
            .is_some_and(|c| !c.is_whitespace());
    }
    rest[2..].chars().next().is_some_and(|c| !c.is_whitespace()) && has_closer(&rest[2..])
}

/// Whether `tail` holds a `**` that can close strong text.
fn has_closer(tail: &str) -> bool {
    tail.match_indices("**").any(|(at, _)| {
        tail[..at]
            .chars()
            .next_back()
            .is_some_and(|c| !c.is_whitespace())
    })
}

/// Appends `piece` in `style`, merging with the previous span when it continues it.
fn push(line: &mut RichLine, piece: &str, style: Style) {
    if piece.is_empty() {
        return;
    }
    let start = line.text.len();
    line.text.push_str(piece);
    if style.role == Role::Text && !style.bold {
        return;
    }
    if let Some(last) = line.spans.last_mut()
        && last.range.end == start
        && last.role == style.role
        && last.bold == style.bold
    {
        last.range.end = line.text.len();
        return;
    }
    line.spans.push(RenderSpan {
        range: start..line.text.len(),
        role: style.role,
        bold: style.bold,
    });
}

#[cfg(test)]
mod tests {
    use super::{MarkdownStream, markdown_wired_rows};
    use crate::render::RenderRow;
    use crate::theme::Role;
    use crate::width::WidthMode;

    /// The unwired render oracle: every production surface passes a wire
    /// context, so only the tests still ask for `wire = None`.
    fn markdown_rows(text: &str, cap: usize, full: usize, mode: WidthMode) -> Vec<RenderRow> {
        markdown_wired_rows(text, cap, full, mode, None)
    }

    fn rows(text: &str, cap: usize) -> Vec<RenderRow> {
        markdown_rows(text, cap, 40, WidthMode::Narrow)
    }

    fn texts(text: &str, cap: usize) -> Vec<String> {
        rows(text, cap).into_iter().map(|row| row.text).collect()
    }

    fn styled(row: &RenderRow) -> Vec<(&str, Role, bool)> {
        row.spans
            .iter()
            .map(|span| (&row.text[span.range.clone()], span.role, span.bold))
            .collect()
    }

    #[test]
    fn strong_and_inline_code_lose_their_markers_and_keep_their_style() {
        let rows = rows("Use **bold** and `code` here", 40);
        assert_eq!(rows[0].text, "Use bold and code here");
        assert_eq!(
            styled(&rows[0]),
            [("bold", Role::Text, true), ("code", Role::Accent, false)]
        );
    }

    #[test]
    fn headings_are_bold_and_bullets_normalize_to_dashes() {
        let rows = rows(
            "## Plan\n* one\n+ two\n- [x] done\n- [ ] todo\n3. third",
            40,
        );
        let text: Vec<&str> = rows.iter().map(|row| row.text.as_str()).collect();
        assert_eq!(
            text,
            ["Plan", "- one", "- two", "[x] done", "[ ] todo", "3. third"]
        );
        assert_eq!(styled(&rows[0]), [("Plan", Role::Text, true)]);
    }

    #[test]
    fn unmatched_markers_stay_literal() {
        assert_eq!(
            texts("2 ** 3 and `open and **x", 40),
            ["2 ** 3 and `open and **x"]
        );
    }

    #[test]
    fn list_continuations_hang_under_their_text() {
        // Break whitespace stays at the end of the closing row, so continuation
        // text starts in the same column as the first row's text.
        assert_eq!(
            texts("- aaaa bbbb cccc", 8),
            ["- aaaa ", "  bbbb ", "  cccc"]
        );
        for row in texts("- aaaa bbbb cccc", 8) {
            let text_at = row
                .find('a')
                .or_else(|| row.find('b'))
                .or_else(|| row.find('c'));
            assert_eq!(text_at, Some(2), "{row:?}");
        }
    }

    #[test]
    fn fenced_code_shows_info_then_ruled_lines_and_ignores_markers() {
        let rows = rows("before\n```rust\nlet **x** = `y`;\n\n```\nafter", 40);
        let text: Vec<&str> = rows.iter().map(|row| row.text.as_str()).collect();
        assert_eq!(text, ["before", "rust", "| let **x** = `y`;", "|", "after"]);
        assert_eq!(rows[1].role, Role::Dim);
        assert_eq!(styled(&rows[2]), [("|", Role::Faint, false)]);
    }

    #[test]
    fn an_open_fence_keeps_streaming_text_as_code() {
        assert_eq!(texts("```\nlet x", 40), ["| let x"]);
    }

    #[test]
    fn code_wraps_at_full_width_not_the_prose_cap() {
        let long = "x".repeat(30);
        let rows = markdown_rows(&format!("```\n{long}\n```"), 10, 40, WidthMode::Narrow);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, format!("| {long}"));
    }

    #[test]
    fn links_survive_marker_removal() {
        let rows = rows("**see** [the docs](file:///tmp/docs) now", 40);
        assert_eq!(rows[0].text, "see the docs now");
        assert_eq!(rows[0].links.len(), 1);
        assert_eq!(&rows[0].text[rows[0].links[0].range.clone()], "the docs");
    }

    #[test]
    fn strong_spans_split_across_wrapped_rows() {
        let rows = rows("**alpha beta gamma**", 11);
        assert_eq!(
            rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>(),
            ["alpha beta ", "gamma"]
        );
        assert!(
            rows.iter()
                .all(|row| row.spans.iter().all(|span| span.bold))
        );
    }

    #[test]
    fn blank_lines_keep_their_row() {
        assert_eq!(texts("one\n\ntwo", 40), ["one", "", "two"]);
    }

    fn first(text: &str) -> RenderRow {
        markdown_rows(text, 4096, 4096, WidthMode::Narrow).remove(0)
    }

    #[test]
    fn an_unmatched_run_stays_literal_as_a_whole() {
        let row = first("``` a `` b");
        assert_eq!(row.text, "``` a `` b");
        assert_eq!(row.spans.len(), 0);
    }

    #[test]
    fn a_run_closes_only_on_an_equal_run_and_keeps_inner_runs() {
        let row = first("x ``a ` b`` y ` z");
        assert_eq!(row.text, "x a ` b y ` z");
        assert_eq!(row.spans.len(), 1);
        assert_eq!(row.spans[0].role, Role::Accent);
        assert_eq!(&row.text[row.spans[0].range.clone()], "a ` b");
    }

    #[test]
    fn a_long_unmatched_run_renders_in_linear_time() {
        let row = first(&format!("x{}tail", "`".repeat(200_000)));
        assert_eq!(row.text, format!("x{}", "`".repeat(4095)));
        assert_eq!(row.spans.len(), 0);
    }

    fn wide(text: &str, full: usize) -> Vec<String> {
        markdown_rows(text, full, full, WidthMode::Narrow)
            .into_iter()
            .map(|row| row.text)
            .collect()
    }

    #[test]
    fn tables_render_columns_with_bold_header_and_a_rule_row() {
        let rows = markdown_rows(
            "| name | detail |\n| ---- | -----: |\n| alpha | 9 |\n| a very long name | 42 |",
            10,
            20,
            WidthMode::Narrow,
        );
        assert_eq!(
            rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>(),
            [
                "| name    | detail |",
                "|---------|--------|",
                "| alpha   |      9 |",
                "| a ve... |     42 |",
            ]
        );
        assert!(rows[0].spans.iter().all(|span| span.bold));
        assert_eq!(rows[1].role, Role::Faint);
    }

    #[test]
    fn table_cells_truncate_with_an_ascii_cue() {
        // Budget 14 affords both columns: the wide cell clips to `lo...` and the
        // narrow column still paints its full content.
        let rows = markdown_rows(
            "| aa | bb |\n| -- | -- |\n| loooong | x |",
            8,
            14,
            WidthMode::Narrow,
        );
        assert_eq!(
            rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>(),
            ["| aa    | bb |", "|-------|----|", "| lo... | x  |"]
        );
    }

    #[test]
    fn a_table_that_cannot_fit_its_columns_renders_as_prose() {
        // `4n + 1 > full` for four columns at full 10: nothing is dropped, the
        // run wraps as lossless prose so every cell stays reachable.
        let rows = wide(
            "| 1 | 2 | 3 | 4 |\n| - | - | - | - |\n| a | b | c | d |",
            10,
        );
        assert!(rows.concat().contains('4'));
        assert!(rows.concat().contains('d'));
        assert_eq!(rows[0], "| 1 | 2 | ");
    }

    #[test]
    fn escaped_pipes_and_empty_edge_cells_stay_in_their_cells() {
        // `\|` inside a code span is cell content, not a delimiter; the row keeps
        // its arity and `union` is never pushed into a phantom column.
        let rows = markdown_rows(
            "| a | b |\n| - | - |\n| `x \\| y` | union |",
            40,
            40,
            WidthMode::Narrow,
        );
        let body: Vec<&str> = rows.iter().map(|row| row.text.as_str()).collect();
        assert_eq!(
            body,
            [
                "| a     | b     |",
                "|-------|-------|",
                "| x | y | union |"
            ]
        );
        // Empty edge cells are preserved: `|| b |` is two columns, not one.
        let rows = markdown_rows("|| b |\n| - | - |\n|| c |", 40, 40, WidthMode::Narrow);
        let body: Vec<&str> = rows.iter().map(|row| row.text.as_str()).collect();
        assert_eq!(body, ["|  | b |", "|--|---|", "|  | c |"]);
    }

    #[test]
    fn a_pipe_line_without_a_delimiter_stays_prose() {
        assert_eq!(wide("| just a | pipe line", 40), ["| just a | pipe line"]);
    }

    #[test]
    fn a_table_between_prose_lines_flushes_at_its_end() {
        assert_eq!(
            wide("intro\n| a |\n| - |\n| 1 |\noutro", 40),
            ["intro", "| a |", "|---|", "| 1 |", "outro"]
        );
    }

    const STREAMED: &str = "# Title\nsome **bold** `code` text that wraps around the cap\n```rust\nlet x = 1;\n\n```\n- item one that also wraps\n| a | b |\ntext between the lone pipe and the table\n| name | val |\n| ---- | --- |\n| left | 1 |\nafter the table\n~~~\nopen fence\n";

    #[test]
    fn a_stream_matches_a_full_render_at_every_prefix() {
        for (cap, full) in [(10, 20), (40, 40)] {
            let mut stream = MarkdownStream::default();
            let ends = STREAMED
                .char_indices()
                .map(|(index, _)| index)
                .chain([STREAMED.len()]);
            for end in ends {
                let prefix = &STREAMED[..end];
                let whole = markdown_rows(prefix, cap, full, WidthMode::Narrow);
                let all = stream.rows(prefix, cap, full, WidthMode::Narrow, usize::MAX, None);
                assert_eq!(all, whole, "prefix {prefix:?}");
                let tail = stream.rows(prefix, cap, full, WidthMode::Narrow, 3, None);
                assert_eq!(
                    tail,
                    whole[whole.len().saturating_sub(3)..],
                    "tail {prefix:?}"
                );
            }
        }
    }

    #[test]
    fn a_stream_renders_each_completed_line_once() {
        let mut stream = MarkdownStream::default();
        let text = "alpha beta gamma\ndelta epsilon\nzeta";
        stream.rows(text, 8, 8, WidthMode::Narrow, usize::MAX, None);
        assert_eq!(stream.consumed, "alpha beta gamma\ndelta epsilon\n".len());
        let completed = stream.committed_pieces;
        stream.rows(text, 8, 8, WidthMode::Narrow, usize::MAX, None);
        assert_eq!(stream.committed_pieces, completed);
        // The frame with no new lines and no cache signal re-renders nothing:
        // committed rows stay in place and only the suffix is copied.
        let rows_before = stream.committed_rows.len();
        let again = stream.rows(text, 8, 8, WidthMode::Narrow, usize::MAX, None);
        assert_eq!(stream.committed_pieces, completed);
        assert_eq!(stream.committed_rows.len(), rows_before);
        // The completed window is exactly what the batch render produces for
        // the same text — no fixed length to go stale when wrapping changes.
        assert_eq!(again, markdown_rows(text, 8, 8, WidthMode::Narrow));
        stream.rows(
            "alpha beta gamma\ndelta epsilon\nzeta eta\n",
            8,
            8,
            WidthMode::Narrow,
            1,
            None,
        );
        assert!(stream.committed_pieces > completed);
    }

    #[test]
    fn a_stream_tail_borrows_instead_of_cloning_state() {
        // The tail renders against the borrowed state: after any frame the
        // buffered table cells and the deferred fence are still exactly the
        // state's own — no scratch clone was rendered and dropped.
        let mut stream = MarkdownStream::default();
        stream.rows("| a | b |\n| -", 8, 8, WidthMode::Narrow, usize::MAX, None);
        // The header completed into the run; the unfinished `| -` tail is
        // only previewed against the run, never buffered into the state.
        assert_eq!(stream.state.table.len(), 1);
        stream.rows(
            "| a | b |\n| - | - |\n| 1 |",
            8,
            8,
            WidthMode::Narrow,
            usize::MAX,
            None,
        );
        assert_eq!(stream.state.table.len(), 2);
    }

    #[test]
    fn a_stream_starts_over_when_the_layout_changes() {
        let mut stream = MarkdownStream::default();
        stream.rows(STREAMED, 10, 20, WidthMode::Narrow, usize::MAX, None);
        let narrow = stream.rows(STREAMED, 40, 40, WidthMode::Narrow, usize::MAX, None);
        assert_eq!(narrow, markdown_rows(STREAMED, 40, 40, WidthMode::Narrow));
    }

    mod wired {
        /// Waits for one background render to land, under a generous deadline.
        ///
        /// A full-suite run shares the machine with every other test binary, so
        /// the renderer's scheduling delay is unbounded in wall time. The wait
        /// stays bounded so a renderer that never lands still fails the test.
        fn wait_for_render(cache: &RenderCache, generation: u64) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while cache.generation() == generation {
                assert!(
                    std::time::Instant::now() < deadline,
                    "background renderer did not finish"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }

        use super::super::{MarkdownStream, markdown_wired_rows};
        use crate::diagram::{DiagramSettings, RenderCache};
        use crate::render::RenderRow;
        use crate::width::WidthMode;

        fn enabled() -> (DiagramSettings, RenderCache) {
            (DiagramSettings { enabled: true }, RenderCache::default())
        }

        fn texts(rows: &[RenderRow]) -> Vec<&str> {
            rows.iter().map(|row| row.text.as_str()).collect()
        }

        #[test]
        fn an_embedded_mermaid_fence_renders_as_a_card_between_prose() {
            let (settings, cache) = enabled();
            let rows = markdown_wired_rows(
                "before\n```mermaid\ngraph TD; A-->B\n```\nafter",
                40,
                40,
                WidthMode::Narrow,
                Some((&settings, &cache)),
            );
            let texts = texts(&rows);
            // The fence leaves no code rows behind: no info line, no ruled
            // source, and the surrounding prose keeps its order around the
            // materialized card.
            assert!(
                !texts
                    .iter()
                    .any(|row| row.contains("graph TD") || row.contains("```")),
                "{texts:?}"
            );
            assert_eq!(texts.first(), Some(&"before"));
            assert_eq!(texts.last(), Some(&"after"));
            assert_eq!(cache.renders(), 1);
        }

        #[test]
        fn two_identical_diagrams_share_one_render() {
            let (settings, cache) = enabled();
            let block = "```mermaid\ngraph TD; A-->B\n```";
            let text = format!("{block}\nmiddle\n{block}");
            let rows =
                markdown_wired_rows(&text, 40, 40, WidthMode::Narrow, Some((&settings, &cache)));
            let texts = texts(&rows);
            assert!(texts.contains(&"middle"), "{texts:?}");
            // Cache identity excludes position: the second occurrence hits
            // the first render.
            assert_eq!(cache.renders(), 1);
        }

        #[test]
        fn a_terminal_crlf_close_routes_and_shares_the_expand_cache_key() {
            let (settings, cache) = enabled();
            // The whole-block path (`fenced_source`) trims the text end, so
            // the final close may carry the CRLF bytes; the embedded path
            // must agree with it, and the wired body must match the bytes
            // `fence_body` slices for expansion.
            let text = "before\n```mermaid\ngraph TD; A-->B\r\n```\r\n";
            let generation = cache.generation();
            let _ = markdown_wired_rows(text, 40, 40, WidthMode::Narrow, Some((&settings, &cache)));
            wait_for_render(&cache, generation);
            let wired =
                markdown_wired_rows(text, 40, 40, WidthMode::Narrow, Some((&settings, &cache)));
            let texts = texts(&wired);
            // A routed card replaces the fence entirely: no `mermaid` info
            // row (also absent from pending/fallback cards, which embed the
            // kind label) and no `"| "`-ruled code rows — the unrouted
            // signature — just the rendered art after the prose.
            assert!(
                !texts.iter().any(|row| row.contains("mermaid")),
                "a text-end CRLF close routes the card: {texts:?}"
            );
            assert!(
                !texts.iter().any(|row| *row == "|" || row.starts_with("| ")),
                "{texts:?}"
            );
            assert_eq!(texts.first(), Some(&"before"));
            assert_eq!(cache.renders(), 1);
            // The committed body shares `fence_body`'s bytes, so the plain
            // LF close resolves through the same cache entry — one render
            // serves both texts, and expand hits the same key.
            let lf = markdown_wired_rows(
                "before\n```mermaid\ngraph TD; A-->B\n```\n",
                40,
                40,
                WidthMode::Narrow,
                Some((&settings, &cache)),
            );
            assert_eq!(lf, wired);
            assert_eq!(cache.renders(), 1, "CRLF and LF closes share one render");
            // An interior `"```\r"` with more prose after it stays code: the
            // frozen grammar widens only at the text end.
            let (settings, cache) = enabled();
            let interior = "before\n```mermaid\nx\n```\r\nafter";
            let wired = markdown_wired_rows(
                interior,
                40,
                40,
                WidthMode::Narrow,
                Some((&settings, &cache)),
            );
            assert_eq!(
                wired,
                markdown_wired_rows(interior, 40, 40, WidthMode::Narrow, None),
            );
            assert_eq!(cache.renders(), 0);
        }

        #[test]
        fn an_unclosed_or_non_routable_fence_stays_code() {
            let (settings, cache) = enabled();
            for text in [
                "before\n```mermaid\ngraph TD; A-->B",
                "before\n~~~mermaid\ngraph TD; A-->B\n~~~\nafter",
                "before\n ```mermaid\ngraph TD; A-->B\n ```\nafter",
                "before\n````mermaid\ngraph TD; A-->B\n````\nafter",
                "before\n```mermaid\ngraph TD; A-->B\n ```\nafter",
                "before\n```mermaid\ngraph TD; A-->B\n````\nafter",
                "before\n```graph\na\n```\nafter",
            ] {
                let wired =
                    markdown_wired_rows(text, 40, 40, WidthMode::Narrow, Some((&settings, &cache)));
                let plain = markdown_wired_rows(text, 40, 40, WidthMode::Narrow, None);
                assert_eq!(wired, plain, "{text:?}");
                assert!(
                    texts(&wired)
                        .iter()
                        .any(|row| row.contains("graph TD") || row.contains('a')),
                    "{text:?}"
                );
            }
            // No routable fence ever closed: nothing was scheduled.
            assert_eq!(cache.renders(), 0);
        }

        #[test]
        fn disabled_settings_render_fences_as_code() {
            let settings = DiagramSettings { enabled: false };
            let cache = RenderCache::default();
            let rows = markdown_wired_rows(
                "before\n```mermaid\ngraph TD; A-->B\n```\nafter",
                40,
                40,
                WidthMode::Narrow,
                Some((&settings, &cache)),
            );
            assert_eq!(
                texts(&rows),
                ["before", "mermaid", "| graph TD; A-->B", "after"]
            );
            assert_eq!(cache.renders(), 0);
        }

        #[test]
        fn a_pending_cli_card_materializes_through_the_stream() {
            let (settings, cache) = enabled();
            let mut stream = MarkdownStream::default();
            let wire = Some((&settings, &cache));
            // A d2 fence schedules a worker: the piece materializes as the
            // pending placeholder while the renderer runs, and the hold flag
            // reaches the entry commit.
            let text = "prose\n```d2\na -> b\n```\nmore\n";
            let rows = stream.rows(text, 40, 40, WidthMode::Narrow, usize::MAX, wire);
            let texts = texts(&rows);
            assert!(
                texts.iter().any(|row| row.contains("prose"))
                    && texts.iter().any(|row| row.contains("more")),
                "{texts:?}"
            );
            assert!(
                rows.iter()
                    .any(|row| row.pending_diagram && row.text.contains("d2")),
                "a scheduled d2 render holds the card pending: {texts:?}"
            );
            // The stream's own rows resolve without a full reseed once the
            // worker lands: the next frame rides the cache generation roll.
            assert!(stream.committed_rows.iter().any(|row| row.pending_diagram));
            let generation = cache.generation();
            wait_for_render(&cache, generation);
            assert!(
                cache.generation() > generation,
                "background renderer did not finish"
            );
            let rows = stream.rows(text, 40, 40, WidthMode::Narrow, usize::MAX, wire);
            assert!(
                !rows.iter().any(|row| row.pending_diagram),
                "the resolved card replaces the placeholder on the next frame: {:?}",
                rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>()
            );
        }

        #[test]
        fn an_incomplete_cli_fence_holds_the_pending_placeholder() {
            // An unclosed CLI-backed fence (d2/nomnoml render async) is still
            // pending while the stream is incomplete: batch replay and live
            // render must agree on the pending placeholder so the transition
            // stays stable once the close lands the card. Sync kinds
            // (mermaid) and unwired renders keep today's code rows.
            for (text, kind) in [
                ("intro\n```d2\na -> b", "d2"),
                ("intro\n```d2\na -> b\n", "d2"),
                ("intro\n```nomnoml\n[a -> b]", "nomnoml"),
            ] {
                let (settings, cache) = enabled();
                let wire = Some((&settings, &cache));
                let whole = markdown_wired_rows(text, 40, 40, WidthMode::Narrow, wire);
                assert_eq!(whole.len(), 2, "{text:?}: {whole:?}");
                assert_eq!(whole[0].text.as_str(), "intro");
                assert!(
                    whole[1].pending_diagram
                        && whole[1].text.contains("rendering")
                        && whole[1].text.contains(kind),
                    "{text:?}: {whole:?}"
                );
                let mut stream = MarkdownStream::default();
                let streamed = stream.rows(text, 40, 40, WidthMode::Narrow, usize::MAX, wire);
                assert_eq!(streamed, whole, "{text:?}");
                // The placeholder is unscheduled: no worker runs on a partial
                // source, so the unwired budget rule still holds.
                assert_eq!(cache.renders(), 0);
            }
            // Sync-kind incomplete fences keep code rows.
            let (settings, cache) = enabled();
            let wire = Some((&settings, &cache));
            let mermaid = "before\n```mermaid\ngraph TD; A-->B";
            assert_eq!(
                markdown_wired_rows(mermaid, 40, 40, WidthMode::Narrow, wire),
                markdown_wired_rows(mermaid, 40, 40, WidthMode::Narrow, None),
            );
            assert_eq!(cache.renders(), 0);
        }

        /// Length and address of each committed prose row's text buffer. A
        /// row that was cloned owns a new buffer, so an address that moved
        /// names bytes the stream copied again.
        #[cfg(unix)]
        fn prose_buffers(stream: &MarkdownStream) -> Vec<(usize, *const u8)> {
            stream
                .committed_rows
                .iter()
                .filter(|row| row.text.starts_with("prior ") || row.text.starts_with("after "))
                .map(|row| (row.text.len(), row.text.as_ptr()))
                .collect()
        }

        /// The bytes of `before`'s rows that `after` holds in new buffers.
        #[cfg(unix)]
        fn recopied_bytes(before: &[(usize, *const u8)], after: &[(usize, *const u8)]) -> usize {
            before
                .iter()
                .zip(after)
                .filter(|(was, now)| was.1 != now.1)
                .map(|(was, _)| was.0)
                .sum()
        }

        #[test]
        #[cfg(unix)]
        fn a_landed_card_copies_only_its_pending_segment() {
            use std::fmt::Write as _;
            use std::os::unix::fs::PermissionsExt;
            let settings = DiagramSettings { enabled: true };
            // A slow fake d2: the worker cannot settle during the first
            // frame, so the placeholder is deterministic; when it lands the
            // fallback card is taller than the one-row placeholder.
            let tools = tempfile::tempdir().expect("temporary tool directory");
            let d2 = tools.path().join("d2");
            std::fs::write(&d2, b"#!/bin/sh\n/bin/sleep 1\nexit 1\n").expect("fake d2 tool");
            std::fs::set_permissions(&d2, std::fs::Permissions::from_mode(0o700))
                .expect("executable fake tool");
            let cache = RenderCache::with_fonts(
                Some(tools.path().as_os_str().to_owned()),
                crate::diagram::FontSnapshot::default(),
            );
            let wire = Some((&settings, &cache));
            let mut text = String::new();
            for index in 0..2000 {
                let _ = writeln!(text, "prior line {index:04}");
            }
            text.push_str("```d2\na -> b\n```\nafter one\nafter two\n");
            let generation = cache.generation();
            let mut stream = MarkdownStream::default();
            let rows = stream.rows(&text, 40, 40, WidthMode::Narrow, usize::MAX, wire);
            assert!(
                rows.iter().any(|row| row.pending_diagram),
                "{:?}",
                texts(&rows)
            );
            let before = prose_buffers(&stream);
            assert_eq!(before.len(), 2002);

            wait_for_render(&cache, generation);
            let rows = stream.rows(&text, 40, 40, WidthMode::Narrow, usize::MAX, wire);
            assert!(!rows.iter().any(|row| row.pending_diagram));
            assert_eq!(
                rows,
                markdown_wired_rows(&text, 40, 40, WidthMode::Narrow, wire)
            );
            let after = prose_buffers(&stream);
            assert_eq!(after.len(), before.len());
            assert_eq!(
                recopied_bytes(&before, &after),
                0,
                "the transition re-copied committed rows outside the pending card"
            );

            // The frames after the transition stay incremental.
            text.push_str("later\n");
            let rows = stream.rows(&text, 40, 40, WidthMode::Narrow, usize::MAX, wire);
            assert_eq!(
                rows,
                markdown_wired_rows(&text, 40, 40, WidthMode::Narrow, wire)
            );
            assert_eq!(recopied_bytes(&before, &prose_buffers(&stream)), 0);
        }

        #[test]
        fn a_stream_matches_wired_rows_at_every_prefix() {
            // Deferred fences exercise every preview path a routable fence
            // leaves unfinished: a mid-body tail, an empty buffer, a blank
            // buffered line, a non-exact close, and the exact close.
            for text in [
                "intro\n```mermaid\ngraph TD; A-->B\n```\noutro",
                "intro\n```mermaid\n\n\ngraph TD; A-->B\n```\noutro",
                "intro\n```mermaid\nx\n ```\noutro",
            ] {
                let (settings, cache) = enabled();
                for end in (0..=text.len()).filter(|end| text.is_char_boundary(*end)) {
                    let prefix = &text[..end];
                    let wire = Some((&settings, &cache));
                    let mut stable = None;
                    // The cache may gain a render between these two reads at
                    // any point, and a full-suite run stretches that window.
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                    while std::time::Instant::now() < deadline {
                        let generation = cache.generation();
                        let mut stream = MarkdownStream::default();
                        let streamed =
                            stream.rows(prefix, 40, 40, WidthMode::Narrow, usize::MAX, wire);
                        let whole = markdown_wired_rows(prefix, 40, 40, WidthMode::Narrow, wire);
                        if cache.generation() == generation {
                            stable = Some((streamed, whole));
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    let (streamed, whole) =
                        stable.expect("diagram cache generation did not stabilize");
                    assert_eq!(streamed, whole, "prefix {prefix:?}");
                }
            }
        }

        #[test]
        fn fence_spans_indexes_completed_fences_only() {
            use super::super::{fence_body, fence_spans};
            let text = "a\n```mermaid\nx\r\n```\nb\n~~~\ny\n~~~\n```d2\nz\n```\n```rust\ntrailing";
            let spans = fence_spans(text);
            assert_eq!(spans.len(), 3, "{spans:?}");
            assert!(spans[0].routable);
            assert_eq!(&text[spans[0].info.clone()], "mermaid");
            assert_eq!(fence_body(text, &spans[0]), "x");
            // The tilde fence completes structurally but is not routable.
            assert!(!spans[1].routable);
            assert_eq!(fence_body(text, &spans[1]), "y");
            assert!(spans[2].routable);
            assert_eq!(&text[spans[2].info.clone()], "d2");
            assert_eq!(fence_body(text, &spans[2]), "z");
            // A `"```\r"` close routes only when the rest of the text is a
            // bare `\r`/`\n` run: the shapes the whole-text trim accepted.
            let crlf = "```mermaid\nx\n```\r\n";
            let spans = fence_spans(crlf);
            assert_eq!(spans.len(), 1);
            assert!(spans[0].routable, "the text-end CRLF close routes");
            assert_eq!(fence_body(crlf, &spans[0]), "x");
            // An interior `"```\r\n"` with prose after it stays non-routable.
            let interior = "```mermaid\nx\n```\r\nafter";
            assert!(fence_spans(interior).iter().all(|span| !span.routable));
            // The unclosed tail fence produces no span.
            assert!(
                fence_spans("```mermaid\nx\n ```")
                    .iter()
                    .all(|span| !span.routable)
            );
        }
    }
}
