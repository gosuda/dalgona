package rpcserver

import (
	json "encoding/json/v2"
	"fmt"
	"log/slog"
	"slices"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
)

type poisonEvent struct{ touched *atomic.Int64 }

func (p poisonEvent) Type() string {
	p.touched.Add(1)
	return "poison"
}

func (poisonEvent) Validate() error { return nil }

func syntheticJournal(turns int, agentItems func(turn int) int) []event.Seqd {
	var records []event.Seqd
	add := func(e event.Event) { records = append(records, event.Seqd{Seq: uint64(len(records) + 1), Event: e}) }
	add(event.SessionCreated{})
	at := time.Unix(1_700_000_000, 0)
	for i := range turns {
		id := fmt.Sprintf("t%04d", i)
		add(event.TurnStarted{TurnID: id, StartedAt: at})
		add(event.UserMessage{ItemID: id + "-u", TurnID: id, Text: "question " + id})
		for j := range agentItems(i) {
			add(event.AssistantMessage{ItemID: fmt.Sprintf("%s-a%d", id, j), TurnID: id, Text: fmt.Sprintf("answer %s.%d", id, j)})
		}
		add(event.TurnCompleted{TurnID: id, Status: "completed", CompletedAt: at})
	}
	return records
}

func poisonOutside(snap *historySnapshot, low, high int, touched *atomic.Int64) {
	clean := slices.Clone(snap.path)
	for i := range snap.path {
		snap.path[i] = event.Seqd{Seq: clean[i].Seq, Event: poisonEvent{touched: touched}}
	}
	copy(snap.path[snap.spans[low].start:snap.spans[high-1].end], clean[snap.spans[low].start:snap.spans[high-1].end])
}

func pagingServer(t *testing.T) *server {
	t.Helper()
	r := newRig(t, config.ApprovalYolo)
	return &server{host: r.host, log: slog.New(slog.DiscardHandler)}
}

func TestHistoryPagesReadOnlyTheirOwnTurns(t *testing.T) {
	s := pagingServer(t)
	records := syntheticJournal(300, func(int) int { return 2 })
	snap := &historySnapshot{id: "h1", threadID: "thread"}
	snap.indexRecords(records, uint64(len(records)))
	var touched atomic.Int64
	poisonOutside(snap, 280, 300, &touched)
	page, err := s.buildPage(t.Context(), snap, pageQuery{limit: 20, direction: directionOlder}, historyCursor{turn: 299})
	if err != nil {
		t.Fatal(err)
	}
	assertTurnRange(t, page, 299, 280)
	if touched.Load() != 0 {
		t.Fatalf("newest page touched %d records outside its 20 turns", touched.Load())
	}
	snap.indexRecords(records, uint64(len(records)))
	poisonOutside(snap, 260, 280, &touched)
	next, ok := snap.parseCursor(page.NextCursor)
	if !ok {
		t.Fatalf("next cursor %q does not parse", page.NextCursor)
	}
	older, err := s.buildPage(t.Context(), snap, pageQuery{limit: 20, direction: directionOlder}, next)
	if err != nil {
		t.Fatal(err)
	}
	assertTurnRange(t, older, 279, 260)
	if touched.Load() != 0 {
		t.Fatalf("second page touched %d records outside its 20 turns", touched.Load())
	}
}

func assertTurnRange(t *testing.T, page rpc.HistoryPage, from, to int) {
	t.Helper()
	if len(page.Turns) != from-to+1 {
		t.Fatalf("page has %d turns, want %d", len(page.Turns), from-to+1)
	}
	for i, turn := range page.Turns {
		want := fmt.Sprintf("t%04d", from-i)
		if turn.TurnID != want || !turn.ItemsComplete || len(turn.Items) != 3 || turn.Items[0].ID != want+"-u" {
			t.Fatalf("turn %d = %s complete=%v items=%d, want %s with 3 items", i, turn.TurnID, turn.ItemsComplete, len(turn.Items), want)
		}
	}
}

func TestHistoryContinuesInsideAnOversizedTurn(t *testing.T) {
	s := pagingServer(t)
	const items = 5000
	records := syntheticJournal(3, func(turn int) int {
		if turn == 1 {
			return items
		}
		return 1
	})
	snap := &historySnapshot{id: "h1", threadID: "thread"}
	snap.indexRecords(records, uint64(len(records)))
	ids := pageOversizedTurn(t, s, snap)
	checkOversizedItems(t, ids, items)
}

func pageOversizedTurn(t *testing.T, s *server, snap *historySnapshot) []string {
	t.Helper()
	pager := &oversizedPager{s: s, snap: snap, at: historyCursor{turn: 1}}
	for ; ; pager.pages++ {
		if pager.finished(t) {
			break
		}
	}
	return pager.ids
}

type oversizedPager struct {
	s        *server
	snap     *historySnapshot
	ids      []string
	revision string
	at       historyCursor
	pages    int
}

func (p *oversizedPager) finished(t *testing.T) bool {
	t.Helper()
	page := p.read(t)
	turn := page.Turns[0]
	p.revision = turn.TurnRevision
	for _, header := range turn.Items {
		p.ids = append(p.ids, header.ID)
	}
	if turn.ItemsComplete {
		if p.pages == 0 {
			t.Fatal("a turn of 5001 item headers fit in one 1 MiB page")
		}
		return true
	}
	cursor, ok := p.snap.parseCursor(page.NextCursor)
	if !ok || cursor.turn != 1 || cursor.item != turn.NextItemIndex {
		t.Fatalf("continuation cursor %q does not resume at item %d", page.NextCursor, turn.NextItemIndex)
	}
	p.at = cursor
	return false
}

func (p *oversizedPager) read(t *testing.T) rpc.HistoryPage {
	t.Helper()
	page, err := p.s.buildPage(t.Context(), p.snap, pageQuery{limit: 1, direction: directionNewer}, p.at)
	if err != nil {
		t.Fatal(err)
	}
	raw, err := json.Marshal(page)
	if err != nil {
		t.Fatal(err)
	}
	turn := page.Turns[0]
	if len(raw) > historyPageBytes || turn.TurnID != "t0001" || turn.FirstItemIndex != len(p.ids) || len(turn.Items) == 0 || p.revision != "" && turn.TurnRevision != p.revision {
		t.Fatalf("page %d: %d bytes, turn %s, first %d after %d items, revision %q", p.pages, len(raw), turn.TurnID, turn.FirstItemIndex, len(p.ids), turn.TurnRevision)
	}
	return page
}

func checkOversizedItems(t *testing.T, ids []string, items int) {
	t.Helper()
	if len(ids) != items+1 || ids[0] != "t0001-u" || ids[items] != fmt.Sprintf("t0001-a%d", items-1) {
		t.Fatalf("collected %d items from %s to %s", len(ids), ids[0], ids[len(ids)-1])
	}
	for i := 1; i < items; i++ {
		if ids[i] != fmt.Sprintf("t0001-a%d", i-1) {
			t.Fatalf("item %d is %s, want t0001-a%d", i, ids[i], i-1)
		}
	}
}

func TestHistorySnapshotRefusesNinthSlotButReplacesInPlace(t *testing.T) {
	table := newHistoryTable()
	now := time.Now()
	for i := range connectionSnapshots {
		if err := table.publish(&historySnapshot{id: fmt.Sprint("h", i), threadID: "thread", expires: now.Add(time.Minute)}, "", now); err != nil {
			t.Fatal(err)
		}
	}
	ninth := checkNinthRefused(t, table, now)
	checkInPlaceReplacement(t, table, now, ninth)
}

func checkNinthRefused(t *testing.T, table *historyTable, now time.Time) *historySnapshot {
	t.Helper()
	ninth := &historySnapshot{id: "h9", threadID: "thread", expires: now.Add(time.Minute)}
	if data, ok := rpc.ErrorDataOf(table.publish(ninth, "", now)); !ok || data.Kind != rpc.ErrorLimitExceeded {
		t.Fatalf("ninth snapshot error kind = %v", data.Kind)
	}
	return ninth
}

func checkInPlaceReplacement(t *testing.T, table *historyTable, now time.Time, ninth *historySnapshot) {
	t.Helper()
	if err := table.publish(ninth, "h3", now); err != nil {
		t.Fatalf("in-place replacement failed: %v", err)
	}
	if _, err := table.acquire("h3", now); err == nil {
		t.Fatal("replaced snapshot handle still resolves")
	}
	if _, err := table.acquire("h9", now); err != nil {
		t.Fatalf("replacement handle does not resolve: %v", err)
	}
	if data, ok := rpc.ErrorDataOf(table.publish(&historySnapshot{id: "h10", threadID: "other"}, "h9", now)); !ok || data.Kind != rpc.ErrorExpired {
		t.Fatal("a snapshot of another thread replaced h9")
	}
	table.closeAll()
}
