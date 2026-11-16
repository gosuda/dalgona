package rpc

import (
	"encoding/json/jsontext"
	"log/slog"
	"maps"
	"reflect"
	"slices"
	"strconv"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/dal/model"
)

type turnPage struct {
	Data            []jsontext.Value `json:"data"`
	NextCursor      string           `json:"nextCursor"`
	BackwardsCursor string           `json:"backwardsCursor"`
}

type turnFixture struct {
	ids  []string
	seqs []uint64
	full []jsontext.Value
}

func listTurns(t *testing.T, c *wireClient, params map[string]any) turnPage {
	t.Helper()
	var page turnPage
	c.ok("thread/turns/list", schemaParams(t, "v2/ThreadTurnsListParams.json", params), &page)
	return page
}

func pageThrough(t *testing.T, c *wireClient, threadID, direction string, limit int) []turnPage {
	t.Helper()
	var pages []turnPage
	cursor := ""
	for range 10 {
		params := map[string]any{"threadId": threadID, "sortDirection": direction, "limit": limit}
		if cursor != "" {
			params["cursor"] = cursor
		}
		page := listTurns(t, c, params)
		pages = append(pages, page)
		if page.NextCursor == "" {
			return pages
		}
		cursor = page.NextCursor
	}
	t.Fatalf("%s paging did not end within 10 pages", direction)
	return nil
}

func scriptedTurns(t *testing.T, r *rig, c *wireClient, threadID string, count int) []string {
	t.Helper()
	ids := make([]string, 0, count)
	for i := range count {
		if i%2 == 1 {
			r.llm.push(llmReply{body: sseCall("call-"+strconv.Itoa(i), "exec", `{"command":"echo hi"}`)})
		}
		r.llm.push(llmReply{body: sseText("ok")})
		ids = append(ids, startTurn(t, c, threadID, "prompt "+strconv.Itoa(i)))
		c.collect()
	}
	return ids
}

func modelRequests(r *rig) int {
	r.llm.mu.Lock()
	defer r.llm.mu.Unlock()
	return r.llm.count
}

func turnStartSeqs(t *testing.T, r *rig, threadID string) []uint64 {
	t.Helper()
	thread, err := r.host.Resume(t.Context(), threadID)
	if err != nil {
		t.Fatal(err)
	}
	records, _, err := thread.Records()
	if err != nil {
		t.Fatal(err)
	}
	var seqs []uint64
	for _, item := range event.ActivePath(records) {
		if _, ok := item.Event.(event.TurnStarted); ok {
			seqs = append(seqs, item.Seq)
		}
	}
	return seqs
}

func fullTurns(t *testing.T, c *wireClient, threadID string) []jsontext.Value {
	t.Helper()
	var read struct {
		Thread struct {
			Turns []jsontext.Value `json:"turns"`
		} `json:"thread"`
	}
	c.ok("thread/read", map[string]any{"threadId": threadID, "includeTurns": true}, &read)
	return read.Thread.Turns
}

func newTurnFixture(t *testing.T, r *rig, c *wireClient, threadID string, count int) turnFixture {
	t.Helper()
	fix := turnFixture{ids: scriptedTurns(t, r, c, threadID, count)}
	fix.seqs = turnStartSeqs(t, r, threadID)
	fix.full = fullTurns(t, c, threadID)
	if len(fix.full) != count || len(fix.seqs) != count {
		t.Fatalf("thread has %d turns and %d turn.started records, want %d each", len(fix.full), len(fix.seqs), count)
	}
	return fix
}

func sameJSON(t *testing.T, a, b jsontext.Value) bool {
	t.Helper()
	var left, right any
	mustUnmarshal(t, a, &left)
	mustUnmarshal(t, b, &right)
	return reflect.DeepEqual(left, right)
}

func cursorOf(prefix string, seq uint64) string { return prefix + strconv.FormatUint(seq, 10) }

func checkTurnsOf(t *testing.T, page turnPage, indexes []int, fix turnFixture) {
	t.Helper()
	if len(page.Data) != len(indexes) {
		t.Fatalf("page holds %d turns, want %d", len(page.Data), len(indexes))
	}
	for j, index := range indexes {
		if id := fieldOf(t, page.Data[j], "id"); id != fix.ids[index] {
			t.Fatalf("turn %d of the page is %s, want %s", j, id, fix.ids[index])
		}
		if !sameJSON(t, page.Data[j], fix.full[index]) {
			t.Fatalf("turn %d differs from thread/read: %s against %s", j, page.Data[j], fix.full[index])
		}
	}
}

func checkCursors(t *testing.T, page turnPage, indexes []int, more bool, fix turnFixture) {
	t.Helper()
	wantBack := cursorOf("b", fix.seqs[indexes[0]])
	wantNext := ""
	if more {
		wantNext = cursorOf("a", fix.seqs[indexes[len(indexes)-1]])
	}
	if page.BackwardsCursor != wantBack || page.NextCursor != wantNext {
		t.Fatalf("cursors = next %q backwards %q, want next %q backwards %q", page.NextCursor, page.BackwardsCursor, wantNext, wantBack)
	}
}

func checkPages(t *testing.T, got []turnPage, want [][]int, fix turnFixture) {
	t.Helper()
	if len(got) != len(want) {
		t.Fatalf("got %d pages, want %d", len(got), len(want))
	}
	for k, indexes := range want {
		checkTurnsOf(t, got[k], indexes, fix)
		checkCursors(t, got[k], indexes, k < len(want)-1, fix)
	}
}

func TestTurnsListPagesEqualFullRead(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	fix := newTurnFixture(t, r, c, threadID, 7)
	t.Run("asc", func(t *testing.T) {
		pages := pageThrough(t, c, threadID, "asc", 3)
		checkPages(t, pages, [][]int{{0, 1, 2}, {3, 4, 5}, {6}}, fix)
	})
	t.Run("desc", func(t *testing.T) {
		pages := pageThrough(t, c, threadID, "desc", 3)
		checkPages(t, pages, [][]int{{6, 5, 4}, {3, 2, 1}, {0}}, fix)
	})
	t.Run("backwards cursor includes its anchor in the other direction", func(t *testing.T) {
		desc := pageThrough(t, c, threadID, "desc", 3)
		newest := listTurns(t, c, map[string]any{"threadId": threadID, "sortDirection": "asc", "limit": 3, "cursor": desc[0].BackwardsCursor})
		checkPages(t, []turnPage{newest}, [][]int{{6}}, fix)
		middle := listTurns(t, c, map[string]any{"threadId": threadID, "sortDirection": "asc", "limit": 2, "cursor": desc[1].BackwardsCursor})
		checkTurnsOf(t, middle, []int{3, 4}, fix)
		checkCursors(t, middle, []int{3, 4}, true, fix)
	})
}

func requireFullViews(t *testing.T, c *wireClient, threadID string, fix turnFixture) {
	t.Helper()
	views := []map[string]any{{}, {"itemsView": "full"}, {"itemsView": "summary"}}
	for _, view := range views {
		params := map[string]any{"threadId": threadID, "sortDirection": "asc"}
		maps.Copy(params, view)
		page := listTurns(t, c, params)
		if len(page.Data) != len(fix.full) {
			t.Fatalf("view %v returned %d turns, want %d", view, len(page.Data), len(fix.full))
		}
		for i, turn := range page.Data {
			if !sameJSON(t, turn, fix.full[i]) {
				t.Fatalf("view %v turn %d = %s, want the full turn %s", view, i, turn, fix.full[i])
			}
		}
	}
}

func requireNotLoaded(t *testing.T, c *wireClient, threadID string, fix turnFixture) {
	t.Helper()
	bare := listTurns(t, c, map[string]any{"threadId": threadID, "sortDirection": "asc", "itemsView": "notLoaded"})
	if len(bare.Data) != len(fix.ids) {
		t.Fatalf("notLoaded returned %d turns, want %d", len(bare.Data), len(fix.ids))
	}
	for i, turn := range bare.Data {
		var decoded, control Turn
		mustUnmarshal(t, turn, &decoded)
		mustUnmarshal(t, fix.full[i], &control)
		if decoded.ID != fix.ids[i] || decoded.Status != statusCompleted || decoded.Items == nil || len(decoded.Items) != 0 {
			t.Fatalf("notLoaded turn %d = %s, want id %s, status completed and items []", i, turn, fix.ids[i])
		}
		if len(control.Items) == 0 {
			t.Fatalf("full turn %d holds no items, so notLoaded proves nothing", i)
		}
	}
}

func TestTurnsListItemsViews(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	fix := newTurnFixture(t, r, c, threadID, 3)
	requireFullViews(t, c, threadID, fix)
	requireNotLoaded(t, c, threadID, fix)
}

func TestTurnsListShowsInProgressTurn(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	gate := make(chan struct{})
	r.llm.push(llmReply{body: sseText("slow"), gate: gate})
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	turnID := startTurn(t, c, threadID, "wait")
	c.await("turn/started", nil)
	running := listTurns(t, c, map[string]any{"threadId": threadID})
	if len(running.Data) != 1 || fieldOf(t, running.Data[0], "id") != turnID || fieldOf(t, running.Data[0], "status") != statusInProgress {
		t.Fatalf("running thread listed %v, want turn %s inProgress", running.Data, turnID)
	}
	close(gate)
	c.collect()
	done := listTurns(t, c, map[string]any{"threadId": threadID})
	if len(done.Data) != 1 || fieldOf(t, done.Data[0], "status") != statusCompleted {
		t.Fatalf("finished thread listed %v, want turn %s completed", done.Data, turnID)
	}
}

func TestTurnsListOfEmptyThread(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	m := c.call("thread/turns/list", map[string]any{"threadId": threadID})
	if m.Error != nil {
		t.Fatalf("thread/turns/list failed: %+v", m.Error)
	}
	var members map[string]jsontext.Value
	mustUnmarshal(t, m.Result, &members)
	if string(members["data"]) != "[]" || len(members) != 1 {
		t.Fatalf("empty thread result %s, want only data []", m.Result)
	}
}

func TestTurnsListRejectsBadParams(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	scriptedTurns(t, r, c, threadID, 2)
	valid := listTurns(t, c, map[string]any{"threadId": threadID, "limit": 1})
	if len(valid.Data) != 1 {
		t.Fatalf("control call returned %d turns, want 1", len(valid.Data))
	}
	bad := []struct {
		name   string
		params map[string]any
		want   string
	}{
		{"negative limit", map[string]any{"limit": -1}, "invalid limit -1"},
		{"limit over uint64", map[string]any{"limit": jsontext.Value("99999999999999999999")}, "limit: expected an integer"},
		{"fractional limit", map[string]any{"limit": jsontext.Value("2.5")}, "limit: expected an integer"},
		{"sort direction", map[string]any{"sortDirection": "up"}, "unsupported sortDirection up"},
		{"items view", map[string]any{"itemsView": "detailed"}, "unsupported itemsView detailed"},
		{"unknown prefix", map[string]any{"cursor": "x1"}, "invalid cursor x1"},
		{"prefix only", map[string]any{"cursor": "a"}, "invalid cursor a"},
		{"upper case prefix", map[string]any{"cursor": "A1"}, "invalid cursor A1"},
		{"negative sequence", map[string]any{"cursor": "a-1"}, "invalid cursor a-1"},
		{"signed sequence", map[string]any{"cursor": "a+1"}, "invalid cursor a+1"},
		{"leading zero", map[string]any{"cursor": "a01"}, "invalid cursor a01"},
		{"fractional sequence", map[string]any{"cursor": "a1.5"}, "invalid cursor a1.5"},
		{"sequence over uint64", map[string]any{"cursor": "a18446744073709551616"}, "invalid cursor a18446744073709551616"},
		{"sequence not a turn start", map[string]any{"cursor": "a99999"}, "cursor a99999 is not on the active path"},
		{"sequence zero", map[string]any{"cursor": "b0"}, "cursor b0 is not on the active path"},
	}
	for _, tc := range bad {
		t.Run(tc.name, func(t *testing.T) {
			params := map[string]any{"threadId": threadID}
			maps.Copy(params, tc.params)
			c.fails("thread/turns/list", params, codeInvalidParams, tc.want)
		})
	}
	c.fails("thread/turns/list", map[string]any{}, codeInvalidParams, "threadId is required")
	c.fails("thread/turns/list", map[string]any{"threadId": "nope"}, codeInvalidParams, "unknown thread nope")
}

func foreignTurnSeq(t *testing.T, r *rig, c *wireClient, own []uint64) uint64 {
	t.Helper()
	other := startThread(t, r, c, nil)
	r.llm.push(llmReply{body: sseCall("call-first", "exec", `{"command":"echo hi"}`)}, llmReply{body: sseText("ok")})
	startTurn(t, c, other, "tool first")
	c.collect()
	scriptedTurns(t, r, c, other, 2)
	foreign := uint64(0)
	for _, seq := range turnStartSeqs(t, r, other) {
		if !slices.Contains(own, seq) {
			foreign = seq
		}
	}
	if foreign == 0 {
		t.Fatalf("every turn.started sequence of the second thread is also on the first: %v", own)
	}
	return foreign
}

func TestTurnsListRejectsCursorOffActivePath(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	ids := scriptedTurns(t, r, c, threadID, 4)
	seqs := turnStartSeqs(t, r, threadID)
	foreign := foreignTurnSeq(t, r, c, seqs)
	c.fails("thread/turns/list", map[string]any{"threadId": threadID, "cursor": cursorOf("a", foreign)}, codeInvalidParams, "cursor "+cursorOf("a", foreign)+" is not on the active path")
	own := listTurns(t, c, map[string]any{"threadId": threadID, "cursor": cursorOf("a", seqs[3]), "sortDirection": "asc"})
	if len(own.Data) != 0 || own.NextCursor != "" {
		t.Fatalf("cursor after the last turn returned %d turns and nextCursor %q, want none", len(own.Data), own.NextCursor)
	}
	c.ok("thread/revert", map[string]any{"threadId": threadID, "beforeTurnId": ids[2]}, nil)
	c.await("thread/reverted", nil)
	c.fails("thread/turns/list", map[string]any{"threadId": threadID, "cursor": cursorOf("a", seqs[2])}, codeInvalidParams, "cursor "+cursorOf("a", seqs[2])+" is not on the active path")
	kept := listTurns(t, c, map[string]any{"threadId": threadID, "cursor": cursorOf("b", seqs[1]), "sortDirection": "asc"})
	if len(kept.Data) != 1 || fieldOf(t, kept.Data[0], "id") != ids[1] {
		t.Fatalf("cursor on a kept turn returned %v, want only turn %s", kept.Data, ids[1])
	}
}

func TestTurnPageQueryDefaultsAndCaps(t *testing.T) {
	cases := []struct {
		limit int
		want  int
	}{{0, 20}, {1, 1}, {7, 7}, {100, 100}, {101, 100}, {1 << 30, 100}}
	for _, tc := range cases {
		query, err := newTurnPageQuery(ThreadTurnsListParams{ThreadID: "t", Limit: tc.limit})
		if err != nil {
			t.Fatal(err)
		}
		if query.limit != tc.want || query.ascending || query.notLoaded || query.cursor != nil {
			t.Fatalf("limit %d gives %+v, want limit %d with the descending, full, uncursored defaults", tc.limit, query, tc.want)
		}
	}
	if _, err := newTurnPageQuery(ThreadTurnsListParams{Limit: -1}); err == nil {
		t.Fatal("limit -1 was accepted")
	}
}

type windowCase struct {
	name      string
	direction string
	cursor    string
	spans     []turnSpan
	low, high int
	err       string
}

func TestTurnWindowBoundaries(t *testing.T) {
	spans := []turnSpan{{seq: 10}, {seq: 20}, {seq: 30}, {seq: 40}, {seq: 50}}
	cases := []windowCase{
		{"desc from the newest", "desc", "", spans, 3, 5, ""},
		{"desc after the middle", "desc", "a30", spans, 0, 2, ""},
		{"desc from the middle", "desc", "b30", spans, 1, 3, ""},
		{"desc after the oldest", "desc", "a10", spans, 0, 0, ""},
		{"desc from the oldest", "desc", "b10", spans, 0, 1, ""},
		{"asc from the oldest", "asc", "", spans, 0, 2, ""},
		{"asc after the middle", "asc", "a30", spans, 3, 5, ""},
		{"asc after the one before last", "asc", "a40", spans, 4, 5, ""},
		{"asc after the newest", "asc", "a50", spans, 5, 5, ""},
		{"asc from the newest", "asc", "b50", spans, 4, 5, ""},
		{"empty desc", "desc", "", nil, 0, 0, ""},
		{"empty asc", "asc", "", nil, 0, 0, ""},
		{"between two turns", "desc", "a25", spans, 0, 0, "cursor a25 is not on the active path"},
		{"empty with a cursor", "asc", "b10", nil, 0, 0, "cursor b10 is not on the active path"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) { checkWindowCase(t, tc) })
	}
}

func checkWindowCase(t *testing.T, tc windowCase) {
	t.Helper()
	query, err := newTurnPageQuery(ThreadTurnsListParams{ThreadID: "t", Cursor: tc.cursor, Limit: 2, SortDirection: tc.direction})
	if err != nil {
		t.Fatal(err)
	}
	low, high, err := query.window(tc.spans)
	if tc.err != "" {
		if err == nil || err.Error() != tc.err {
			t.Fatalf("window error = %v, want %q", err, tc.err)
		}
		return
	}
	if err != nil || low != tc.low || high != tc.high {
		t.Fatalf("window = [%d,%d) with error %v, want [%d,%d)", low, high, err, tc.low, tc.high)
	}
}

func requireExcluded(t *testing.T, resumed ThreadResumeResponse, requests int) {
	t.Helper()
	if resumed.Thread.Turns == nil || len(resumed.Thread.Turns) != 0 {
		t.Fatalf("excludeTurns resume returned turns %+v, want []", resumed.Thread.Turns)
	}
	if resumed.TokenUsage == nil {
		t.Fatal("excludeTurns resume has no tokenUsage")
	}
	total := resumed.TokenUsage.Total
	if total.InputTokens != 20*requests || total.OutputTokens != 7*requests || total.TotalTokens != 27*requests {
		t.Fatalf("excludeTurns usage = %+v, want %d model requests of 20 input and 7 output tokens", total, requests)
	}
	requireCost(t, "excludeTurns resume", *resumed.TokenUsage, int64(requests)*165, false)
}

func TestResumeExcludeTurnsKeepsUsage(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	r.cfg.Config.Models = map[string]config.ModelSpec{"openai/gpt-test": {Price: &model.Price{
		InputMicros: 3_000_000, OutputMicros: 15_000_000, CacheReadMicros: 300_000, CacheWriteMicros: 3_750_000,
	}}}
	reopen(t, r)
	first := r.connect()
	threadID := startThread(t, r, first, nil)
	ids := scriptedTurns(t, r, first, threadID, 3)
	requests := modelRequests(r)
	params := map[string]any{"threadId": threadID, "excludeTurns": true}

	second := r.connect()
	for range 2 {
		var resumed ThreadResumeResponse
		second.ok("thread/resume", schemaParams(t, "v2/ThreadResumeParams.json", params), &resumed)
		requireExcluded(t, resumed, requests)
	}

	third := r.connect()
	var whole ThreadResumeResponse
	third.ok("thread/resume", map[string]any{"threadId": threadID}, &whole)
	if len(whole.Thread.Turns) != len(ids) || whole.TokenUsage == nil || whole.TokenUsage.Total.TotalTokens != 27*requests {
		t.Fatalf("resume without excludeTurns = %d turns and usage %+v, want %d turns and total %d", len(whole.Thread.Turns), whole.TokenUsage, len(ids), 27*requests)
	}

	r.llm.push(llmReply{body: sseText("more")})
	startTurn(t, second, threadID, "again")
	var live ThreadTokenUsageUpdatedNotification
	mustUnmarshal(t, second.await("thread/tokenUsage/updated", nil).Params, &live)
	if live.TokenUsage.Total.TotalTokens != 27*(requests+1) {
		t.Fatalf("live total after excludeTurns resume = %d, want %d", live.TokenUsage.Total.TotalTokens, 27*(requests+1))
	}
	requireCost(t, "live after excludeTurns resume", live.TokenUsage, int64(requests+1)*165, false)
	second.await("turn/completed", nil)
}

type blobLoader struct{ loaded []string }

func (l *blobLoader) load(ref *event.BlobRef) ([]byte, error) {
	l.loaded = append(l.loaded, ref.Blob)
	return []byte(ref.Blob), nil
}

func blobTracker(l *blobLoader) *tracker {
	noKeywords := func(string, string, agent.InputKind) []agent.KeywordMatch { return nil }
	return newTracker("t1", l.load, nil, noKeywords, slog.New(slog.DiscardHandler))
}

var blobEpoch = time.Unix(1_700_000_000, 0)

func appendRecord(records []event.Seqd, ev event.Event) []event.Seqd {
	return append(records, event.Seqd{Seq: uint64(len(records)) + 1, Event: ev})
}

func blobRef(name string) *event.BlobRef { return &event.BlobRef{Blob: name, Bytes: int64(len(name))} }

func appendOpenTurn(records []event.Seqd) []event.Seqd {
	records = appendRecord(records, event.TurnStarted{TurnID: "turn-open", StartedAt: blobEpoch})
	return appendRecord(records, event.UserMessage{ItemID: "u-open", TurnID: "turn-open", TextBlob: blobRef("text-open"), Kind: "prompt"})
}

func appendBlobTurn(records []event.Seqd, index int) ([]event.Seqd, []string) {
	turn := "turn-" + strconv.Itoa(index)
	text, post := "text-"+strconv.Itoa(index), "post-"+strconv.Itoa(index)
	files := []event.FileChange{{Path: "f.go", PostSHA: "sha", PostBlob: blobRef(post)}}
	for _, ev := range []event.Event{
		event.TurnStarted{TurnID: turn, StartedAt: blobEpoch},
		event.UserMessage{ItemID: "u-" + turn, TurnID: turn, TextBlob: blobRef(text), Kind: "prompt"},
		event.ToolStarted{CallID: "c-" + turn, TurnID: turn, Tool: "patch", Class: "edit", Args: jsontext.Value("{}"), Files: files},
		event.ToolCompleted{CallID: "c-" + turn, OK: true},
		event.AssistantMessage{ItemID: "a-" + turn, TurnID: turn, Text: "done", StopReason: "stop", Usage: event.Usage{Input: 10, Output: 5}, ContextWindow: 8000},
		event.TurnCompleted{TurnID: turn, Status: "completed", CompletedAt: blobEpoch.Add(time.Second)},
	} {
		records = appendRecord(records, ev)
	}
	return records, []string{text, post}
}

func blobRecords(closed int) ([]event.Seqd, [][]string) {
	records := appendRecord(nil, event.SessionCreated{ID: "t1", Cwd: "/work", Model: "openai/m"})
	digests := make([][]string, 0, closed)
	for i := range closed {
		var turn []string
		records, turn = appendBlobTurn(records, i)
		digests = append(digests, turn)
	}
	return records, digests
}

func TestTurnsListLoadsOnlyPageBlobs(t *testing.T) {
	const closed, pageSize = 200, 5
	records, digests := blobRecords(closed)
	paged := &blobLoader{}
	got, err := turnsPage(records, blobTracker(paged), turnPageQuery{limit: pageSize})
	if err != nil {
		t.Fatal(err)
	}
	if len(got.Data) != pageSize || got.Data[0].ID != "turn-199" || got.Data[pageSize-1].ID != "turn-195" || len(got.Data[0].Items) != 3 {
		t.Fatalf("page = %d turns, first %q with %d items, want %d turns from turn-199", len(got.Data), got.Data[0].ID, len(got.Data[0].Items), pageSize)
	}
	want := slices.Concat(digests[closed-pageSize:]...)
	slices.Sort(want)
	slices.Sort(paged.loaded)
	if !slices.Equal(paged.loaded, want) {
		t.Fatalf("page loaded blobs %v, want exactly the blobs of its turns %v", paged.loaded, want)
	}
	whole := &blobLoader{}
	blobTracker(whole).replayAll(event.ActivePath(records))
	if len(whole.loaded) != closed*2 {
		t.Fatalf("full replay loaded %d blobs, want %d", len(whole.loaded), closed*2)
	}
}

type pageLimitCase struct {
	name  string
	limit int
	count int
	last  string
}

func TestTurnsListAppliesDefaultAndCappedPageLimits(t *testing.T) {
	records, _ := blobRecords(105)
	for _, tc := range []pageLimitCase{
		{name: "default", count: 20, last: "turn-85"},
		{name: "capped", limit: 101, count: 100, last: "turn-5"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			page := firstPageOfLimitCase(t, records, tc)
			checkPageLimitContinuation(t, records, page, tc)
		})
	}
}

func firstPageOfLimitCase(t *testing.T, records []event.Seqd, tc pageLimitCase) ThreadTurnsListResponse {
	t.Helper()
	query, err := newTurnPageQuery(ThreadTurnsListParams{Limit: tc.limit, ItemsView: "notLoaded"})
	if err != nil {
		t.Fatal(err)
	}
	page, err := turnsPage(records, blobTracker(&blobLoader{}), query)
	if err != nil {
		t.Fatal(err)
	}
	if len(page.Data) != tc.count {
		t.Fatalf("page contains %d turns, want %d", len(page.Data), tc.count)
	}
	if page.Data[0].ID != "turn-104" || page.Data[tc.count-1].ID != tc.last || page.NextCursor == "" {
		t.Fatalf("page boundaries = %q through %q, next %q", page.Data[0].ID, page.Data[tc.count-1].ID, page.NextCursor)
	}
	return page
}

func checkPageLimitContinuation(t *testing.T, records []event.Seqd, page ThreadTurnsListResponse, tc pageLimitCase) {
	t.Helper()
	query, err := newTurnPageQuery(ThreadTurnsListParams{Cursor: page.NextCursor, Limit: 100, ItemsView: "notLoaded"})
	if err != nil {
		t.Fatal(err)
	}
	tail, err := turnsPage(records, blobTracker(&blobLoader{}), query)
	if err != nil {
		t.Fatal(err)
	}
	if len(tail.Data) != 105-tc.count || tail.Data[0].ID == tc.last || tail.Data[len(tail.Data)-1].ID != "turn-0" || tail.NextCursor != "" {
		t.Fatalf("continuation = %+v, want the remaining older turns exactly once", tail)
	}
}

func TestTurnsListNotLoadedLoadsNoBlob(t *testing.T) {
	records, _ := blobRecords(20)
	loader := &blobLoader{}
	got, err := turnsPage(records, blobTracker(loader), turnPageQuery{limit: 3, notLoaded: true})
	if err != nil {
		t.Fatal(err)
	}
	if len(got.Data) != 3 || got.Data[0].ID != "turn-19" || got.Data[0].Status != statusCompleted || got.Data[0].Items == nil || len(got.Data[0].Items) != 0 {
		t.Fatalf("notLoaded page = %+v, want 3 completed turns with items []", got.Data)
	}
	if len(loader.loaded) != 0 {
		t.Fatalf("notLoaded page loaded %v, want no blob", loader.loaded)
	}
}

type probeSink struct{ turnIDs []string }

func (s *probeSink) turn(string, Turn) {}
func (s *probeSink) item(_, turnID string, _ *Item) {
	s.turnIDs = append(s.turnIDs, turnID)
}
func (s *probeSink) notify(string, func() writer) {}

func TestReplayLiveLoadsOnlyOpenTurnBlobs(t *testing.T) {
	const closed = 200
	records, _ := blobRecords(closed)
	withOpen := appendOpenTurn(slices.Clone(records))
	loader := &blobLoader{}
	tr := blobTracker(loader)
	if turns := tr.replayLive(event.ActivePath(withOpen)); turns != nil {
		t.Fatalf("replayLive returned %d turns, want none", len(turns))
	}
	if !slices.Equal(loader.loaded, []string{"text-open"}) {
		t.Fatalf("replayLive loaded %v, want only the blob of the open turn", loader.loaded)
	}
	usage := tr.usageSnapshot()
	if usage == nil || usage.Total.TotalTokens != closed*15 || usage.Total.InputTokens != closed*10 || usage.ModelContextWindow != 8000 {
		t.Fatalf("usage = %+v, want %d total tokens and window 8000", usage, closed*15)
	}
	probe := &probeSink{}
	tr.sink = probe
	tr.agentDelta("stream", "hi")
	if !slices.Equal(probe.turnIDs, []string{"turn-open"}) {
		t.Fatalf("live delta after replayLive opened items in %v, want turn-open", probe.turnIDs)
	}
}

func TestReplayLiveOfClosedHistoryLoadsNoBlob(t *testing.T) {
	records, _ := blobRecords(200)
	loader := &blobLoader{}
	tr := blobTracker(loader)
	tr.replayLive(event.ActivePath(records))
	probe := &probeSink{}
	tr.sink = probe
	tr.agentDelta("stream", "hi")
	if len(loader.loaded) != 0 || !slices.Equal(probe.turnIDs, []string{"turn-199"}) {
		t.Fatalf("closed history loaded %v and a delta opened items in %v, want no loads and turn-199", loader.loaded, probe.turnIDs)
	}
}

type completedSink struct{ items []Item }

func (s *completedSink) turn(string, Turn) {}
func (s *completedSink) item(method, _ string, it *Item) {
	if method == methodItemCompleted {
		s.items = append(s.items, *it)
	}
}
func (s *completedSink) notify(string, func() writer) {}

func pricedTracker() *tracker {
	prices := map[string]model.Price{
		"m/one": {InputMicros: 1_000_000, OutputMicros: 2_000_000},
		"m/two": {InputMicros: 2_000_000, OutputMicros: 4_000_000, CacheReadMicros: 500_000},
	}
	tr := blobTracker(&blobLoader{})
	tr.price = func(id string) (model.Price, bool) {
		price, ok := prices[id]
		return price, ok
	}
	return tr
}

func appendUsageTurn(records []event.Seqd, turn, modelID string, usage event.Usage, window int) []event.Seqd {
	for _, ev := range []event.Event{
		event.TurnStarted{TurnID: turn, StartedAt: blobEpoch},
		event.UserMessage{ItemID: "u-" + turn, TurnID: turn, Text: "hi", Kind: "prompt", Model: modelID},
		event.AssistantMessage{ItemID: "a-" + turn, TurnID: turn, Text: "ok", StopReason: "stop", Usage: usage, ContextWindow: window},
		event.TurnCompleted{TurnID: turn, Status: "completed", CompletedAt: blobEpoch.Add(time.Second)},
	} {
		records = appendRecord(records, ev)
	}
	return records
}

func usageRecords() []event.Seqd {
	records := appendRecord(nil, event.SessionCreated{ID: "t1", Cwd: "/work", Model: "m/one"})
	records = appendUsageTurn(records, "t1", "m/one", event.Usage{Input: 10, Output: 3}, 1000)
	return appendUsageTurn(records, "t2", "m/two", event.Usage{Input: 5, Output: 2, CacheRead: 4}, 0)
}

func appendOpenCall(records []event.Seqd) []event.Seqd {
	args := jsontext.Value(`{"command":"ls"}`)
	for _, ev := range []event.Event{
		event.TurnStarted{TurnID: "t3", StartedAt: blobEpoch},
		event.UserMessage{ItemID: "u-t3", TurnID: "t3", Text: "run", Kind: "prompt", Model: "m/two"},
		event.AssistantMessage{
			ItemID: "a-t3", TurnID: "t3", StopReason: "tool_calls", Usage: event.Usage{Input: 7, Output: 1},
			ToolCalls: []event.ToolCall{{ID: "c1", Name: "exec", Args: args}},
		},
		event.ToolStarted{CallID: "c1", TurnID: "t3", Tool: "exec", Class: "exec", Args: args},
	} {
		records = appendRecord(records, ev)
	}
	return records
}

func replayed(tr *tracker, records []event.Seqd, replay replayer) []Turn {
	if created, ok := sessionCreated(records); ok {
		tr.created = created
	}
	return replay(tr, event.ActivePath(records))
}

func requireSameState(t *testing.T, live, all *tracker) {
	t.Helper()
	if live.turn != all.turn || live.lastTurn != all.lastTurn || live.modelID != all.modelID {
		t.Fatalf("replayLive turn %q last %q model %q, want replayAll turn %q last %q model %q",
			live.turn, live.lastTurn, live.modelID, all.turn, all.lastTurn, all.modelID)
	}
	if live.seq != all.seq || live.cost != all.cost || live.created != all.created {
		t.Fatalf("replayLive seq %d cost %+v created %+v, want replayAll seq %d cost %+v created %+v",
			live.seq, live.cost, live.created, all.seq, all.cost, all.created)
	}
}

func TestReplayLiveMatchesReplayAllWhenIdle(t *testing.T) {
	records := usageRecords()
	all, live := pricedTracker(), pricedTracker()
	replayed(all, records, (*tracker).replayAll)
	if turns := replayed(live, records, (*tracker).replayLive); turns != nil {
		t.Fatalf("replayLive returned %d turns, want none", len(turns))
	}
	want := ThreadTokenUsage{
		Last:               TokenUsageBreakdown{InputTokens: 5, CachedInputTokens: 4, OutputTokens: 2, TotalTokens: 7},
		Total:              TokenUsageBreakdown{InputTokens: 15, CachedInputTokens: 4, OutputTokens: 5, TotalTokens: 20},
		ModelContextWindow: 1000,
	}
	micros := int64(16 + 20)
	want.CostUsdMicros = &micros
	got := live.usageSnapshot()
	if !reflect.DeepEqual(got, &want) {
		t.Fatalf("replayLive usage = %+v, want %+v", got, want)
	}
	if !reflect.DeepEqual(got, all.usageSnapshot()) {
		t.Fatalf("replayLive usage %+v differs from replayAll usage %+v", got, all.usageSnapshot())
	}
	if live.lastTurn != "t2" || live.turn != "" || live.modelID != "m/two" {
		t.Fatalf("replayLive turn %q last %q model %q, want turn \"\" last t2 model m/two", live.turn, live.lastTurn, live.modelID)
	}
	requireSameState(t, live, all)
}

func itemFacts(it Item) [6]string {
	return [6]string{it.Type, it.ID, it.Tool, it.Command, it.Status, it.Output}
}

func requireUsage(t *testing.T, usage *ThreadTokenUsage, input, output int, micros int64) {
	t.Helper()
	if usage == nil || usage.Total.InputTokens != input || usage.Total.OutputTokens != output || usage.CostUsdMicros == nil || *usage.CostUsdMicros != micros {
		t.Fatalf("usage = %+v, want %d input and %d output tokens at %d micros", usage, input, output, micros)
	}
}

func TestReplayLiveKeepsOpenTurnState(t *testing.T) {
	records := appendOpenCall(usageRecords())
	path := event.ActivePath(records)
	tail := event.ToolCompleted{CallID: "c1", OK: true, Output: "a b"}
	complete := func(tr *tracker) Item {
		t.Helper()
		sink := &completedSink{}
		tr.sink = sink
		tr.apply(uint64(len(path))+1, tail)
		if len(sink.items) != 1 {
			t.Fatalf("completing c1 emitted %d items, want 1", len(sink.items))
		}
		return sink.items[0]
	}
	all, live, history := pricedTracker(), pricedTracker(), pricedTracker()
	replayed(all, records, (*tracker).replayAll)
	replayed(live, records, (*tracker).replayLive)
	for _, e := range path {
		history.applyHistory(e.Seq, e.Event)
	}
	want := [6]string{kindCommand, "c1", "exec", "ls", statusCompleted, "a b"}
	got := complete(live)
	if itemFacts(got) != want {
		t.Fatalf("item completed after replayLive = %v, want %v", itemFacts(got), want)
	}
	if control := complete(all); !reflect.DeepEqual(got, control) {
		t.Fatalf("item after replayLive %+v differs from item after replayAll %+v", got, control)
	}
	if lost := complete(history); itemFacts(lost) == want {
		t.Fatalf("history-only replay kept the open call (%+v), so the check cannot see a lost open turn", lost)
	}
	requireUsage(t, live.usageSnapshot(), 22, 6, int64(16+20+7*2+1*4))
}

func joinedPages(t *testing.T, records []event.Seqd, direction string, limit int) []Turn {
	t.Helper()
	var joined []Turn
	cursor := ""
	for range 20 {
		query, err := newTurnPageQuery(ThreadTurnsListParams{ThreadID: "t1", Cursor: cursor, Limit: limit, SortDirection: direction})
		if err != nil {
			t.Fatal(err)
		}
		page, err := turnsPage(records, blobTracker(&blobLoader{}), query)
		if err != nil {
			t.Fatal(err)
		}
		if len(page.Data) > limit {
			t.Fatalf("%s page holds %d turns, want at most %d", direction, len(page.Data), limit)
		}
		joined = append(joined, page.Data...)
		if page.NextCursor == "" {
			return joined
		}
		cursor = page.NextCursor
	}
	t.Fatalf("%s paging did not end within 20 pages", direction)
	return nil
}

func interTurnRecords() []event.Seqd {
	records := appendRecord(nil, event.SessionCreated{ID: "t1", Cwd: "/work", Model: "openai/m"})
	records = appendRecord(records, event.ThreadNamed{Name: "named before the first turn"})
	for i := range 4 {
		records, _ = appendBlobTurn(records, i)
		if i%2 == 0 {
			records = appendRecord(records, event.AdvisorNote{Advisor: "a", Severity: "info", Note: "look", Delivery: "queued"})
			continue
		}
		records = appendRecord(records, event.Compaction{FirstKeptSeq: 1, Summary: "s", Method: "auto"})
	}
	return appendOpenTurn(records)
}

func TestTurnsPageEqualsFullReplayAcrossInterTurnEvents(t *testing.T) {
	records := interTurnRecords()
	full := blobTracker(&blobLoader{}).replayAll(event.ActivePath(records))
	if len(full) != 5 || len(full[0].Items) != 4 || len(full[1].Items) != 4 || full[4].Status != statusInProgress {
		t.Fatalf("full replay = %d turns with %d and %d items in the first two, want 5 turns with 4 items each and an open last turn", len(full), len(full[0].Items), len(full[1].Items))
	}
	if got := joinedPages(t, records, "asc", 2); !reflect.DeepEqual(got, full) {
		t.Fatalf("ascending pages differ from the full replay:\n%+v\n%+v", got, full)
	}
	descending := joinedPages(t, records, "desc", 2)
	slices.Reverse(descending)
	if !reflect.DeepEqual(descending, full) {
		t.Fatalf("descending pages differ from the full replay:\n%+v\n%+v", descending, full)
	}
}
