//go:build gui

package shell

import (
	"image"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

func bandSnapshot(hide []string, efforts []string, usage *rpc.ThreadTokenUsage) (state.Snapshot, state.ThreadKey) {
	key := state.ThreadKey{Endpoint: local, Thread: "thread-0"}
	ep := endpoint(local, "local", client.PhaseConnected)
	ep.StatusHide = hide
	var options []rpc.ReasoningEffortOption
	for _, e := range efforts {
		options = append(options, rpc.ReasoningEffortOption{ReasoningEffort: e})
	}
	ep.Models = []rpc.Model{{ID: "gpt-test", Model: "gpt-test", DefaultReasoningEffort: "medium", SupportedReasoningEfforts: options}}
	tv := openThread(key, "Thread 0")
	tv.Model, tv.Approval, tv.Sandbox, tv.Usage = "gpt-test", "ask", "off", usage
	tv.Thread.Cwd = "/home/u/work/dalgona"
	snap := state.Snapshot{
		Endpoints: []state.EndpointView{ep},
		Lanes:     map[client.EndpointID]state.LaneView{local: loaded(summary("thread-0", "Thread 0", ""))},
		Threads:   []*state.ThreadView{tv},
		FollowUps: map[state.DraftKey][]state.FollowUp{{Endpoint: local, Window: testWindow, Thread: "thread-0"}: {{ID: 1}, {ID: 2}}},
	}
	return snap, key
}

func openBand(t *testing.T, snap state.Snapshot, size image.Point) (*harness, *state.View) {
	h := newHarness(t)
	h.size = size
	v := snap.View()
	h.settle(v)
	h.click(v, "Thread 0")
	h.settle(v)
	return h, v
}

func TestBandShowsEveryRegisteredValueWithTextNames(t *testing.T) {
	zero := 0
	usage := &rpc.ThreadTokenUsage{ModelContextWindow: 1000, Last: rpc.TokenUsageBreakdown{InputTokens: 900}, EstimatedContextTokens: &zero}
	snap, _ := bandSnapshot(nil, []string{"low", "medium", "high"}, usage)
	h, _ := openBand(t, snap, image.Pt(1600, 800))
	for _, want := range []string{"gpt-test", "Effort medium", "Approval ask", "Sandbox off", "dalgona", "2 follow-ups queued", "Context 0% used", markDescription} {
		if _, ok := h.button(want); !ok {
			t.Errorf("band lacks %q: %v", want, h.labels())
		}
	}
}

func TestModelStaysWhenHiddenAndHiddenItemsLeave(t *testing.T) {
	snap, _ := bandSnapshot([]string{"model", "effort", "queue"}, []string{"low", "high"}, nil)
	h, _ := openBand(t, snap, image.Pt(1600, 800))
	if _, ok := h.button("gpt-test"); !ok {
		t.Error("statusHide removed the model")
	}
	for _, gone := range []string{"Effort medium", "2 follow-ups queued"} {
		if _, ok := h.button(gone); ok {
			t.Errorf("hidden item %q still shows", gone)
		}
	}
}

func TestEffortNeedsTwoLevelsAndUsesItsRank(t *testing.T) {
	if _, ok := effortItem([]rpc.Model{{ID: "m", SupportedReasoningEfforts: []rpc.ReasoningEffortOption{{ReasoningEffort: "high"}}}}, "m", "high"); ok {
		t.Error("a single effort level still shows")
	}
	levels := []rpc.ReasoningEffortOption{{ReasoningEffort: "low"}, {ReasoningEffort: "medium"}, {ReasoningEffort: "high"}}
	want := map[string]string{"low": effortGlyphs[0], "medium": effortGlyphs[2], "high": effortGlyphs[3]}
	for effort, glyph := range want {
		it, ok := effortItem([]rpc.Model{{ID: "m", SupportedReasoningEfforts: levels}}, "m", effort)
		if !ok || it.glyph != glyph {
			t.Errorf("effort %q glyph %q, want %q", effort, it.glyph, glyph)
		}
	}
	it, _ := effortItem([]rpc.Model{{ID: "m", DefaultReasoningEffort: "low", SupportedReasoningEfforts: levels}}, "m", "")
	if it.label != "Effort low" {
		t.Errorf("unset thread effort shows %q, want the model default", it.label)
	}
}

func TestNarrowBandDropsFolderAndQueueButKeepsTheRest(t *testing.T) {
	usage := &rpc.ThreadTokenUsage{ModelContextWindow: 1000, Last: rpc.TokenUsageBreakdown{InputTokens: 500}}
	snap, _ := bandSnapshot(nil, []string{"low", "medium"}, usage)
	h, _ := openBand(t, snap, image.Pt(400, 900))
	for _, gone := range []string{"dalgona", "2 follow-ups queued"} {
		if _, ok := h.button(gone); ok {
			t.Errorf("%q survives a 400 dp band", gone)
		}
	}
	for _, kept := range []string{"gpt-test", "Approval ask", "Sandbox off", "Context 50% used"} {
		if _, ok := h.button(kept); !ok {
			t.Errorf("%q dropped from a 400 dp band: %v", kept, h.labels())
		}
	}
}

func TestBandActivationOpensModelPanelOrDetails(t *testing.T) {
	snap, key := bandSnapshot(nil, []string{"low", "medium"}, nil)
	h, v := openBand(t, snap, image.Pt(1600, 800))
	h.click(v, "gpt-test")
	if got := h.panels["model"].opened; len(got) != 1 || got[0].Thread != key {
		t.Fatalf("model activation opened %v", got)
	}
	h.click(v, "Close model")
	h.settle(v)
	h.click(v, "Approval ask")
	for _, want := range []string{"approval ask", "sandbox off", "folder /home/u/work/dalgona", "2 follow-ups queued"} {
		if !h.has(want) {
			t.Errorf("details lack %q: %v", want, h.labels())
		}
	}
	h.click(v, closeLabel)
	if h.has("approval ask") {
		t.Error("details stayed open after Close")
	}
}

func TestContextGaugeNeedsAKnownWindow(t *testing.T) {
	if _, ok := contextItem(&rpc.ThreadTokenUsage{Last: rpc.TokenUsageBreakdown{InputTokens: 10}}); ok {
		t.Error("context shows without a context window")
	}
	full, _ := contextItem(&rpc.ThreadTokenUsage{ModelContextWindow: 100, Last: rpc.TokenUsageBreakdown{InputTokens: 300}})
	if full.label != "Context 100% used" || full.glyph != string(rune(moonFirst+moonSteps)) {
		t.Errorf("overfull context = %q %U", full.label, []rune(full.glyph))
	}
}
