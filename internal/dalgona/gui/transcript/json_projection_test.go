//go:build gui

package transcript

import (
	"encoding/json/jsontext"
	"errors"
	"image"
	"strings"
	"testing"

	"gioui.org/io/key"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

func TestJSONIndentPreflightMatchesFormatter(t *testing.T) {
	for _, sample := range []string{fittingArguments, ` [ {}, [], [true, {"x": -3.7e+4}], "x" ] `, " \t null ", "\"\xff\""} {
		raw := []byte(sample)
		size, valid, err := indentedJSONSize(raw, residentBudget)
		value := jsontext.Value([]byte(sample))
		if formatErr := value.Indent(jsontext.WithIndent(argumentIndent)); formatErr != nil {
			t.Fatal(formatErr)
		}
		if err != nil || !valid || size != len(value) {
			t.Fatalf("preflight size = %d, valid %t, error %v; formatter size %d", size, valid, err, len(value))
		}
	}
}

const (
	fittingArguments = `{"first":[1,{}],"s":"\u0061","first":true}`
	wantedArguments  = `{
  "first": [
    1,
    {}
  ],
  "s": "\u0061",
  "first": true
}` + "\ntool result"
)

func TestVisibleToolArgumentCopyEnforcesProjectionBudget(t *testing.T) {
	for _, tc := range argumentCases() {
		t.Run(tc.name, func(t *testing.T) {
			withTranscriptAt(t, "high", toolArgumentTurn(tc.raw), func(t *testing.T, h *harness) {
				h.click("Arguments")
				checkVisibleArgumentProjection(t, h, tc.oversize)
				startArgumentCopy(t, h)
				checkArgumentClipboard(t, h, wantedArguments, tc.oversize)
				if tc.oversize {
					h.click("Copy code")
					assertActionClipboard(t, h, "tool result")
				}
			})
		})
	}
}

func TestColdToolArgumentCopyEnforcesProjectionBudget(t *testing.T) {
	for _, tc := range argumentCases() {
		t.Run(tc.name, func(t *testing.T) {
			withTranscriptAt(t, "high", mixedArgumentTurn(tc.raw), func(t *testing.T, h *harness) {
				defer h.tr.Close()
				makeArgumentsCold(t, h)
				startArgumentCopy(t, h)
				waitForMixedCopy(t, h)
				checkArgumentClipboard(t, h, wantedArguments+"\n\n"+strings.Repeat("filler\n\n", 8)+"spill end", tc.oversize)
			})
		})
	}
}

func TestFullTextRefusedArgumentsKeepsExactRawCopy(t *testing.T) {
	raw := deepArguments()
	withTranscriptAt(t, "high", toolArgumentTurn(raw), func(t *testing.T, h *harness) {
		defer h.tr.Close()
		h.click("Arguments")
		checkVisibleArgumentProjection(t, h, true)
		startArgumentCopy(t, h)
		checkArgumentClipboard(t, h, "", true)
		h.click("Open full text")
		focusRefusedArgumentCopy(t, h)
		h.router.Queue(key.Event{Name: key.NameReturn, State: key.Press}, key.Event{Name: key.NameReturn, State: key.Release})
		h.frame()
		assertActionClipboard(t, h, raw)
	})
}

func focusRefusedArgumentCopy(t *testing.T, h *harness) {
	t.Helper()
	f := waitLoadedArgumentsField(t, h)
	row := checkRefusedArgumentsBudget(t, h, f)
	focusRawArgumentsCopy(t, h, row)
}

func waitLoadedArgumentsField(t *testing.T, h *harness) *viewField {
	t.Helper()
	for range 20 {
		h.frame()
		if r := h.tr.review; r.active() && len(r.fields) > 0 && r.fields[0].complete() {
			return r.fields[0]
		}
	}
	t.Fatal("Open full text did not load its Arguments field")
	return nil
}

func checkRefusedArgumentsBudget(t *testing.T, h *harness, f *viewField) *row {
	t.Helper()
	row := h.tr.review.pane.rows[f.id]
	if f.ref.Field != "arguments" || row == nil || !errors.Is(row.sheet.projectionErr, errSourceBudget) {
		t.Fatal("the modal did not refuse its own Arguments projection")
	}
	return row
}

func focusRawArgumentsCopy(t *testing.T, h *harness, row *row) {
	t.Helper()
	tag := &row.sourceActions.rawCode
	for range len(h.tr.review.tags) {
		h.keyPress(key.NameTab, 0)
		if h.router.Source().Focused(tag) {
			return
		}
	}
	t.Fatal("the raw Arguments Copy code footer action is outside the modal Tab order")
}

type argumentCase struct {
	name, raw string
	oversize  bool
}

func deepArguments() string {
	const depth = 4300
	return strings.Repeat("[", depth) + "0" + strings.Repeat("]", depth)
}

func argumentCases() []argumentCase {
	return []argumentCase{
		{name: "fits", raw: fittingArguments},
		{name: "expanded depth exceeds budget", raw: deepArguments(), oversize: true},
	}
}

func toolArgumentTurn(raw string) func(*daemon) []rpc.HistoryTurn {
	return func(d *daemon) []rpc.HistoryTurn {
		return []rpc.HistoryTurn{frozenTurn("tool", rpc.StatusCompleted,
			d.header("call", rpc.KindMCP, nil,
				fieldData{name: "arguments", data: raw, json: true},
				fieldData{name: "result", data: `{"content":[{"text":"tool result"}]}`, json: true}),
		)}
	}
}

func mixedArgumentTurn(raw string) func(*daemon) []rpc.HistoryTurn {
	return func(d *daemon) []rpc.HistoryTurn {
		turns := toolArgumentTurn(raw)(d)
		items := make([]rpc.ItemHeader, 0, 9)
		for i := range 8 {
			items = append(items, d.header("filler"+string('a'+rune(i)), rpc.KindAgent, nil, fieldData{name: "text", data: "filler"}))
		}
		items = append(items, d.header("spill", rpc.KindCommand, nil, fieldData{name: fieldSpill, data: "spill end"}))
		return append(turns, frozenTurn("last", rpc.StatusCompleted, items...))
	}
}

func expandedArgumentRow(t *testing.T, h *harness) *row {
	t.Helper()
	p := h.tr.panes[h.key]
	r := p.rows[identity(h.key, "tool", "call")]
	for range 20 {
		h.frame()
		if p.expanded[r.id] && r.doc != nil && !r.sheet.wait && r.sheet.failed == nil {
			return r
		}
	}
	t.Fatalf("expanded arguments never settled: error %v, loading %v", r.sheet.projectionErr, r.sheet.wait)
	return nil
}

func checkVisibleArgumentProjection(t *testing.T, h *harness, oversize bool) {
	t.Helper()
	r := expandedArgumentRow(t, h)
	if !oversize {
		if r.sheet.projectionErr != nil || r.doc.Source() != wantedArguments {
			t.Fatalf("fitting visible arguments projection: error %v, source bytes %d", r.sheet.projectionErr, len(r.doc.Source()))
		}
		return
	}
	if !errors.Is(r.sheet.projectionErr, errSourceBudget) || r.doc.Source() != "tool result" {
		t.Fatalf("oversize visible arguments: error %v, retained %d bytes", r.sheet.projectionErr, len(r.doc.Source()))
	}
	h.expect("Content could not be displayed: "+errSourceBudget.Error(), "Open full text")
}

func makeArgumentsCold(t *testing.T, h *harness) {
	t.Helper()
	h.size = image.Pt(800, 120)
	h.frame()
	p := h.tr.panes[h.key]
	p.vp.JumpToLatest()
	h.frame()
	r := p.rows[identity(h.key, "tool", "call")]
	if h.tr.sourceWanted(p, r) || r.doc == nil {
		t.Fatal("the tool fixture must be cached but outside the viewport")
	}
	p.discardSource(r, h.tr.loader)
	p.expanded[r.id] = true
	h.frame()
	if !r.sourceEvicted || !p.expanded[r.id] {
		t.Fatal("rebuilding the pane did not keep the expanded tool cold")
	}
}

func startArgumentCopy(t *testing.T, h *harness) {
	t.Helper()
	if err := platform.WriteText(h.router.Source(), "previous clipboard"); err != nil {
		t.Fatal(err)
	}
	h.frame()
	_, data, ok := h.router.WriteClipboard()
	if !ok || string(data) != "previous clipboard" {
		t.Fatal("the fixture did not first publish its previous clipboard value")
	}
	h.router.Source().Execute(key.FocusCmd{Tag: h.tr.FocusTag()})
	h.keyPress("A", key.ModShortcut)
	h.keyPress("C", key.ModShortcut)
}

func checkArgumentClipboard(t *testing.T, h *harness, want string, oversize bool) {
	t.Helper()
	_, copied, ok := h.router.WriteClipboard()
	if oversize {
		if ok || h.tr.notice != "Copy failed: "+errSourceBudget.Error() {
			t.Fatalf("oversize copy published %d bytes or missed projection budget: notice %q", len(copied), h.tr.notice)
		}
		return
	}
	if !ok || string(copied) != want {
		t.Fatalf("copy published %d bytes, want %d exact projected bytes: notice %q", len(copied), len(want), h.tr.notice)
	}
}
