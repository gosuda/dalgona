package state

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func headers(ids ...string) []rpc.ItemHeader {
	out := make([]rpc.ItemHeader, 0, len(ids))
	for _, id := range ids {
		out = append(out, rpc.ItemHeader{ID: id, Type: rpc.KindAgent})
	}
	return out
}

func historyTurn(id, revision string, first int, complete bool, items ...string) rpc.HistoryTurn {
	return rpc.HistoryTurn{TurnID: id, TurnRevision: revision, Status: "completed", FirstItemIndex: first, NextItemIndex: first + len(items), ItemsComplete: complete, Items: headers(items...)}
}

func partialProjection(t *testing.T) *projection {
	t.Helper()
	p := newProjection(rpc.HistorySnapshot{HistoryID: "h", PathRevision: "path", StreamID: "s"}, 0)
	if err := p.mergePage(rpc.HistoryPage{PathRevision: "path", NextCursor: "more", Turns: []rpc.HistoryTurn{historyTurn("b", "rb", 0, false, "b0", "b1")}}); err != nil {
		t.Fatal(err)
	}
	return p
}

func TestHistoryPagesRejectGapsRepeatsAndRevisionChanges(t *testing.T) {
	cases := []struct {
		name string
		page rpc.HistoryPage
		want error
	}{
		{"another turn before the partial one finishes", rpc.HistoryPage{PathRevision: "path", Turns: []rpc.HistoryTurn{historyTurn("a", "ra", 0, true, "a0")}}, errPageGap},
		{"skipped item", rpc.HistoryPage{PathRevision: "path", Turns: []rpc.HistoryTurn{historyTurn("b", "rb", 3, true, "b3")}}, errPageGap},
		{"repeated item", rpc.HistoryPage{PathRevision: "path", Turns: []rpc.HistoryTurn{historyTurn("b", "rb", 2, true, "b1")}}, errPageConflict},
		{"turn revision change", rpc.HistoryPage{PathRevision: "path", Turns: []rpc.HistoryTurn{historyTurn("b", "rb2", 2, true, "b2")}}, errPageRevision},
		{"path revision change", rpc.HistoryPage{PathRevision: "other", Turns: []rpc.HistoryTurn{historyTurn("b", "rb", 2, true, "b2")}}, errPageRevision},
		{"inconsistent item range", rpc.HistoryPage{PathRevision: "path", Turns: []rpc.HistoryTurn{{TurnID: "b", TurnRevision: "rb", FirstItemIndex: 2, NextItemIndex: 4, ItemsComplete: true, Items: headers("b2")}}}, errPageGap},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if err := partialProjection(t).mergePage(c.page); !errors.Is(err, c.want) {
				t.Fatalf("merge returned %v, want %v", err, c.want)
			}
		})
	}
}

func itemIDs(t *turnState) []string {
	out := make([]string, 0, len(t.items))
	for _, it := range t.items {
		out = append(out, it.id)
	}
	return out
}

func TestHistoryPagesContinueATurnAndPrependOlderTurns(t *testing.T) {
	p := partialProjection(t)
	page := rpc.HistoryPage{PathRevision: "path", Turns: []rpc.HistoryTurn{historyTurn("b", "rb", 2, true, "b2"), historyTurn("a", "ra", 0, true, "a0")}}
	if err := p.mergePage(page); err != nil {
		t.Fatal(err)
	}
	if len(p.turns) != 2 || p.turns[0].id != "a" || p.turns[1].id != "b" || !p.complete {
		t.Fatalf("turns %v complete %v, want a then b and complete history", p.turns, p.complete)
	}
	if got := itemIDs(p.turns[1]); !slices.Equal(got, []string{"b0", "b1", "b2"}) {
		t.Fatalf("turn b items %v", got)
	}
}

func TestLiveItemsOfTheActiveTurnStayAfterItsSnapshotItems(t *testing.T) {
	p := newProjection(rpc.HistorySnapshot{HistoryID: "h", PathRevision: "path", StreamID: "s", ActiveTurnID: "b"}, 4)
	live := []envelope{
		{first: 5, last: 5, method: "item/agentMessage/delta", params: mustJSON(t, deltaNote{TurnID: "b", ItemID: "b1", Delta: "tail"})},
		{first: 6, last: 6, method: "item/started", params: mustJSON(t, itemNote{TurnID: "b", Item: rpc.ItemHeader{ID: "b2", Type: rpc.KindAgent}})},
	}
	for _, e := range live {
		if err := p.apply(e); err != nil {
			t.Fatal(err)
		}
	}
	if err := p.mergePage(rpc.HistoryPage{PathRevision: "path", Turns: []rpc.HistoryTurn{historyTurn("b", "rb", 0, true, "b0", "b1")}}); err != nil {
		t.Fatal(err)
	}
	turn := p.turnByID["b"]
	if got := itemIDs(turn); !slices.Equal(got, []string{"b0", "b1", "b2"}) || string(turn.byID["b1"].tail) != "tail" || turn.byID["b1"].base == nil {
		t.Fatalf("active turn items %v with b1 tail %q", got, turn.byID["b1"].tail)
	}
	conflict := newProjection(rpc.HistorySnapshot{PathRevision: "path"}, 0)
	if err := conflict.apply(live[1]); err != nil {
		t.Fatal(err)
	}
	if err := conflict.mergePage(rpc.HistoryPage{PathRevision: "path", Turns: []rpc.HistoryTurn{historyTurn("b", "rb", 0, true, "b0", "b2")}}); !errors.Is(err, errPageConflict) {
		t.Fatalf("a page placing a live-started item after its own items returned %v", err)
	}
}

func chunked(data []byte, size uint64, corrupt func(*rpc.ContentChunk)) fetchChunk {
	sum := sha256.Sum256(data)
	length := uint64(len(data))
	total := rpc.FormatDecimal(length)
	return func(_ context.Context, offset string) (rpc.ContentChunk, error) {
		at, err := rpc.ParseDecimal(offset)
		if err != nil {
			return rpc.ContentChunk{}, err
		}
		end := min(at+size, length)
		chunk := rpc.ContentChunk{DataBase64: base64.StdEncoding.EncodeToString(data[at:end]), Offset: offset, Next: rpc.FormatDecimal(end), Total: total, Digest: hex.EncodeToString(sum[:])}
		corrupt(&chunk)
		return chunk, nil
	}
}

func summaryOf(data []byte) rpc.ContentSummary {
	sum := sha256.Sum256(data)
	return rpc.ContentSummary{Total: rpc.FormatDecimal(uint64(len(data))), Digest: hex.EncodeToString(sum[:])}
}

func TestReadAllReassemblesChunksAndRefusesBrokenOnes(t *testing.T) {
	data := []byte("가나다 split across chunks 😀")
	got, err := checkContent(summaryOf(data), nil, nil)
	if err == nil {
		t.Fatalf("an empty read matched a nonempty summary: %q", got)
	}
	got, err = readAll(t.Context(), chunked(data, 5, func(*rpc.ContentChunk) {}), summaryOf(data))
	if got, err = checkContent(summaryOf(data), got, err); err != nil || string(got) != string(data) {
		t.Fatalf("read %q, %v", got, err)
	}
	broken := map[string]func(*rpc.ContentChunk){
		"wrong offset":  func(c *rpc.ContentChunk) { c.Offset = "1" },
		"wrong next":    func(c *rpc.ContentChunk) { c.Next = c.Total },
		"no progress":   func(c *rpc.ContentChunk) { c.DataBase64, c.Next = "", c.Offset },
		"wrong digest":  func(c *rpc.ContentChunk) { c.Digest = "00" },
		"invalid bytes": func(c *rpc.ContentChunk) { c.DataBase64 = "!" },
	}
	for name, corrupt := range broken {
		if _, err := readAll(t.Context(), chunked(data, 5, corrupt), summaryOf(data)); !errors.Is(err, errContentMismatch) {
			t.Fatalf("%s returned %v", name, err)
		}
	}
	tampered := append([]byte(nil), data...)
	tampered[0] ^= 1
	if _, err := checkContent(summaryOf(data), tampered, nil); !errors.Is(err, errContentMismatch) {
		t.Fatalf("tampered bytes passed the digest check: %v", err)
	}
	if _, err := readAll(t.Context(), chunked(data, 5, func(*rpc.ContentChunk) {}), rpc.ContentSummary{Total: rpc.FormatDecimal(readLimit + 1)}); err == nil {
		t.Fatal("a read beyond the detail limit started")
	}
}
