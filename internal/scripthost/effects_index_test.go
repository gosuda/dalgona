package scripthost

import (
	"fmt"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func indexTestRecords(filler int) []event.Seqd {
	records := []event.Seqd{{Seq: 1, Event: event.ScriptStarted{RunID: "r"}}}
	seq := uint64(2)
	for i := range filler {
		records = append(records, event.Seqd{Seq: seq, Event: event.ToolStarted{CallID: fmt.Sprintf("other/%d", i)}})
		seq++
	}
	records = append(records,
		event.Seqd{Seq: seq, Event: event.ScriptEffect{RunID: "r", N: 1, Op: "time"}},
		event.Seqd{Seq: seq + 1, Event: event.ToolStarted{CallID: "r/2", Tool: "read"}},
		event.Seqd{Seq: seq + 2, Event: event.ToolCompleted{CallID: "r/2", OK: true, Output: "data"}},
	)
	return records
}

func TestEffectCacheMatchesFullScan(t *testing.T) {
	records := indexTestRecords(500)
	cached := newEffectCache()
	for _, n := range []int{1, 2, 3} {
		want := traceEffect(records, "r", n)
		got := cached.trace(records, "r", n)
		if (want.effect == nil) != (got.effect == nil) || (want.started == nil) != (got.started == nil) || (want.completed == nil) != (got.completed == nil) {
			t.Fatalf("n=%d mismatch: want %+v got %+v", n, want, got)
		}
	}
	if _, ok := cached.start(records, "r"); !ok {
		t.Fatalf("cached start missed run r")
	}
	if _, ok := cached.start(records, "missing"); ok {
		t.Fatalf("cached start hit missing run")
	}
}

func TestEffectCacheIndexesOnlyNewSuffix(t *testing.T) {
	records := indexTestRecords(100)
	cached := newEffectCache()
	cached.trace(records, "r", 2)
	if cached.head != len(records) {
		t.Fatalf("head = %d, want %d", cached.head, len(records))
	}
	extra := make([]event.Seqd, len(records)+1)
	copy(extra, records)
	extra[len(records)] = event.Seqd{Seq: uint64(len(records)) + 1, Event: event.ToolCompleted{CallID: "r/2", OK: false, Output: "boom"}}
	got := cached.trace(extra, "r", 2)
	want := traceEffect(extra, "r", 2)
	if got.completed == nil || want.completed == nil || got.completed.Output != "boom" || want.completed.Output != "boom" {
		t.Fatalf("last completed wins: want %+v got %+v", want.completed, got.completed)
	}
}

func BenchmarkTraceFullScan(b *testing.B) {
	records := indexTestRecords(2000)
	b.ResetTimer()
	for b.Loop() {
		trace := traceEffect(records, "r", 2)
		if trace.started == nil || trace.completed == nil {
			b.Fatalf("missing trace %+v", trace)
		}
	}
}

func BenchmarkTraceCachedIndex(b *testing.B) {
	records := indexTestRecords(2000)
	cached := newEffectCache()
	cached.trace(records, "r", 2)
	b.ResetTimer()
	for b.Loop() {
		trace := cached.trace(records, "r", 2)
		if trace.started == nil || trace.completed == nil {
			b.Fatalf("missing trace %+v", trace)
		}
	}
}
