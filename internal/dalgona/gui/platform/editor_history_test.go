//go:build gui

package platform

import (
	"reflect"
	"strings"
	"testing"

	"gioui.org/io/key"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dal/textinput"
)

const (
	contractUndoSteps = 1000
	contractUndoBytes = 32 << 20
	historyEdits      = 100_000
)

func gioHistory(w *widget.Editor) reflect.Value {
	return reflect.ValueOf(w).Elem().FieldByName("history")
}

func gioHistoryBytes(w *widget.Editor) int {
	history, total := gioHistory(w), 0
	for i := range history.Len() {
		total += history.Index(i).FieldByName("ApplyContent").Len() + history.Index(i).FieldByName("ReverseContent").Len()
	}
	return total
}

func cycleLetter(i int) string {
	return string(rune('a' + i%26))
}

func replaceAll(t *testing.T, ed *Editor, text string) {
	t.Helper()
	if err := ed.Replace(textinput.Range{End: len(ed.Draft().Text)}, text); err != nil {
		t.Fatal(err)
	}
}

func TestEditorKeepsTheLastThousandUndoStepsAcross100kEdits(t *testing.T) {
	rig := newEditorRig(t, prompt(100), textinput.Draft{})
	for i := range historyEdits {
		replaceAll(t, rig.editor, cycleLetter(i))
		if i%contractUndoSteps != contractUndoSteps-1 {
			continue
		}
		rig.frame()
		if steps, gio := len(rig.editor.history), gioHistory(&rig.editor.widget).Len(); steps > contractUndoSteps || gio > contractUndoSteps+1 {
			t.Fatalf("after %d edits the editor holds %d undo steps and Gio %d; want at most %d and %d", i+1, steps, gio, contractUndoSteps, contractUndoSteps+1)
		}
	}
	for k := 1; k <= contractUndoSteps+1; k++ {
		rig.press("Z", key.ModShortcut)
		rig.frame()
		want := cycleLetter(historyEdits - 1 - min(k, contractUndoSteps))
		rig.wantDraft(textinput.Draft{Text: want, Selection: caretAt(1)})
		rig.wantShown(want)
	}
	for range contractUndoSteps {
		rig.press("Z", key.ModShortcut|key.ModShift)
		rig.frame()
	}
	last := cycleLetter(historyEdits - 1)
	rig.wantDraft(textinput.Draft{Text: last, Selection: caretAt(1)})
	rig.wantShown(last)
}

func TestEditorUndoHistoryKeepsTheNewestStepsWithin32MiB(t *testing.T) {
	const block = 3 << 20
	var ed Editor
	text := ""
	for i := range 20 {
		next := strings.Repeat(cycleLetter(i), block)
		ed.record(textinput.Draft{Text: text}, textinput.Draft{Text: next})
		text = next
	}
	kept, held := len(ed.history), 0
	for _, tx := range ed.history {
		held += len(tx.removed) + len(tx.inserted)
	}
	if held > contractUndoBytes || (kept+1)*2*block <= contractUndoBytes || ed.applied != kept {
		t.Fatalf("%d undo steps hold %d bytes; want the most %d-byte steps that fit in %d bytes", kept, held, 2*block, contractUndoBytes)
	}
	if newest := ed.history[kept-1]; newest.inserted != text || newest.removed != strings.Repeat(cycleLetter(18), block) {
		t.Fatal("the newest undo step is not the last replacement")
	}
}

func TestEditorUndoHistoryKeepsANewestStepLargerThan32MiB(t *testing.T) {
	var ed Editor
	huge := []string{strings.Repeat("x", 17<<20), strings.Repeat("y", 17<<20)}
	ed.record(textinput.Draft{Text: "a"}, textinput.Draft{Text: huge[0]})
	ed.record(textinput.Draft{Text: huge[0]}, textinput.Draft{Text: huge[1]})
	if len(ed.history) != 1 || ed.history[0].removed != huge[0] || ed.history[0].inserted != huge[1] {
		t.Fatalf("%d undo steps after one %d-byte replacement; want that replacement alone", len(ed.history), 2*len(huge[0]))
	}
}

func TestEditorBoundsGioHistoryBytes(t *testing.T) {
	const block = 3 << 20
	ed, err := NewEditor(prompt(block), textinput.Draft{})
	if err != nil {
		t.Fatal(err)
	}
	for i := range 8 {
		ed.showView(strings.Repeat(string(rune(0x1F600+i)), block/4), key.Range{})
		if gio := gioHistoryBytes(&ed.widget); gio > contractUndoBytes+block {
			t.Fatalf("after %d replacements Gio holds %d history bytes; want at most %d", i+1, gio, contractUndoBytes+block)
		}
	}
}

func TestEditorResetsGioHistoryOnlyOutsideComposition(t *testing.T) {
	rig := newEditorRig(t, prompt(100), textinput.Draft{Text: "hello", Selection: caretAt(5)})
	rig.press(key.NameLeftArrow, key.ModShift)
	rig.frame()
	rig.wantDraft(textinput.Draft{Text: "hello", Selection: textinput.Range{Start: 4, End: 5}})
	rig.editor.widgetEdits = contractUndoSteps
	rig.frame()
	if gio := gioHistory(&rig.editor.widget).Len(); gio != 1 {
		t.Fatalf("Gio holds %d history entries after the bound, want only the reset text", gio)
	}
	if caret, anchor := rig.editor.widget.Selection(); caret != 4 || anchor != 5 {
		t.Fatalf("the reset moved the selection to caret %d, anchor %d; want caret 4, anchor 5", caret, anchor)
	}
	rig.wantShown("hello")

	rig.ime.windowsComposition("ㅎ")
	rig.frame()
	rig.editor.widgetEdits = contractUndoSteps
	rig.frame()
	if gio := gioHistory(&rig.editor.widget).Len(); gio == 1 {
		t.Fatal("Gio history was reset during a composition")
	}
	rig.wantShown("hellㅎ")
	rig.ime.windowsResult("한")
	rig.ime.windowsEndComposition()
	rig.frame()
	rig.wantDraft(textinput.Draft{Text: "hell한", Selection: caretAt(7)})
	rig.wantShown("hell한")
	if gio := gioHistory(&rig.editor.widget).Len(); gio != 1 {
		t.Fatalf("Gio holds %d history entries after the commit, want the deferred reset", gio)
	}
	rig.press("Z", key.ModShortcut)
	rig.frame()
	rig.wantDraft(textinput.Draft{Text: "hello", Selection: textinput.Range{Start: 4, End: 5}})
	rig.wantShown("hello")
}
