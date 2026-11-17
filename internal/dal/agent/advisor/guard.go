package advisor

import (
	"slices"
	"strings"
	"sync"
	"unicode"

	"golang.org/x/text/unicode/norm"
)

type Severity uint8

const (
	SeverityNit Severity = iota + 1
	SeverityConcern
	SeverityBlocker
)

func ParseSeverity(name string) (Severity, bool) {
	switch name {
	case "nit":
		return SeverityNit, true
	case "concern":
		return SeverityConcern, true
	case "blocker":
		return SeverityBlocker, true
	}
	return 0, false
}

func (s Severity) String() string {
	switch s {
	case SeverityNit:
		return "nit"
	case SeverityConcern:
		return "concern"
	case SeverityBlocker:
		return "blocker"
	}
	return "unknown"
}

type Verdict string

const (
	VerdictAccepted  Verdict = "accepted"
	VerdictDuplicate Verdict = "duplicate"
	VerdictNoise     Verdict = "noise"
	VerdictBudget    Verdict = "budget"
)

type Admission struct {
	Verdict   Verdict
	Key       string
	Displaced string
	previous  Severity
}

func Normalize(note string) string {
	folded := strings.ToLower(norm.NFKC.String(note))
	var out strings.Builder
	out.Grow(len(folded))
	gap := false
	for _, r := range folded {
		if !unicode.IsLetter(r) && !unicode.IsNumber(r) {
			gap = true
			continue
		}
		if gap && out.Len() > 0 {
			out.WriteByte(' ')
		}
		gap = false
		out.WriteRune(r)
	}
	return out.String()
}

type slot struct {
	key      string
	severity Severity
	pending  bool
}

type Guard struct {
	mu       sync.Mutex
	maxNotes int
	seen     map[string]Severity
	slots    []slot
}

func NewGuard(maxNotes int) *Guard {
	return &Guard{maxNotes: maxNotes, seen: map[string]Severity{}, slots: make([]slot, 0, maxNotes)}
}

func (g *Guard) Admit(note string, severity Severity) Admission {
	key := Normalize(note)
	if key == "" {
		return Admission{Verdict: VerdictNoise}
	}
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.seen[key] >= severity {
		return Admission{Verdict: VerdictDuplicate, Key: key}
	}
	displaced, ok := g.reserve(key, severity)
	if !ok {
		return Admission{Verdict: VerdictBudget, Key: key}
	}
	previous := g.seen[key]
	g.seen[key] = severity
	return Admission{Verdict: VerdictAccepted, Key: key, Displaced: displaced, previous: previous}
}

func (g *Guard) Withdraw(admission Admission) {
	if admission.Verdict != VerdictAccepted {
		return
	}
	g.mu.Lock()
	defer g.mu.Unlock()
	g.seen[admission.Key] = admission.previous
}

func (g *Guard) reserve(key string, severity Severity) (string, bool) {
	own := slices.IndexFunc(g.slots, func(s slot) bool { return s.key == key })
	if own >= 0 {
		return g.escalate(own, severity), true
	}
	if severity == SeverityBlocker {
		return "", true
	}
	if len(g.slots) < g.maxNotes {
		g.slots = append(g.slots, slot{key: key, severity: severity, pending: true})
		return "", true
	}
	low := g.lowestPending()
	if low < 0 || g.slots[low].severity >= severity {
		return "", false
	}
	displaced := g.slots[low].key
	g.slots[low] = slot{key: key, severity: severity, pending: true}
	return displaced, true
}

func (g *Guard) escalate(own int, severity Severity) string {
	current := g.slots[own]
	if current.pending && severity == SeverityBlocker {
		g.slots = slices.Delete(g.slots, own, own+1)
		return current.key
	}
	g.slots[own].severity = severity
	if current.pending {
		return current.key
	}
	return ""
}

func (g *Guard) lowestPending() int {
	low := -1
	for i, current := range g.slots {
		if current.pending && (low < 0 || current.severity < g.slots[low].severity) {
			low = i
		}
	}
	return low
}

func (g *Guard) Delivered(key string) {
	g.mu.Lock()
	defer g.mu.Unlock()
	for i := range g.slots {
		if g.slots[i].key == key {
			g.slots[i].pending = false
		}
	}
}

func (g *Guard) BeginReview() {
	g.mu.Lock()
	defer g.mu.Unlock()
	g.slots = g.slots[:0]
}

func (g *Guard) Reset() {
	g.mu.Lock()
	defer g.mu.Unlock()
	clear(g.seen)
	g.slots = g.slots[:0]
}
