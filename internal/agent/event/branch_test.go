package event_test

import (
	"fmt"
	"maps"
	"math/rand/v2"
	"reflect"
	"slices"
	"strings"
	"testing"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func TestActivePathMatchesIndependentTurnTree(t *testing.T) {
	t.Parallel()
	random := rand.New(rand.NewPCG(81, 17))
	for iteration := range 200 {
		journal := make([]event.Seqd, 0, 128)
		parents := make(map[string]string)
		completed := make(map[string]uint64)
		leaf := ""
		nextTurn := 0
		for range 40 {
			if nextTurn == 0 || len(completed) == 0 || random.IntN(3) > 0 {
				turn := fmt.Sprintf("t%d", nextTurn)
				nextTurn++
				parents[turn] = leaf
				appendTurn(&journal, completed, turn, leaf)
				leaf = turn
				continue
			}
			leaf = chooseLeaf(random, completed)
			appendBranch(&journal, completed, leaf)
		}
		wantTurns := referencePath(parents, leaf)
		gotTurns := activeTurnIDs(event.ActivePath(journal))
		if !reflect.DeepEqual(gotTurns, wantTurns) {
			t.Fatalf("iteration %d active path = %v, want %v", iteration, gotTurns, wantTurns)
		}
		assertTreeMatchesReference(t, iteration, event.Tree(journal), parents, wantTurns)
	}
}

func TestActivePathHandlesRootAndMalformedBranch(t *testing.T) {
	t.Parallel()
	journal := []event.Seqd{
		{Seq: 1, Event: event.TurnStarted{TurnID: "a"}},
		{Seq: 2, Event: event.TurnCompleted{TurnID: "a", Status: "completed"}},
		{Seq: 3, Event: event.TurnStarted{TurnID: "b"}},
		{Seq: 4, Event: event.TurnCompleted{TurnID: "b", Status: "completed"}},
		{Seq: 5, Event: event.Branch{ParentSeq: 0}},
		{Seq: 6, Event: event.TurnStarted{TurnID: "c"}},
		{Seq: 7, Event: event.TurnCompleted{TurnID: "c", Status: "completed"}},
	}
	if got := activeTurnIDs(event.ActivePath(journal)); !reflect.DeepEqual(got, []string{"c"}) {
		t.Fatalf("root branch path = %v", got)
	}
	nodes := event.Tree(journal)
	if nodes[2].ParentTurnID != "" || !nodes[2].Active || nodes[0].Active || nodes[1].Active {
		t.Fatalf("root branch tree = %#v", nodes)
	}
	malformed := []event.Seqd{{Seq: 9, Event: event.Branch{ParentSeq: 8}}}
	if got := event.ActivePath(malformed); len(got) != 0 {
		t.Fatalf("missing parent produced path %#v", got)
	}
}

func TestTreePreviewUsesEightyUnicodeCharacters(t *testing.T) {
	t.Parallel()
	prompt := strings.Repeat("달", 81)
	journal := []event.Seqd{
		{Seq: 1, Event: event.TurnStarted{TurnID: "t"}},
		{Seq: 2, Event: event.UserMessage{ItemID: "u", TurnID: "t", Text: prompt, Kind: "prompt"}},
		{Seq: 3, Event: event.TurnCompleted{TurnID: "t", Status: "completed"}},
	}
	nodes := event.Tree(journal)
	if len(nodes) != 1 || utf8.RuneCountInString(nodes[0].Preview) != 80 {
		t.Fatalf("preview has %d runes", utf8.RuneCountInString(nodes[0].Preview))
	}
	if nodes[0].Preview != strings.Repeat("달", 80) {
		t.Fatal("preview split or changed the prompt")
	}
}

func appendTurn(journal *[]event.Seqd, completed map[string]uint64, turn, parent string) {
	start := nextSeq(*journal)
	*journal = append(*journal,
		event.Seqd{Seq: start, Event: event.TurnStarted{TurnID: turn}},
		event.Seqd{Seq: start + 1, Event: event.UserMessage{ItemID: "u" + turn, TurnID: turn, Text: "prompt " + turn, Kind: "prompt"}},
		event.Seqd{Seq: start + 2, Event: event.TurnCompleted{TurnID: turn, Status: "completed"}},
	)
	completed[turn] = start + 2
}

func appendBranch(journal *[]event.Seqd, completed map[string]uint64, leaf string) {
	seq := nextSeq(*journal)
	*journal = append(*journal, event.Seqd{Seq: seq, Event: event.Branch{ParentSeq: completed[leaf]}})
}

func nextSeq(journal []event.Seqd) uint64 {
	if len(journal) == 0 {
		return 1
	}
	return journal[len(journal)-1].Seq + 1
}

func chooseLeaf(random *rand.Rand, completed map[string]uint64) string {
	keys := slices.Sorted(maps.Keys(completed))
	if random.IntN(4) == 0 {
		return ""
	}
	return keys[random.IntN(len(keys))]
}

func referencePath(parents map[string]string, leaf string) []string {
	var reversed []string
	for leaf != "" {
		reversed = append(reversed, leaf)
		leaf = parents[leaf]
	}
	slices.Reverse(reversed)
	return reversed
}

func activeTurnIDs(path []event.Seqd) []string {
	var turns []string
	for _, item := range path {
		turn, ok := item.Event.(event.TurnStarted)
		if ok {
			turns = append(turns, turn.TurnID)
		}
	}
	return turns
}

func assertTreeMatchesReference(t *testing.T, iteration int, nodes []event.TurnNode, parents map[string]string, active []string) {
	t.Helper()
	activeSet := make(map[string]bool, len(active))
	for _, turn := range active {
		activeSet[turn] = true
	}
	for _, node := range nodes {
		if node.ParentTurnID != parents[node.TurnID] || node.Active != activeSet[node.TurnID] {
			t.Fatalf("iteration %d node = %#v, parent %q active %v", iteration, node, parents[node.TurnID], activeSet[node.TurnID])
		}
	}
}
