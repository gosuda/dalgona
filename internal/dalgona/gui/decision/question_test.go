//go:build gui

package decision

import (
	"context"
	"slices"
	"strings"
	"testing"
	"time"

	"gioui.org/io/key"
	"gioui.org/io/semantic"

	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const choiceParams = `{"threadId":"t1","turnId":"u1","itemId":"a1","isBlocking":true,"questions":[` +
	`{"id":"q1","header":"Targets","question":"Which targets should the build cover?","isOther":true,"isSecret":false,"multiSelect":true,"options":[` +
	`{"label":"linux &amp; mac","description":"Both desktop targets."},` +
	`{"label":"windows","description":"Windows only.","preview":"GOOS=windows  go build ./..."},` +
	`{"label":"wasm","description":"Browser build."}]},` +
	`{"id":"q2","header":"Mode","question":"Which mode?","isOther":true,"isSecret":false,"multiSelect":false,"options":[` +
	`{"label":"fast","description":"Skip checks."},{"label":"safe","description":"Run every check."}]}]}`

const freeParams = `{"threadId":"t1","turnId":"u1","itemId":"a1","isBlocking":true,"questions":[{"id":"q1","header":"Name","question":"What should the release be called?","isOther":false,"isSecret":false,"multiSelect":false,"options":null}]}`

func questionScene(params string) (scene, state.RequestView) {
	rv := request("ask", methodQuestion, 1, params)
	return sceneWith(thread(rv)), rv
}

func (h *harness) typeInto(field, content string) {
	h.t.Helper()
	h.click(field, semantic.Editor)
	h.frame(h.last)
	h.ime.insert(content)
}

func (h *harness) typed() string {
	return h.dock.cards[h.dock.head].answers[0].text()
}

func TestQuestionCardLabelsItsQuestionsAndRecommendsTheFirstOption(t *testing.T) {
	h := newHarness(t)
	sc, _ := questionScene(choiceParams)
	h.frame(sc)
	h.must("Answer these questions", semantic.Unknown)
	for _, want := range []string{"Question required · on box", "Targets", "Which targets should the build cover?", "Mode", "Which mode?"} {
		if !h.showsExactly(want) {
			t.Fatalf("the card omits %q; labels %q", want, h.labels())
		}
	}
	first := h.must("linux &amp; mac", semantic.CheckBox)
	if !strings.HasPrefix(first.Desc.Description, "Recommended") {
		t.Fatalf("the first option is not marked Recommended: %q", first.Desc.Description)
	}
	if second := h.must("windows", semantic.CheckBox); strings.Contains(second.Desc.Description, "Recommended") {
		t.Fatal("a later option is marked Recommended")
	}
	h.must("fast", semantic.RadioButton)
}

func TestSubmitSendsOriginalLabelsInDeclaredOrderThenTypedText(t *testing.T) {
	h := newHarness(t)
	sc, rv := questionScene(choiceParams)
	h.last = sc
	h.frame(sc)
	h.click("wasm", semantic.CheckBox)
	h.frame(sc)
	h.click("linux &amp; mac", semantic.CheckBox)
	h.frame(sc)
	h.typeInto("Other answer for Targets", "  ios\n")
	h.frame(sc)
	h.click("safe", semantic.RadioButton)
	h.frame(sc)
	h.click("Submit", semantic.Button)
	h.frame(sc)
	got := h.onlyAnswer()
	answers := answersOf(t, got.Result)
	if got.Request != rv.Key {
		t.Fatalf("answered %+v, want %+v", got.Request, rv.Key)
	}
	if !slices.Equal(answers["q1"], []string{"linux &amp; mac", "wasm", "  ios\n"}) || !slices.Equal(answers["q2"], []string{"safe"}) {
		t.Fatalf("submitted %q", answers)
	}
}

func TestSingleSelectionKeepsOneChoice(t *testing.T) {
	h := newHarness(t)
	sc, _ := questionScene(choiceParams)
	h.last = sc
	h.frame(sc)
	h.click("fast", semantic.RadioButton)
	h.frame(sc)
	h.click("safe", semantic.RadioButton)
	h.frame(sc)
	if h.must("fast", semantic.RadioButton).Desc.Selected || !h.must("safe", semantic.RadioButton).Desc.Selected {
		t.Fatal("a single-selection question kept two choices")
	}
	h.typeInto("Other answer for Mode", "balanced")
	h.frame(sc)
	h.click("Submit", semantic.Button)
	h.frame(sc)
	if got := answersOf(t, h.onlyAnswer().Result)["q2"]; !slices.Equal(got, []string{"balanced"}) {
		t.Fatalf("a single-selection question sent %q", got)
	}
}

func TestOverLimitAnswerIsRefusedWholeAndStaysRecoverable(t *testing.T) {
	h := newHarness(t)
	sc, _ := questionScene(freeParams)
	h.last = sc
	h.frame(sc)
	if h.shows("Recommended") {
		t.Fatal("a question without options shows options")
	}
	oversized := " " + strings.Repeat("😀", 1997) + "e\u0301!!"
	h.typeInto("Answer for Name", oversized)
	h.frame(sc)
	if !h.shows("This answer has 2002 characters. Shorten it to 2000 or fewer.") {
		t.Fatalf("no field error for 2002 scalars; labels %q", h.labels())
	}
	if h.typed() != "" {
		t.Fatalf("the refused candidate changed the committed answer to %d scalars", len([]rune(h.typed())))
	}
	h.click("Copy rejected text", semantic.Button)
	h.frame(sc)
	if _, content, ok := h.router.WriteClipboard(); !ok || string(content) != oversized {
		t.Fatal("the refused candidate is not recoverable through the clipboard")
	}
	within := " " + strings.Repeat("😀", 1997) + "e\u0301"
	h.ime.insert(within)
	h.frame(sc)
	if h.shows("This answer has") {
		t.Fatal("an accepted edit kept the refusal")
	}
	h.click("Submit", semantic.Button)
	h.frame(sc)
	if got := answersOf(t, h.onlyAnswer().Result)["q1"]; !slices.Equal(got, []string{within}) {
		t.Fatalf("the accepted answer was altered: %d scalars", len([]rune(strings.Join(got, ""))))
	}
}

func TestCompositionPreeditNeverReachesTheAnswer(t *testing.T) {
	h := newHarness(t)
	sc, _ := questionScene(freeParams)
	h.last = sc
	h.frame(sc)
	h.click("Answer for Name", semantic.Editor)
	h.frame(sc)
	for _, preedit := range []string{"ㅎ", "하", "한"} {
		h.ime.composition(preedit)
		h.frame(sc)
	}
	h.press(key.NameEscape)
	h.press(key.NameReturn)
	h.frame(sc)
	if len(h.submitted) != 0 || h.typed() != "" {
		t.Fatalf("composition keys acted on the card (%v) or preedit was committed (%q)", h.submitted, h.typed())
	}
	h.ime.result("한")
	h.frame(sc)
	h.ime.composition("ㄱ")
	h.frame(sc)
	h.click("Submit", semantic.Button)
	h.frame(sc)
	if got := answersOf(t, h.onlyAnswer().Result)["q1"]; !slices.Equal(got, []string{"한"}) {
		t.Fatalf("submitted %q, want only the committed syllable", got)
	}
}

func TestFieldEnterSubmitsAndEscapeDismisses(t *testing.T) {
	for name, want := range map[key.Name][]string{key.NameReturn: {"x"}, key.NameEscape: nil} {
		h := newHarness(t)
		sc, _ := questionScene(freeParams)
		h.last = sc
		h.frame(sc)
		h.typeInto("Answer for Name", "x")
		h.frame(sc)
		h.press(name)
		h.frame(sc)
		got := answersOf(t, h.onlyAnswer().Result)
		if want == nil && len(got) != 0 || want != nil && !slices.Equal(got["q1"], want) {
			t.Fatalf("%s in the field sent %v", name, got)
		}
	}
}

func TestSkipAndDismissSendTheirDistinctShapes(t *testing.T) {
	h := newHarness(t)
	sc, _ := questionScene(choiceParams)
	h.frame(sc)
	h.click("windows", semantic.CheckBox)
	h.frame(sc)
	h.click("Skip", semantic.Button)
	h.frame(sc)
	got := answersOf(t, h.onlyAnswer().Result)
	if len(got) != 2 || len(got["q1"]) != 0 || len(got["q2"]) != 0 {
		t.Fatalf("Skip sent %v, want every question with an empty list", got)
	}
	h = newHarness(t)
	h.frame(sc)
	h.frame(sc)
	h.press(key.NameEscape)
	h.frame(sc)
	if got := answersOf(t, h.onlyAnswer().Result); len(got) != 0 {
		t.Fatalf("Escape sent %v, want {\"answers\": {}}", got)
	}
}

func TestPreviewOfTheChosenOptionAppears(t *testing.T) {
	h := newHarness(t)
	sc, _ := questionScene(choiceParams)
	h.frame(sc)
	if h.showsExactly("GOOS=windows  go build ./...") {
		t.Fatal("a preview shows before its option is chosen")
	}
	h.click("windows", semantic.CheckBox)
	h.frame(sc)
	if !h.showsExactly("GOOS=windows  go build ./...") {
		t.Fatalf("the chosen option's preview is missing; labels %q", h.labels())
	}
}

func TestNulAnswerIsRefusedWhole(t *testing.T) {
	h := newHarness(t)
	sc, _ := questionScene(freeParams)
	h.last = sc
	h.frame(sc)
	h.typeInto("Answer for Name", "v1\x00")
	h.frame(sc)
	if !h.shows("NUL character") || h.typed() != "" {
		t.Fatalf("a NUL answer was not refused whole; labels %q", h.labels())
	}
}

func TestCorrectionWarningReopensTheSameRequestOnce(t *testing.T) {
	h := newHarness(t)
	sc, rv := questionScene(freeParams)
	h.last = sc
	h.frame(sc)
	h.typeInto("Answer for Name", "v1")
	h.frame(sc)
	h.click("Submit", semantic.Button)
	h.frame(sc)
	h.click("Submit", semantic.Button)
	h.frame(sc)
	if len(h.answers()) != 1 || !h.shows("Answer sent") {
		t.Fatalf("submitting twice before a warning sent %d answers", len(h.answers()))
	}

	warned := rv
	warned.Warning = &state.AnswerWarning{Message: "invalid text: answers.q1.answers[0]: text_too_large", Reason: "text_too_large", Field: "answers.q1.answers[0]"}
	sc = sceneWith(thread(warned))
	h.last = sc
	h.frame(sc)
	if !h.shows("dalgona refused this answer because it has more than 2000 characters") {
		t.Fatalf("the correction does not mark the field; labels %q", h.labels())
	}
	if h.must("Submit", semantic.Button).Desc.Disabled {
		t.Fatal("the correction did not restore editing")
	}
	h.click("Submit", semantic.Button)
	h.frame(sc)
	h.click("Submit", semantic.Button)
	h.frame(sc)
	got := h.answers()
	if len(got) != 2 || got[1].Request != rv.Key {
		t.Fatalf("the correction sent %d answers; want one more for %+v", len(got), rv.Key)
	}
	if answers := answersOf(t, got[1].Result)["q1"]; !slices.Equal(answers, []string{"v1"}) {
		t.Fatalf("the corrected answer lost its text: %q", answers)
	}
}

func TestAnswersStayWhileTheSubmissionAwaitsResolution(t *testing.T) {
	h := newHarness(t)
	sc, rv := questionScene(choiceParams)
	h.frame(sc)
	h.click("windows", semantic.CheckBox)
	h.frame(sc)
	h.click("Submit", semantic.Button)
	h.frame(sc)
	answered := rv
	answered.Answered, answered.Actionable = true, false
	sc = sceneWith(thread(answered))
	h.frame(sc)
	n := h.must("windows", semantic.CheckBox)
	if !n.Desc.Selected || !n.Desc.Disabled {
		t.Fatalf("the submitted choice is not kept read-only: %+v", n.Desc)
	}
	h.click("wasm", semantic.CheckBox)
	h.frame(sc)
	if h.must("wasm", semantic.CheckBox).Desc.Selected {
		t.Fatal("a pending submission accepted another edit")
	}
}

type pastedText string

func (p pastedText) ReadClipboard(context.Context, platform.PasteKind) (platform.ClipboardContent, error) {
	return platform.ClipboardContent{Text: string(p)}, nil
}

func TestAnswerFieldPastesThroughTheWindowReaderThatAppearedLater(t *testing.T) {
	h := newHarness(t)
	var reader platform.ClipboardReader
	h.clipboard = func() platform.ClipboardReader { return reader }
	sc, _ := questionScene(freeParams)
	h.last = sc
	h.frame(sc)
	h.click("Answer for Name", semantic.Editor)
	h.frame(sc)
	reader = pastedText("v1.4 Hazel")
	h.router.Queue(key.Event{Name: "V", Modifiers: key.ModShortcut, State: key.Press})
	h.frame(sc)
	select {
	case <-h.woke:
	case <-time.After(5 * time.Second):
		t.Fatal("the window clipboard reader was never asked")
	}
	h.frame(sc)
	if h.typed() != "v1.4 Hazel" {
		t.Fatalf("the answer field holds %q after the paste", h.typed())
	}
}
