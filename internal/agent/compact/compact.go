package compact

import (
	"cmp"
	"math"
	"slices"
	"strings"
)

type Kind int

const (
	UserMessage Kind = iota + 1
	AssistantMessage
	ToolStarted
	ToolCompleted
	Other
)

type Entry struct {
	Seq         uint64
	Kind        Kind
	Bytes       int
	CallID      string
	CallIDs     []string
	RunID       string
	OutputBytes int
}

func Reserve(window int) int {
	if window <= 0 {
		return 16384
	}
	percent := window/100*15 + (window%100)*15/100
	if percent < 16384 {
		return 16384
	}
	return percent
}

func ShouldCompact(window, usedTokens int, threshold float64) bool {
	if window <= 0 || usedTokens <= 0 || math.IsNaN(threshold) {
		return false
	}
	available := window - Reserve(window)
	return float64(usedTokens) > threshold*float64(available)
}

func Cut(entries []Entry, keepTokens int, prevCompactionIdx int) (int, bool) {
	cut, ok := findCut(entries, tokensToBytes(keepTokens))
	if !ok {
		return 0, false
	}
	cut = turnBoundary(entries, cut)
	if cut <= prevCompactionIdx || cut == 0 {
		return 0, false
	}
	return cut, true
}

func findCut(entries []Entry, target int) (int, bool) {
	cut := len(entries)
	if target <= 0 {
		return cut, true
	}
	total := 0
	for i := len(entries) - 1; i >= 0; i-- {
		total = addBytes(total, entries[i].Bytes)
		if total >= target {
			return i, true
		}
	}
	return 0, false
}

func turnBoundary(entries []Entry, cut int) int {
	if cut <= 0 || cut >= len(entries) {
		return cut
	}
	first := scopeFirst(entries)
	earliest := len(entries)
	for i := len(entries) - 1; i >= cut; i-- {
		earliest = earliestScope(first, entries[i], earliest)
	}
	for cut > 0 && (!isBoundary(entries[cut].Kind) || earliest < cut) {
		cut--
		earliest = earliestScope(first, entries[cut], earliest)
	}
	return cut
}

func safeCut(entries []Entry, cut int) bool {
	if !isBoundary(entries[cut].Kind) {
		return false
	}
	first := scopeFirst(entries)
	earliest := len(entries)
	for i := len(entries) - 1; i >= cut; i-- {
		earliest = earliestScope(first, entries[i], earliest)
	}
	return earliest >= cut
}

func scopeFirst(entries []Entry) map[string]int {
	first := make(map[string]int)
	for i, entry := range entries {
		noteScope(first, entry.CallID, i)
		for _, id := range entry.CallIDs {
			noteScope(first, id, i)
		}
		noteScope(first, entry.RunID, i)
	}
	return first
}

func noteScope(first map[string]int, id string, i int) {
	if id == "" {
		return
	}
	root := scopeRoot(id)
	if _, ok := first[root]; !ok {
		first[root] = i
	}
}

func earliestScope(first map[string]int, entry Entry, earliest int) int {
	earliest = earliestID(first, entry.CallID, earliest)
	for _, id := range entry.CallIDs {
		earliest = earliestID(first, id, earliest)
	}
	return earliestID(first, entry.RunID, earliest)
}

func earliestID(first map[string]int, id string, earliest int) int {
	if id == "" {
		return earliest
	}
	if i, ok := first[scopeRoot(id)]; ok && i < earliest {
		return i
	}
	return earliest
}

func scopeRoot(id string) string {
	root, _, _ := strings.Cut(id, "/")
	return root
}

func PruneSelect(entries []Entry, protectTokens, minBytes, maxCount, targetTokens int) []string {
	if maxCount <= 0 || targetTokens <= 0 {
		return nil
	}
	protected := protectedOutputs(entries, tokensToBytes(protectTokens))
	candidates := pruneCandidates(entries, protected, minBytes)
	slices.SortStableFunc(candidates, func(left, right int) int {
		return cmp.Compare(entries[right].OutputBytes, entries[left].OutputBytes)
	})
	return selectPrune(entries, candidates, maxCount, tokensToBytes(targetTokens))
}

func pruneCandidates(entries []Entry, protected []bool, minBytes int) []int {
	candidates := make([]int, 0, len(entries))
	for i, entry := range entries {
		if entry.Kind == ToolCompleted && entry.OutputBytes >= minBytes && entry.OutputBytes > 0 && !protected[i] && entry.CallID != "" {
			candidates = append(candidates, i)
		}
	}
	return candidates
}

func selectPrune(entries []Entry, candidates []int, maxCount, target int) []string {
	selected := make([]string, 0, minInt(maxCount, len(candidates)))
	saved := 0
	for _, index := range candidates {
		selected = append(selected, entries[index].CallID)
		saved = addBytes(saved, entries[index].OutputBytes)
		if len(selected) == maxCount || saved >= target {
			break
		}
	}
	return selected
}

func isBoundary(kind Kind) bool {
	return kind == UserMessage || kind == AssistantMessage
}

func tokensToBytes(tokens int) int {
	if tokens <= 0 {
		return 0
	}
	if tokens > math.MaxInt/4 {
		return math.MaxInt
	}
	return tokens * 4
}

func addBytes(left, right int) int {
	if right <= 0 || left == math.MaxInt {
		return left
	}
	if right > math.MaxInt-left {
		return math.MaxInt
	}
	return left + right
}

func protectedOutputs(entries []Entry, protectBytes int) []bool {
	protected := make([]bool, len(entries))
	if protectBytes <= 0 {
		return protected
	}
	remaining := protectBytes
	for i := len(entries) - 1; i >= 0 && remaining > 0; i-- {
		if entries[i].Kind != ToolCompleted || entries[i].OutputBytes <= 0 {
			continue
		}
		protected[i] = true
		remaining -= entries[i].OutputBytes
	}
	return protected
}

func minInt(left, right int) int {
	if left < right {
		return left
	}
	return right
}
