package compact

import (
	"fmt"
	"math/rand"
	"testing"
)

func TestReserveAndShouldCompact(t *testing.T) {
	if got := Reserve(200000); got != 30000 {
		t.Fatalf("Reserve() = %d", got)
	}
	if !ShouldCompact(200000, 170000, 0.8) {
		t.Fatal("expected compaction")
	}
	if ShouldCompact(200000, 130000, 0.8) {
		t.Fatal("unexpected compaction")
	}
}

func TestCutKeepsTurnBoundary(t *testing.T) {
	entries := []Entry{
		{Kind: UserMessage, Bytes: 20},
		{Kind: AssistantMessage, Bytes: 20, CallIDs: []string{"a"}},
		{Kind: ToolStarted, Bytes: 20, CallID: "a"},
		{Kind: ToolCompleted, Bytes: 20, CallID: "a"},
		{Kind: UserMessage, Bytes: 20},
		{Kind: AssistantMessage, Bytes: 20},
	}
	cut, ok := Cut(entries, 11, 0)
	if !ok || cut != 1 {
		t.Fatalf("Cut() = %d, %t", cut, ok)
	}
	if _, ok := Cut(entries, 100, 0); ok {
		t.Fatal("compacted history shorter than keep target")
	}
}

func TestCutSkipsSteerInsideToolBatch(t *testing.T) {
	entries := []Entry{
		{Kind: UserMessage, Bytes: 20},
		{Kind: AssistantMessage, Bytes: 20, CallIDs: []string{"a"}},
		{Kind: ToolStarted, Bytes: 20, CallID: "a"},
		{Kind: UserMessage, Bytes: 20},
		{Kind: ToolCompleted, Bytes: 20, CallID: "a"},
		{Kind: UserMessage, Bytes: 20},
	}
	cut, ok := Cut(entries, 8, 0)
	if !ok {
		t.Fatal("Cut() reported no boundary")
	}
	if cut != 1 {
		t.Fatalf("Cut() = %d, want the assistant call at 1", cut)
	}
	if entries[cut].Kind != AssistantMessage {
		t.Fatalf("cut starts at %v", entries[cut].Kind)
	}
	if _, ok := Cut(entries[:2], 8, 0); ok {
		t.Fatal("Cut() found a boundary while a tool batch stayed pending with no call to keep")
	}
}

func TestCutKeepsDeclinedSiblingWithIssuer(t *testing.T) {
	entries := []Entry{
		{Kind: UserMessage, Bytes: 20},
		{Kind: AssistantMessage, Bytes: 20, CallIDs: []string{"a", "b"}},
		{Kind: ToolStarted, Bytes: 20, CallID: "a"},
		{Kind: ToolCompleted, Bytes: 20, CallID: "b"},
		{Kind: UserMessage, Bytes: 20},
		{Kind: ToolCompleted, Bytes: 20, CallID: "a"},
		{Kind: UserMessage, Bytes: 20},
	}
	cut, ok := Cut(entries, 11, 0)
	if !ok {
		t.Fatal("Cut() reported no boundary")
	}
	if cut != 1 {
		t.Fatalf("Cut() = %d, want the assistant call at 1", cut)
	}
	if entries[cut].Kind != AssistantMessage {
		t.Fatalf("cut starts at %v", entries[cut].Kind)
	}
}

func TestPruneSelectLargestOlderOutputs(t *testing.T) {
	entries := []Entry{
		{Kind: ToolCompleted, CallID: "old-small", OutputBytes: 9000},
		{Kind: ToolCompleted, CallID: "old-large", OutputBytes: 20000},
		{Kind: ToolCompleted, CallID: "newest", OutputBytes: 40000},
	}
	got := PruneSelect(entries, 10000, 2000, 20, 5000)
	if len(got) != 1 || got[0] != "old-large" {
		t.Fatalf("PruneSelect() = %#v", got)
	}
}

func TestCutPropertyNeverSplitsToolBatch(t *testing.T) {
	rng := rand.New(rand.NewSource(42))
	for range 500 {
		entries := randomEntries(rng)
		cut, ok := Cut(entries, rng.Intn(30)+1, 0)
		if !ok {
			continue
		}
		assertCutKeepsIssuers(t, entries, cut)
	}
}

func randomEntries(rng *rand.Rand) []Entry {
	entries := make([]Entry, 0, 30)
	for turn := range 5 {
		entries = append(entries, Entry{Kind: UserMessage, Bytes: rng.Intn(20) + 1})
		ids := randomCallIDs(rng, turn)
		entries = append(entries, Entry{Kind: AssistantMessage, Bytes: rng.Intn(20) + 1, CallIDs: ids})
		if len(ids) == 2 && rng.Intn(2) == 0 {
			entries = append(entries, declinedSiblingBatch(rng, ids)...)
			continue
		}
		for _, id := range ids {
			entries = append(entries, Entry{Kind: ToolStarted, Bytes: rng.Intn(20) + 1, CallID: id})
			if rng.Intn(4) == 0 {
				entries = append(entries, Entry{Kind: UserMessage, Bytes: rng.Intn(20) + 1})
			}
			entries = append(entries, Entry{Kind: ToolCompleted, Bytes: rng.Intn(20) + 1, CallID: id})
		}
	}
	return entries
}

func randomCallIDs(rng *rand.Rand, turn int) []string {
	count := rng.Intn(3)
	ids := make([]string, 0, count)
	for call := range count {
		ids = append(ids, fmt.Sprintf("t%dc%d", turn, call))
	}
	return ids
}

func declinedSiblingBatch(rng *rand.Rand, ids []string) []Entry {
	return []Entry{
		{Kind: ToolStarted, Bytes: rng.Intn(20) + 1, CallID: ids[0]},
		{Kind: ToolCompleted, Bytes: rng.Intn(20) + 1, CallID: ids[1]},
		{Kind: UserMessage, Bytes: rng.Intn(20) + 1},
		{Kind: ToolCompleted, Bytes: rng.Intn(20) + 1, CallID: ids[0]},
	}
}

func assertCutKeepsIssuers(t *testing.T, entries []Entry, cut int) {
	t.Helper()
	if entries[cut].Kind != UserMessage && entries[cut].Kind != AssistantMessage {
		t.Fatalf("cut %d starts at %v", cut, entries[cut].Kind)
	}
	issued := issuedIDs(entries, cut)
	for i := cut; i < len(entries); i++ {
		if !exposed(entries[i]) {
			continue
		}
		if _, ok := issued[entries[i].CallID]; !ok {
			t.Fatalf("cut %d orphans call %q", cut, entries[i].CallID)
		}
	}
}
