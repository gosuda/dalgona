package rpcserver

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"slices"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
)

const (
	historyLifetime     = 5 * time.Minute
	historyPageDefault  = 20
	historyPageMax      = 100
	historyPageBytes    = 1 << 20
	pendingRequestLimit = 64
	pendingRequestBytes = 256 << 10
	directionOlder      = "older"
	directionNewer      = "newer"
	cursorSeparator     = "."
	historyTurnOverhead = 512
)

var daemonHistories atomic.Int64

func historyMethods() map[string]method {
	return map[string]method{
		"dalgona/history/open":         deadline(PageDeadline, (*server).historyOpen),
		"dalgona/history/page":         deadline(PageDeadline, (*server).historyPage),
		"dalgona/history/content/read": deadline(PageDeadline, (*server).historyContentRead),
		"dalgona/history/close":        (*server).historyClose,
	}
}

func (s *server) announceReplay() replayer {
	if s.native.Load() != nil {
		return (*tracker).replayLive
	}
	return (*tracker).replayAll
}

type historyTable struct {
	mu        sync.Mutex
	snapshots map[string]*historySnapshot
	held      int
}

type frozenItem struct {
	id      string
	turnID  string
	kind    string
	field   string
	content frozenContent
	digest  string
}

type historySnapshot struct {
	id         string
	threadID   string
	revision   string
	path       []event.Seqd
	spans      []turnSpan
	created    event.SessionCreated
	activeTurn string
	active     []frozenItem
	expires    time.Time
	reads      int
	retired    bool
}

type pageItem struct {
	item   rpc.Item
	blobs  fieldBlobs
	frozen *frozenItem
}

type historyCursor struct {
	turn int
	item int
}

func newHistoryTable() *historyTable {
	return &historyTable{snapshots: map[string]*historySnapshot{}}
}

func (t *historyTable) acquire(id string, now time.Time) (*historySnapshot, error) {
	t.mu.Lock()
	defer t.mu.Unlock()
	snap, ok := t.snapshots[id]
	if !ok {
		return nil, rpc.NativeKind(rpc.ErrorExpired)
	}
	if now.After(snap.expires) {
		t.retireLocked(snap)
		return nil, rpc.NativeKind(rpc.ErrorExpired)
	}
	snap.expires = now.Add(historyLifetime)
	snap.reads++
	return snap, nil
}

func (t *historyTable) done(snap *historySnapshot) {
	t.mu.Lock()
	defer t.mu.Unlock()
	snap.reads--
	if snap.reads == 0 && snap.retired {
		t.releaseLocked(snap)
	}
}

func (t *historyTable) retireLocked(snap *historySnapshot) {
	if t.snapshots[snap.id] != snap {
		return
	}
	delete(t.snapshots, snap.id)
	snap.retired = true
	if snap.reads == 0 {
		t.releaseLocked(snap)
	}
}

func (t *historyTable) releaseLocked(snap *historySnapshot) {
	t.held--
	daemonHistories.Add(-1)
	snap.release()
}

func (t *historyTable) sweepLocked(now time.Time) {
	for _, snap := range t.snapshots {
		if now.After(snap.expires) {
			t.retireLocked(snap)
		}
	}
}

func (t *historyTable) publish(next *historySnapshot, replace string, now time.Time) error {
	t.mu.Lock()
	defer t.mu.Unlock()
	t.sweepLocked(now)
	old, replacing, err := t.replaceTarget(replace, next)
	if err != nil {
		return err
	}
	if replacing && old.reads == 0 {
		delete(t.snapshots, old.id)
		old.release()
	} else {
		if err := t.admit(); err != nil {
			return err
		}
		if replacing {
			t.retireLocked(old)
		}
	}
	t.snapshots[next.id] = next
	return nil
}

func (t *historyTable) replaceTarget(replace string, next *historySnapshot) (*historySnapshot, bool, error) {
	old, replacing := t.snapshots[replace]
	if replace != "" && (!replacing || old.threadID != next.threadID) {
		return nil, false, rpc.NativeKind(rpc.ErrorExpired)
	}
	return old, replacing, nil
}

func (t *historyTable) admit() error {
	if t.held >= connectionSnapshots {
		return rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	if daemonHistories.Add(1) > daemonSnapshots {
		daemonHistories.Add(-1)
		return rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	t.held++
	return nil
}

func (t *historyTable) close(id string) {
	t.mu.Lock()
	defer t.mu.Unlock()
	if snap, ok := t.snapshots[id]; ok {
		t.retireLocked(snap)
	}
}

func (t *historyTable) closeThread(threadID string) {
	t.mu.Lock()
	defer t.mu.Unlock()
	for _, snap := range t.snapshots {
		if snap.threadID == threadID {
			t.retireLocked(snap)
		}
	}
}

func (t *historyTable) closeAll() {
	t.mu.Lock()
	defer t.mu.Unlock()
	for _, snap := range t.snapshots {
		t.retireLocked(snap)
	}
}

func (s *server) historyOpen(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	native := s.native.Load()
	if native == nil {
		return nil, rpc.NativeKind(rpc.ErrorIncompatible)
	}
	threadID, replace, err := decodeHistoryOpen(params)
	if err != nil {
		return nil, err
	}
	info, ok := s.info(threadID)
	if !ok {
		return nil, rpc.NativeKind(rpc.ErrorNotFound)
	}
	thread, err := s.open(ctx, threadID)
	if err != nil {
		return nil, err
	}
	if _, _, watchErr := s.watch(ctx, thread, (*tracker).replayLive); watchErr != nil {
		return nil, watchErr
	}
	return s.snapshotResult(ctx, native, s.threadObject(info, threadStatus(thread), nil), replace)
}

func decodeHistoryOpen(params jsontext.Value) (string, string, error) {
	var threadID, replace string
	err := decodeParams(params, stringMembers(map[string]*string{"threadId": &threadID, "replaceHistoryId": &replace}))
	if err != nil || threadID == "" || len(threadID) > rpc.IDLimit || len(replace) > rpc.IDLimit {
		return "", "", rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return threadID, replace, nil
}

func (s *server) snapshotResult(ctx context.Context, native *nativeSession, thread rpc.Thread, replace string) (jsontext.Value, error) {
	s.mu.Lock()
	w := s.watches[thread.ID]
	s.mu.Unlock()
	if w == nil {
		return nil, rpc.NativeKind(rpc.ErrorUnavailable)
	}
	snap, cut, err := s.capture(ctx, w)
	if err != nil {
		return nil, err
	}
	if pubErr := s.publishSnapshot(native, w, snap, replace); pubErr != nil {
		snap.release()
		return nil, pubErr
	}
	raw, marshalErr := json.Marshal(rpc.HistorySnapshot{
		HistoryID: snap.id, Thread: thread, PathRevision: snap.revision,
		StreamID: native.streamID, CutOrdinal: rpc.FormatDecimal(cut.Ordinal), ActiveTurnID: snap.activeTurn,
		PendingRequests: cut.Requests, NewestCursor: snap.cursor(historyCursor{turn: max(len(snap.spans)-1, 0)}),
		OldestCursor: snap.cursor(historyCursor{}), ExpiresAt: rpc.FormatTimestamp(snap.expires),
	})
	if marshalErr != nil {
		native.histories.close(snap.id)
		return nil, internalError(marshalErr)
	}
	return raw, nil
}

func (s *server) publishSnapshot(native *nativeSession, w *watch, snap *historySnapshot, replace string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closing || s.watches[snap.threadID] != w {
		return rpc.NativeKind(rpc.ErrorUnavailable)
	}
	return native.histories.publish(snap, replace, time.Now())
}

func (s *server) capture(ctx context.Context, w *watch) (*historySnapshot, rpc.Cut, error) {
	if err := w.waitReady(ctx); err != nil {
		return nil, rpc.Cut{}, err
	}
	threadID := w.thread.ID()
	w.apply.Lock()
	if w.overflowed != nil && w.overflowed() {
		w.apply.Unlock()
		return nil, rpc.Cut{}, rpc.NativeKind(rpc.ErrorUnavailable)
	}
	cut, _ := s.conn.Cut(threadID)
	head := w.head
	active, err := w.live.freeze()
	w.apply.Unlock()
	if err != nil {
		return nil, rpc.Cut{}, err
	}
	snap := &historySnapshot{id: newHandle(), threadID: threadID, active: active, expires: time.Now().Add(historyLifetime)}
	if err := checkPending(cut.Requests); err != nil {
		snap.release()
		return nil, rpc.Cut{}, err
	}
	if err := s.index(snap, w, head); err != nil {
		snap.release()
		return nil, rpc.Cut{}, err
	}
	return snap, cut, nil
}

func checkPending(requests []rpc.PendingRequest) error {
	raw, err := json.Marshal(requests)
	if err != nil || len(requests) > pendingRequestLimit || len(raw) > pendingRequestBytes {
		return rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	return nil
}

func (s *server) index(snap *historySnapshot, w *watch, head uint64) error {
	records, _, err := w.thread.Records()
	if err != nil {
		return hostError(err, snap.threadID, "")
	}
	snap.indexRecords(records[:min(head, uint64(len(records)))], head)
	for i := range snap.active {
		if err := snap.active[i].seal(); err != nil {
			return err
		}
	}
	return nil
}

func (snap *historySnapshot) indexRecords(records []event.Seqd, head uint64) {
	snap.path = event.ActivePath(records)
	snap.spans = turnSpans(snap.path)
	if created, ok := sessionCreated(records); ok {
		snap.created = created
	}
	if open := openTurnStart(snap.path); open < len(snap.path) && len(snap.spans) > 0 {
		snap.activeTurn = turnIDAt(snap.path, snap.spans[len(snap.spans)-1])
	}
	snap.revision = "r" + rpc.FormatDecimal(head)
}

func (p *liveProjection) freeze() ([]frozenItem, error) {
	frozen := make([]frozenItem, 0, len(p.items))
	for _, item := range p.items {
		content, err := item.acc.freeze()
		if err != nil {
			releaseFrozen(frozen)
			return nil, err
		}
		frozen = append(frozen, frozenItem{id: item.id, turnID: item.turnID, kind: item.kind, field: item.field, content: content})
	}
	return frozen, nil
}

func releaseFrozen(items []frozenItem) {
	for _, item := range items {
		item.content.release()
	}
}

func (f *frozenItem) seal() error {
	digest := sha256.New()
	for offset := int64(0); offset < f.content.length; offset += contentMaxLimit {
		chunk, err := f.content.read(offset, contentMaxLimit)
		if err != nil {
			return internalError(err)
		}
		digest.Write(chunk)
	}
	f.digest = hex.EncodeToString(digest.Sum(nil))
	return nil
}

func (snap *historySnapshot) release() { releaseFrozen(snap.active) }

func (snap *historySnapshot) cursor(at historyCursor) string {
	return strings.Join([]string{snap.id, strconv.Itoa(at.turn), strconv.Itoa(at.item)}, cursorSeparator)
}

func (snap *historySnapshot) parseCursor(raw string) (historyCursor, bool) {
	parts := strings.Split(raw, cursorSeparator)
	if len(parts) != 3 || parts[0] != snap.id {
		return historyCursor{}, false
	}
	turn, turnErr := strconv.Atoi(parts[1])
	item, itemErr := strconv.Atoi(parts[2])
	if turnErr != nil || itemErr != nil || turn < 0 || item < 0 || turn >= max(len(snap.spans), 1) {
		return historyCursor{}, false
	}
	return historyCursor{turn: turn, item: item}, true
}

type pageQuery struct {
	historyID string
	cursor    string
	direction string
	limit     int
}

func (q *pageQuery) member(name string, d *jsontext.Decoder) error {
	var err error
	switch name {
	case "historyId":
		q.historyID, err = rpc.ReadString(d)
	case "cursor":
		q.cursor, err = rpc.ReadString(d)
	case "direction":
		q.direction, err = rpc.ReadString(d)
	case "limit":
		q.limit, err = rpc.ReadInt(d)
	default:
		err = d.SkipValue()
	}
	return err
}

func (q *pageQuery) valid() bool {
	if q.direction == "" {
		q.direction = directionOlder
	}
	if q.limit == 0 {
		q.limit = historyPageDefault
	}
	return q.historyID != "" && q.limit > 0 && q.limit <= historyPageMax && (q.direction == directionOlder || q.direction == directionNewer)
}

func (s *server) historyPage(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	native := s.native.Load()
	if native == nil {
		return nil, rpc.NativeKind(rpc.ErrorIncompatible)
	}
	var q pageQuery
	if err := decodeParams(params, q.member); err != nil || !q.valid() {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	snap, err := native.histories.acquire(q.historyID, time.Now())
	if err != nil {
		return nil, err
	}
	defer native.histories.done(snap)
	start, err := s.pageStart(snap, q)
	if err != nil {
		return nil, err
	}
	page, err := s.buildPage(ctx, snap, q, start)
	if err != nil {
		return nil, err
	}
	raw, err := json.Marshal(page)
	if err != nil {
		return nil, internalError(err)
	}
	return raw, nil
}

func (s *server) pageStart(snap *historySnapshot, q pageQuery) (historyCursor, error) {
	start := historyCursor{}
	if q.direction == directionOlder {
		start.turn = max(len(snap.spans)-1, 0)
	}
	if q.cursor == "" {
		return start, nil
	}
	cursor, ok := snap.parseCursor(q.cursor)
	if !ok {
		return historyCursor{}, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return cursor, nil
}

func (s *server) buildPage(ctx context.Context, snap *historySnapshot, q pageQuery, start historyCursor) (rpc.HistoryPage, error) {
	page := rpc.HistoryPage{Turns: []rpc.HistoryTurn{}, PathRevision: snap.revision}
	step := pageStep(q.direction)
	if previous := start.turn - step; start.item == 0 && previous >= 0 && previous < len(snap.spans) {
		page.PreviousCursor = snap.cursor(historyCursor{turn: previous})
	}
	turns, next, err := s.pageTurns(ctx, snap, q, start, historyPageBytes-historyTurnOverhead)
	if err != nil {
		return rpc.HistoryPage{}, err
	}
	page.Turns = turns
	page.NextCursor = next
	if len(turns) > 0 && next == "" {
		page.NextCursor = s.trailingCursor(snap, start, step, len(turns))
	}
	return page, nil
}

func pageStep(direction string) int {
	if direction == directionNewer {
		return 1
	}
	return -1
}

func (s *server) trailingCursor(snap *historySnapshot, start historyCursor, step, count int) string {
	next := start.turn + step*count
	if next < 0 || next >= len(snap.spans) {
		return ""
	}
	return snap.cursor(historyCursor{turn: next})
}

func (s *server) pageTurns(ctx context.Context, snap *historySnapshot, q pageQuery, start historyCursor, budget int) ([]rpc.HistoryTurn, string, error) {
	step := pageStep(q.direction)
	turns := []rpc.HistoryTurn{}
	for at := start; at.turn >= 0 && at.turn < len(snap.spans) && len(turns) < q.limit; at = (historyCursor{turn: at.turn + step}) {
		if err := ctx.Err(); err != nil {
			return nil, "", err
		}
		turn, used, err := s.turnPage(snap, at, budget, len(turns) == 0)
		if err != nil {
			return nil, "", err
		}
		if turn.ItemsComplete {
			turns = append(turns, turn)
			budget -= used
			continue
		}
		if len(turn.Items) > 0 {
			turns = append(turns, turn)
		}
		return turns, snap.cursor(historyCursor{turn: at.turn, item: turn.NextItemIndex}), nil
	}
	return turns, "", nil
}

func (s *server) turnPage(snap *historySnapshot, at historyCursor, budget int, first bool) (rpc.HistoryTurn, int, error) {
	turn, items := s.turnItems(snap, at.turn)
	if at.item > len(items) {
		return rpc.HistoryTurn{}, 0, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	out := rpc.HistoryTurn{
		TurnID: turn.ID, TurnRevision: snap.turnRevision(at.turn), Status: turn.Status, Error: turn.Error,
		StartedAt: turn.StartedAt, CompletedAt: turn.CompletedAt, DurationMs: turn.DurationMs,
		FirstItemIndex: at.item, NextItemIndex: at.item, Items: []rpc.ItemHeader{},
	}
	empty, err := json.Marshal(out)
	if err != nil {
		return rpc.HistoryTurn{}, 0, internalError(err)
	}
	used := len(empty) + historyTurnOverhead
	for index := at.item; index < len(items); index++ {
		header, err := snap.header(at.turn, index, items[index])
		if err != nil {
			return rpc.HistoryTurn{}, 0, err
		}
		raw, err := json.Marshal(header)
		if err != nil {
			return rpc.HistoryTurn{}, 0, internalError(err)
		}
		if used+len(raw)+1 > budget && (len(out.Items) > 0 || !first) {
			return out, used, nil
		}
		used += len(raw) + 1
		out.Items = append(out.Items, header)
		out.NextItemIndex = index + 1
	}
	out.ItemsComplete = true
	return out, used, nil
}

func (snap *historySnapshot) turnRevision(index int) string {
	span := snap.spans[index]
	revision := rpc.FormatDecimal(snap.path[span.start].Seq) + "-" + rpc.FormatDecimal(snap.path[span.end-1].Seq)
	if index == len(snap.spans)-1 && snap.activeTurn != "" {
		revision += "+" + snap.revision
	}
	return revision
}

func (s *server) turnItems(snap *historySnapshot, index int) (rpc.Turn, []pageItem) {
	span := snap.spans[index]
	tr := s.contentTracker(snap.threadID)
	tr.created = snap.created
	var turn rpc.Turn
	turns, blobs := tr.collectSourced(snap.path[span.start:span.end])
	if len(turns) > 0 {
		turn = turns[0]
	}
	items := make([]pageItem, 0, len(turn.Items))
	for _, it := range turn.Items {
		items = append(items, pageItem{item: it, blobs: blobs[it.ID]})
	}
	if index != len(snap.spans)-1 || snap.activeTurn == "" {
		return turn, items
	}
	for i := range snap.active {
		frozen := &snap.active[i]
		if frozen.turnID != snap.activeTurn {
			continue
		}
		if at := slices.IndexFunc(items, func(item pageItem) bool { return item.item.ID == frozen.id }); at >= 0 {
			items[at].frozen = frozen
			continue
		}
		items = append(items, pageItem{item: rpc.Item{Type: frozen.kind, ID: frozen.id}, frozen: frozen})
	}
	return turn, items
}

func (snap *historySnapshot) header(turn, index int, item pageItem) (rpc.ItemHeader, error) {
	source := rpc.ContentSource{Kind: sourceSnapshot, HistoryID: snap.id}
	contentID := func(field string) string {
		return strings.Join([]string{strconv.Itoa(turn), strconv.Itoa(index), field}, cursorSeparator)
	}
	frozenUsed := false
	header, err := itemHeader(item.item, item.blobs, func(field itemField) (rpc.ContentReference, error) {
		if item.frozen != nil && field.name == item.frozen.field {
			frozenUsed = true
			return item.frozen.reference(source, contentID(field.name))
		}
		return fieldReference(source, contentID(field.name), field)
	})
	if err != nil || item.frozen == nil || frozenUsed {
		return header, err
	}
	extra, err := item.frozen.reference(source, contentID(item.frozen.field))
	if err != nil {
		return rpc.ItemHeader{}, err
	}
	header.Contents = append(header.Contents, extra)
	header.ItemRevision = itemRevision(header)
	return header, nil
}

func (f *frozenItem) reference(source rpc.ContentSource, contentID string) (rpc.ContentReference, error) {
	if f.content.length < 0 {
		return rpc.ContentReference{}, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return rpc.ContentReference{Source: source, ContentID: contentID, Field: f.field, Encoding: encodingUTF8, Total: rpc.FormatDecimal(uint64(f.content.length)), Digest: f.digest}, nil
}

type contentQuery struct {
	source    rpc.ContentSource
	contentID string
	window    byteRange
}

func (q *contentQuery) member(name string, d *jsontext.Decoder) error {
	if handled, err := q.window.member(name, d); handled {
		return err
	}
	switch name {
	case "source":
		return rpc.ReadMembers(d, stringMembers(map[string]*string{"kind": &q.source.Kind, "historyId": &q.source.HistoryID, "streamId": &q.source.StreamID}))
	case "contentId":
		value, err := rpc.ReadString(d)
		q.contentID = value
		return err
	}
	return d.SkipValue()
}

func (s *server) historyContentRead(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	native := s.native.Load()
	if native == nil {
		return nil, rpc.NativeKind(rpc.ErrorIncompatible)
	}
	var q contentQuery
	if err := decodeParams(params, q.member); err != nil || q.contentID == "" || len(q.contentID) > rpc.IDLimit {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	var chunk rpc.ContentChunk
	var err error
	switch q.source.Kind {
	case sourceSnapshot:
		chunk, err = s.snapshotContent(ctx, native, q)
	case sourceLive:
		chunk, err = s.liveContent(ctx, native, q)
	default:
		err = rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	if err != nil {
		return nil, err
	}
	return marshalChunk(chunk)
}

func (s *server) liveContent(ctx context.Context, native *nativeSession, q contentQuery) (rpc.ContentChunk, error) {
	if q.source.StreamID != native.streamID {
		return rpc.ContentChunk{}, rpc.NativeKind(rpc.ErrorExpired)
	}
	ref, err := native.contents.lookup(q.contentID, time.Now())
	if err != nil {
		return rpc.ContentChunk{}, err
	}
	defer ref.releaseContent()
	if checkErr := q.window.check(ref.total); checkErr != nil {
		return rpc.ContentChunk{}, checkErr
	}
	data, err := s.liveRange(ctx, ref, q.window)
	if err != nil {
		s.log.Debug("rpc: live content unavailable", "content", q.contentID, "ordinal", ref.ordinal, "error", err)
		return rpc.ContentChunk{}, rpc.NativeKind(rpc.ErrorExpired)
	}
	return rangeChunk(data, q.window.offset, ref.total, ref.digest)
}

func (s *server) snapshotItem(snap *historySnapshot, parsed parsedContentID) (pageItem, error) {
	_, items := s.turnItems(snap, parsed.turn)
	if parsed.index >= len(items) {
		return pageItem{}, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return items[parsed.index], nil
}

func snapshotItemField(item pageItem, name string) (itemField, error) {
	field, found, err := fieldOfItem(item.item, item.blobs, name)
	if err != nil || !found {
		return itemField{}, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return field, nil
}

func (s *server) snapshotContent(ctx context.Context, native *nativeSession, q contentQuery) (rpc.ContentChunk, error) {
	snap, err := native.histories.acquire(q.source.HistoryID, time.Now())
	if err != nil {
		return rpc.ContentChunk{}, err
	}
	defer native.histories.done(snap)
	parsed, ok := snap.parseContentID(q.contentID)
	if !ok {
		return rpc.ContentChunk{}, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	item, err := s.snapshotItem(snap, parsed)
	if err != nil {
		return rpc.ContentChunk{}, err
	}
	if item.frozen != nil && item.frozen.field == parsed.name {
		return frozenChunk(item.frozen, q.window)
	}
	field, err := snapshotItemField(item, parsed.name)
	if err != nil {
		return rpc.ContentChunk{}, err
	}
	if checkErr := q.window.check(field.total()); checkErr != nil {
		return rpc.ContentChunk{}, checkErr
	}
	data, err := s.fieldRange(ctx, snap.threadID, field, q.window)
	if err != nil {
		s.log.Debug("rpc: snapshot content unavailable", "content", q.contentID, "error", err)
		return rpc.ContentChunk{}, rpc.NativeKind(rpc.ErrorUnavailable)
	}
	return rangeChunk(data, q.window.offset, field.total(), field.digest())
}

func frozenChunk(frozen *frozenItem, window byteRange) (rpc.ContentChunk, error) {
	if err := window.check(frozen.content.length); err != nil {
		return rpc.ContentChunk{}, err
	}
	data, err := frozen.content.read(window.offset, window.limit)
	if err != nil {
		return rpc.ContentChunk{}, internalError(err)
	}
	return rangeChunk(data, window.offset, frozen.content.length, frozen.digest)
}

type parsedContentID struct {
	turn  int
	index int
	name  string
}

func (snap *historySnapshot) parseContentID(raw string) (parsedContentID, bool) {
	parts := strings.SplitN(raw, cursorSeparator, 3)
	if len(parts) != 3 {
		return parsedContentID{}, false
	}
	turn, turnErr := strconv.Atoi(parts[0])
	index, indexErr := strconv.Atoi(parts[1])
	if turnErr != nil || indexErr != nil || turn < 0 || turn >= len(snap.spans) || index < 0 {
		return parsedContentID{}, false
	}
	return parsedContentID{turn: turn, index: index, name: parts[2]}, true
}

func (s *server) historyClose(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	native := s.native.Load()
	if native == nil {
		return nil, rpc.NativeKind(rpc.ErrorIncompatible)
	}
	var id string
	if err := decodeParams(params, stringMembers(map[string]*string{"historyId": &id})); err != nil || id == "" {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	native.histories.close(id)
	o := rpc.NewWriter()
	o.Open()
	o.Bool("closed", true)
	o.Close()
	return o.Bytes()
}
