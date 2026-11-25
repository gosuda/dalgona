package platform

import (
	"errors"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/textinput"
)

func prompt(maxScalars int) textinput.Policy {
	return textinput.Policy{Lines: textinput.MultiLine, Limit: textinput.Limit{Unit: textinput.Scalars, Max: maxScalars}}
}

func newComposer(t *testing.T, policy textinput.Policy, text string, selection textinput.Range) *Composer {
	t.Helper()
	c, err := NewComposer(policy, textinput.Draft{Text: text, Selection: selection})
	if err != nil {
		t.Fatal(err)
	}
	return c
}

func caretAt(i int) textinput.Range { return textinput.Range{Start: i, End: i} }

func TestCompositionCommitInsertsTextExactlyOnce(t *testing.T) {
	c := newComposer(t, prompt(100), "", caretAt(0))
	for _, preedit := range []string{"ㅎ", "하", "한", "한ㄱ", "한그", "한글"} {
		c.Compose(preedit)
		if c.Draft().Text != "" {
			t.Fatalf("preedit %q entered committed text %q", preedit, c.Draft().Text)
		}
	}
	if c.Preedit() != "한글" {
		t.Fatalf("preedit %q, want the latest platform preedit", c.Preedit())
	}
	if err := c.Commit("한글"); err != nil {
		t.Fatal(err)
	}
	want := textinput.Draft{Text: "한글", Selection: caretAt(len("한글"))}
	if c.Draft() != want || c.Preedit() != "" || c.Composing() {
		t.Fatalf("after commit: draft %+v, preedit %q, composing %v; want %+v with no composition", c.Draft(), c.Preedit(), c.Composing(), want)
	}
}

func TestCompositionCancelRestoresCommittedTextAndSelection(t *testing.T) {
	before := textinput.Draft{Text: "abc", Selection: textinput.Range{Start: 1, End: 2}}
	c := newComposer(t, prompt(100), before.Text, before.Selection)
	c.Compose("か")
	c.Compose("漢")
	c.Cancel()
	if c.Draft() != before || c.Composing() || c.Preedit() != "" {
		t.Fatalf("after cancel: draft %+v, composing %v, preedit %q; want %+v", c.Draft(), c.Composing(), c.Preedit(), before)
	}
}

func TestCompositionCommitReplacesTheSelection(t *testing.T) {
	c := newComposer(t, prompt(100), "abcdef", textinput.Range{Start: 2, End: 4})
	c.Compose("中")
	if err := c.Commit("中文"); err != nil {
		t.Fatal(err)
	}
	want := textinput.Draft{Text: "ab中文ef", Selection: caretAt(len("ab中文"))}
	if c.Draft() != want {
		t.Fatalf("draft %+v, want %+v", c.Draft(), want)
	}
}

func TestCompositionRejectedCommitKeepsARecoverableCandidate(t *testing.T) {
	before := textinput.Draft{Text: "abcd", Selection: caretAt(4)}
	c := newComposer(t, prompt(5), before.Text, before.Selection)
	c.Compose("ef")
	err := c.Commit("ef")
	if !errors.Is(err, textinput.TextTooLarge) {
		t.Fatalf("commit one scalar over the limit: error %v, want TextTooLarge", err)
	}
	if c.Draft() != before {
		t.Fatalf("a rejected commit inserted a prefix: draft %+v", c.Draft())
	}
	if c.Composing() {
		t.Fatal("a rejected commit left the composition active")
	}
	if got := c.TakeRejected(); got != "ef" {
		t.Fatalf("recoverable candidate %q, want %q", got, "ef")
	}
	if got := c.TakeRejected(); got != "" {
		t.Fatalf("the candidate was returned twice: %q", got)
	}
}

func TestCompositionCommitAtTheFieldLimit(t *testing.T) {
	exact := newComposer(t, prompt(5), "abcd", caretAt(4))
	if err := exact.Commit("한"); err != nil {
		t.Fatalf("commit reaching exactly the limit: %v", err)
	}
	joined := newComposer(t, prompt(5), "abce", caretAt(4))
	if err := joined.Commit("\u0301"); err != nil {
		t.Fatalf("commit joining the adjacent grapheme at the limit: %v", err)
	}
	want := textinput.Draft{Text: "abce\u0301", Selection: caretAt(len("abce\u0301"))}
	if joined.Draft() != want {
		t.Fatalf("joined commit draft %+v, want %+v", joined.Draft(), want)
	}
}

func TestCompositionOwnsKeysThroughTheEndingKey(t *testing.T) {
	c := newComposer(t, prompt(100), "", caretAt(0))
	for _, name := range []string{"⏎", "⎋", "⇥", "↑"} {
		if c.Owns(KeyPress{Name: name}) {
			t.Fatalf("idle field: the composition claimed %s", name)
		}
	}
	c.Compose("にほん")
	for _, name := range []string{"⏎", "⎋", "⇥", "↑", "Space"} {
		if !c.Owns(KeyPress{Name: name}) {
			t.Fatalf("active composition: application received %s", name)
		}
	}
	if err := c.Commit("日本"); err != nil {
		t.Fatal(err)
	}
	if !c.Owns(KeyPress{Name: "⏎", Composing: true}) {
		t.Fatal("the key that ended the composition reached the application after the commit")
	}
	if c.Owns(KeyPress{Name: "⏎"}) {
		t.Fatal("a later Enter press stayed owned by the finished composition")
	}
}

func TestCompositionBlocksApplicationEditsWhileActive(t *testing.T) {
	before := textinput.Draft{Text: "abc", Selection: caretAt(3)}
	c := newComposer(t, prompt(100), before.Text, before.Selection)
	c.Compose("ㄱ")
	if err := c.Replace("pasted"); !errors.Is(err, ErrComposing) {
		t.Fatalf("replacement during composition: error %v, want ErrComposing", err)
	}
	if err := c.Select(caretAt(0)); !errors.Is(err, ErrComposing) {
		t.Fatalf("selection during composition: error %v, want ErrComposing", err)
	}
	if c.Draft() != before || c.Preedit() != "ㄱ" {
		t.Fatalf("application edits overwrote the composition: draft %+v, preedit %q", c.Draft(), c.Preedit())
	}
}

func TestComposerSelectionRespectsGraphemeBoundaries(t *testing.T) {
	c := newComposer(t, prompt(100), "e\u0301x", caretAt(0))
	if err := c.Select(caretAt(1)); !errors.Is(err, textinput.InvalidOffset) {
		t.Fatalf("selection inside a grapheme: error %v, want InvalidOffset", err)
	}
	if err := c.Select(textinput.Range{Start: 0, End: len("e\u0301")}); err != nil {
		t.Fatalf("selection of a whole grapheme: %v", err)
	}
}

func TestNewComposerRejectsAnInvalidRestoredDraft(t *testing.T) {
	if _, err := NewComposer(prompt(100), textinput.Draft{Text: "a\x00b"}); !errors.Is(err, textinput.DisallowedControl) {
		t.Fatalf("restored draft with NUL: error %v, want DisallowedControl", err)
	}
	if _, err := NewComposer(prompt(100), textinput.Draft{Text: "e\u0301", Selection: caretAt(1)}); !errors.Is(err, textinput.InvalidOffset) {
		t.Fatalf("restored selection inside a grapheme: error %v, want InvalidOffset", err)
	}
}
