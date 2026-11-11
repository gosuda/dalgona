package compact

import (
	"fmt"
	"math/rand"
	"strconv"
	"strings"
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
		assertCutKeepsScopes(t, entries, cut)
	}
}

func randomEntries(rng *rand.Rand) []Entry {
	entries := make([]Entry, 0, 40)
	for turn := range 5 {
		entries = append(entries, Entry{Kind: UserMessage, Bytes: rng.Intn(20) + 1})
		ids := randomCallIDs(rng, turn)
		entries = append(entries, Entry{Kind: AssistantMessage, Bytes: rng.Intn(20) + 1, CallIDs: ids})
		if len(ids) == 2 && rng.Intn(2) == 0 {
			entries = append(entries, declinedSiblingBatch(rng, ids)...)
			continue
		}
		for _, id := range ids {
			if rng.Intn(4) == 0 {
				entries = append(entries, nestedScriptBatch(rng, id)...)
				continue
			}
			entries = append(entries, Entry{Kind: ToolStarted, Bytes: rng.Intn(20) + 1, CallID: id})
			if rng.Intn(4) == 0 {
				entries = append(entries, Entry{Kind: UserMessage, Bytes: rng.Intn(20) + 1})
			}
			entries = append(entries, Entry{Kind: ToolCompleted, Bytes: rng.Intn(20) + 1, CallID: id})
		}
		if rng.Intn(5) == 0 {
			entries = append(entries, Entry{Kind: Other, Bytes: rng.Intn(20) + 1})
		}
	}
	return entries
}

func nestedScriptBatch(rng *rand.Rand, id string) []Entry {
	out := []Entry{
		{Kind: ToolStarted, Bytes: rng.Intn(20) + 1, CallID: id},
		{Kind: Other, Bytes: rng.Intn(20) + 1, RunID: id},
	}
	for n := range rng.Intn(3) {
		call := id + "/" + strconv.Itoa(n)
		if rng.Intn(3) != 0 {
			out = append(out, Entry{Kind: ToolStarted, Bytes: rng.Intn(20) + 1, CallID: call})
		}
		out = append(out, Entry{Kind: ToolCompleted, Bytes: rng.Intn(20) + 1, CallID: call})
	}
	if rng.Intn(3) == 0 {
		out = append(out, Entry{Kind: Other, Bytes: rng.Intn(20) + 1, CallID: id + "/spawn"})
	}
	return append(out,
		Entry{Kind: Other, Bytes: rng.Intn(20) + 1, RunID: id},
		Entry{Kind: ToolCompleted, Bytes: rng.Intn(20) + 1, CallID: id},
	)
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

func assertCutKeepsScopes(t *testing.T, entries []Entry, cut int) {
	t.Helper()
	if entries[cut].Kind != UserMessage && entries[cut].Kind != AssistantMessage {
		t.Fatalf("cut %d starts at %v", cut, entries[cut].Kind)
	}
	before := make(map[string]struct{})
	for _, entry := range entries[:cut] {
		for _, id := range entryIDs(entry) {
			before[testRoot(id)] = struct{}{}
		}
	}
	for _, entry := range entries[cut:] {
		for _, id := range entryIDs(entry) {
			if _, ok := before[testRoot(id)]; ok {
				t.Fatalf("cut %d splits scope of %q", cut, id)
			}
		}
	}
}

func entryIDs(entry Entry) []string {
	ids := make([]string, 0, 3)
	if entry.CallID != "" {
		ids = append(ids, entry.CallID)
	}
	for _, id := range entry.CallIDs {
		if id != "" {
			ids = append(ids, id)
		}
	}
	if entry.RunID != "" {
		ids = append(ids, entry.RunID)
	}
	return ids
}

func testRoot(id string) string {
	root, _, _ := strings.Cut(id, "/")
	return root
}

func TestCutKeepsNestedScriptFamily(t *testing.T) {
	entries := []Entry{
		{Kind: UserMessage, Bytes: 20},
		{Kind: AssistantMessage, Bytes: 20, CallIDs: []string{"e"}},
		{Kind: ToolStarted, Bytes: 20, CallID: "e"},
		{Kind: Other, Bytes: 20, RunID: "e"},
		{Kind: ToolStarted, Bytes: 20, CallID: "e/1"},
		{Kind: UserMessage, Bytes: 20},
		{Kind: ToolCompleted, Bytes: 20, CallID: "e/1"},
		{Kind: Other, Bytes: 20, RunID: "e"},
		{Kind: ToolCompleted, Bytes: 20, CallID: "e"},
		{Kind: UserMessage, Bytes: 20},
	}
	cut, ok := Cut(entries, 11, 0)
	if !ok {
		t.Fatal("Cut() reported no boundary")
	}
	if cut != 1 {
		t.Fatalf("Cut() = %d, want the assistant call at 1", cut)
	}
	if !safeCut(entries, 1) {
		t.Fatal("safeCut() rejected the assistant call that keeps the family")
	}
	if safeCut(entries, 5) {
		t.Fatal("safeCut() allowed a cut inside a nested script family")
	}
}
