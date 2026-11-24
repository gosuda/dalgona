package platform

import (
	"errors"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

func validTree() []SemanticNode {
	return []SemanticNode{
		{ID: "transcript", Role: RoleDocument, Name: "Transcript"},
		{ID: "heading", Parent: "transcript", Role: RoleHeading, Name: "Plan", Level: 2},
		{ID: "composer", Parent: "transcript", Role: RoleTextField, Name: "Message", Action: "Edit", Value: "draft"},
		{ID: "token", Parent: "transcript", Role: RoleSecureField, Name: "API token", Action: "Edit"},
		{ID: "token-error", Parent: "token", Role: RoleError, Name: "The token is empty", Target: "token"},
		{ID: "send", Parent: "transcript", Role: RoleButton, Name: "Send", Action: "Send", Disabled: true, Reason: "Connect to a daemon first"},
	}
}

func TestCheckSemanticsAcceptsACompleteTree(t *testing.T) {
	if err := CheckSemantics(validTree()); err != nil {
		t.Fatal(err)
	}
}

func TestCheckSemanticsRejectsIncompleteNodes(t *testing.T) {
	cases := map[string]func([]SemanticNode) []SemanticNode{
		"unnamed control":        func(n []SemanticNode) []SemanticNode { n[5].Name = ""; return n },
		"control without action": func(n []SemanticNode) []SemanticNode { n[2].Action = ""; return n },
		"secure value exposed":   func(n []SemanticNode) []SemanticNode { n[3].Value = "sk-live"; return n },
		"detached error":         func(n []SemanticNode) []SemanticNode { n[4].Target = "missing"; return n },
		"error without target":   func(n []SemanticNode) []SemanticNode { n[4].Target = ""; return n },
		"heading level zero":     func(n []SemanticNode) []SemanticNode { n[1].Level = 0; return n },
		"heading level seven":    func(n []SemanticNode) []SemanticNode { n[1].Level = 7; return n },
		"repeated identity":      func(n []SemanticNode) []SemanticNode { n[1].ID = n[0].ID; return n },
		"empty identity":         func(n []SemanticNode) []SemanticNode { n[0].ID = ""; return n },
		"unknown role":           func(n []SemanticNode) []SemanticNode { n[0].Role = 0; return n },
	}
	for name, breakTree := range cases {
		if err := CheckSemantics(breakTree(validTree())); err == nil {
			t.Errorf("%s: the tree was accepted", name)
		}
	}
}

func TestEveryControlRoleNeedsNameAndOperation(t *testing.T) {
	controls := []Role{RoleButton, RoleCheckBox, RoleRadioButton, RoleSwitch, RoleTextField, RoleSecureField, RoleLink, RoleMenuItem}
	for _, role := range controls {
		if err := CheckSemantics([]SemanticNode{{ID: "c", Role: role, Name: "Name"}}); err == nil {
			t.Errorf("role %d accepted a control without an operation", role)
		}
		if err := CheckSemantics([]SemanticNode{{ID: "c", Role: role, Action: "Activate"}}); err == nil {
			t.Errorf("role %d accepted a control without a name", role)
		}
	}
}

func TestSnapshotRefusesStaleIdentifiers(t *testing.T) {
	snap, err := NewSemanticSnapshot(9, "composer", validTree())
	if err != nil {
		t.Fatal(err)
	}
	if n, err := snap.Lookup("send", 9); err != nil || n.Name != "Send" {
		t.Fatalf("current lookup: node %+v, error %v", n, err)
	}
	if _, err := snap.Lookup("send", 8); !errors.Is(err, ErrElementUnavailable) {
		t.Fatalf("stale revision: error %v, want ErrElementUnavailable", err)
	}
	if _, err := snap.Lookup("gone", 9); !errors.Is(err, ErrElementUnavailable) {
		t.Fatalf("unknown node: error %v, want ErrElementUnavailable", err)
	}
	if _, err := NewSemanticSnapshot(10, "", []SemanticNode{{ID: "x", Role: RoleButton}}); err == nil {
		t.Fatal("a snapshot accepted an invalid tree")
	}
}

func checkTextObjects(t *testing.T, text string, wantEnds []int) {
	t.Helper()
	objects := TextObjects(text)
	var joined strings.Builder
	ends := make([]int, 0, len(objects))
	start := 0
	for _, o := range objects {
		if o.Start != start || o.End-o.Start > 64*1024 || o.End <= o.Start {
			t.Fatalf("object %+v after offset %d breaks the 64 KiB contiguous bound", o, start)
		}
		joined.WriteString(text[o.Start:o.End])
		ends = append(ends, o.End)
		start = o.End
	}
	if joined.String() != text {
		t.Fatal("the text objects do not reproduce the complete text")
	}
	if !slices.Equal(ends, wantEnds) {
		t.Fatalf("object ends %v, want %v", ends, wantEnds)
	}
}

func TestTextObjectsSplitWholeGraphemesWithinTheBound(t *testing.T) {
	const limit = 64 * 1024
	family := "\U0001F469\u200D\U0001F469\u200D\U0001F467"
	text := strings.Repeat("a", limit-3) + family + strings.Repeat("b", 10)
	checkTextObjects(t, text, []int{limit - 3, len(text)})
	checkTextObjects(t, "", nil)
	checkTextObjects(t, strings.Repeat("가", limit/3), []int{limit / 3 * 3})
}

func TestTextObjectsSplitAnOversizedGraphemeAtScalars(t *testing.T) {
	const limit = 64 * 1024
	text := "e" + strings.Repeat("\u0301", 40000)
	checkTextObjects(t, text, []int{limit - 1, len(text)})
	for _, o := range TextObjects(text) {
		if _, err := inputtext.DecodeUTF8([]byte(text[o.Start:o.End])); err != nil {
			t.Fatalf("object %+v splits a scalar", o)
		}
	}
}

func TestAnnouncerCoalescesProgressToOnePerSecond(t *testing.T) {
	var a Announcer
	t0 := time.Unix(1_800_000_000, 0)
	progress := func(text string) Announcement { return Announcement{Kind: AnnounceProgress, Text: text} }
	if n, ok := a.Announce(t0, progress("token 1")); !ok || n.Text != "token 1" {
		t.Fatalf("first progress: %+v %v", n, ok)
	}
	for i, text := range []string{"token 2", "token 3", "token 4"} {
		if n, ok := a.Announce(t0.Add(time.Duration(i+1)*200*time.Millisecond), progress(text)); ok {
			t.Fatalf("progress %q announced %+v within one second", text, n)
		}
	}
	checkProgressFlush(t, &a, t0)
}

func checkProgressFlush(t *testing.T, a *Announcer, t0 time.Time) {
	t.Helper()
	if !a.Due().Equal(t0.Add(time.Second)) {
		t.Fatalf("due %v, want one second after the last progress", a.Due())
	}
	if _, ok := a.Flush(t0.Add(999 * time.Millisecond)); ok {
		t.Fatal("progress flushed before one second elapsed")
	}
	if n, ok := a.Flush(t0.Add(time.Second)); !ok || n.Text != "token 4" {
		t.Fatalf("flush after one second: %+v %v, want the latest progress", n, ok)
	}
	if _, ok := a.Flush(t0.Add(3 * time.Second)); ok {
		t.Fatal("one progress update was announced twice")
	}
}

func TestAnnouncerKeepsDecisionsAndOutcomesTimelyAndDistinct(t *testing.T) {
	var a Announcer
	t0 := time.Unix(1_800_000_000, 0)
	a.Announce(t0, Announcement{Kind: AnnounceProgress, Text: "working"})
	a.Announce(t0.Add(100*time.Millisecond), Announcement{Kind: AnnounceProgress, Text: "still working"})
	urgent := []Announcement{
		{Kind: AnnounceDecision, Text: "Approve the command"},
		{Kind: AnnounceFailure, Text: "Failed"},
		{Kind: AnnounceFailure, Text: "Failed"},
		{Kind: AnnounceCompletion, Text: "Done"},
	}
	for i, n := range urgent {
		got, ok := a.Announce(t0.Add(time.Duration(200+i)*time.Millisecond), n)
		if !ok || got != n {
			t.Fatalf("urgent announcement %d: %+v %v, want %+v immediately", i, got, ok, n)
		}
	}
	if n, ok := a.Flush(t0.Add(5 * time.Second)); ok {
		t.Fatalf("superseded progress %+v was announced after the outcome", n)
	}
}
