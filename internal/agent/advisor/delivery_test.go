package advisor

import (
	"strconv"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
)

type deliveryCase struct {
	severity Severity
	inside   bool
	want     agent.Delivery
}

func TestDeliver_matrixAgainstLivePrimaryStates(t *testing.T) {
	h := newHarness(t, func(cfg *config.Config) {
		cfg.Advisor.Enabled, cfg.Advisor.ContinueSeverity, cfg.Advisor.ImmuneTurns = false, "concern", 3
	})
	const (
		steer  = agent.DeliverySteer
		aside  = agent.DeliveryAside
		resume = agent.DeliveryContinue
	)
	table := map[Phase][]deliveryCase{
		PhaseRunning: {
			{SeverityNit, false, aside},
			{SeverityConcern, false, steer},
			{SeverityBlocker, false, steer},
			{SeverityNit, true, aside},
			{SeverityConcern, true, aside},
			{SeverityBlocker, true, steer},
		},
		PhaseIdle: {
			{SeverityNit, false, aside},
			{SeverityConcern, false, resume},
			{SeverityBlocker, false, resume},
			{SeverityNit, true, aside},
			{SeverityConcern, true, aside},
			{SeverityBlocker, true, resume},
		},
		PhaseInterrupted: {
			{SeverityNit, false, aside},
			{SeverityConcern, false, aside},
			{SeverityBlocker, false, aside},
			{SeverityNit, true, aside},
			{SeverityConcern, true, aside},
			{SeverityBlocker, true, aside},
		},
	}
	gate := make(chan struct{})
	defer close(gate)

	running, runningRT := h.start()
	h.model.push(rolePrimary, gated(text("slow"), gate))
	h.submit(running, "long task")
	h.model.waitRequests(rolePrimary, 1)
	for i, test := range table[PhaseRunning] {
		checkDelivery(t, h, runningRT, i, test)
	}

	interrupted, interruptedRT := h.start()
	h.model.push(rolePrimary, gated(text("slow"), gate))
	h.submit(interrupted, "another task")
	h.model.waitRequests(rolePrimary, 2)
	interrupted.Interrupt()
	h.waitIdle(interrupted)
	for i, test := range table[PhaseInterrupted] {
		checkDelivery(t, h, interruptedRT, i, test)
	}

	for i, test := range table[PhaseIdle] {
		_, idleRT := h.start()
		checkDelivery(t, h, idleRT, i, test)
	}
}

func checkDelivery(t *testing.T, h *harness, rt *Runtime, i int, test deliveryCase) {
	t.Helper()
	rt.mu.Lock()
	rt.steered, rt.sinceSteer = true, 3
	if test.inside {
		rt.sinceSteer = 2
	}
	rt.mu.Unlock()
	a := rt.advisors[0]
	text := "note " + strconv.Itoa(i) + " " + test.severity.String()
	if err := a.deliver(t.Context(), pendingNote{key: Normalize(text), text: text, note: text, severity: test.severity}); err != nil {
		t.Fatal(err)
	}
	notes := notesIn(journalEvents(t, h.store, rt.primary.ID()))
	got := notes[len(notes)-1]
	if got.Note != text || agent.Delivery(got.Delivery) != test.want {
		t.Errorf("%s inside=%v: journaled %+v, want delivery %s", test.severity, test.inside, got, test.want)
	}
}

func TestDeliver_immuneWindowCountsBoundariesAfterSteer(t *testing.T) {
	h := newHarness(t, func(cfg *config.Config) { cfg.Advisor.Enabled, cfg.Advisor.ImmuneTurns = false, 2 })
	gate := make(chan struct{})
	defer close(gate)
	thread, rt := h.start()
	h.model.push(rolePrimary, gated(text("slow"), gate))
	h.submit(thread, "long task")
	h.model.waitRequests(rolePrimary, 1)
	a := rt.advisors[0]
	want := []agent.Delivery{agent.DeliverySteer, agent.DeliveryAside, agent.DeliveryAside, agent.DeliverySteer}
	for i, expected := range want {
		if i >= 2 {
			rt.boundary(agent.Boundary{Thread: thread.ID(), Kind: agent.BoundaryTurn})
		}
		text := "concern " + strconv.Itoa(i)
		if err := a.deliver(t.Context(), pendingNote{key: Normalize(text), text: text, note: text, severity: SeverityConcern}); err != nil {
			t.Fatal(err)
		}
		notes := notesIn(journalEvents(t, h.store, thread.ID()))
		if got := agent.Delivery(notes[len(notes)-1].Delivery); got != expected {
			t.Fatalf("concern %d delivered as %s, want %s", i, got, expected)
		}
	}
}

func TestDeliver_oneContinuationPerBatch(t *testing.T) {
	h := newHarness(t, func(cfg *config.Config) { cfg.Advisor.Enabled = false })
	thread, rt := h.start()
	a := rt.advisors[0]
	for _, text := range []string{"first blocker", "second blocker"} {
		if err := a.deliver(t.Context(), pendingNote{key: Normalize(text), text: text, note: text, severity: SeverityBlocker}); err != nil {
			t.Fatal(err)
		}
	}
	h.waitIdle(thread)
	notes := notesIn(journalEvents(t, h.store, thread.ID()))
	if len(notes) != 2 || notes[0].Delivery != string(agent.DeliveryContinue) || notes[1].Delivery != string(agent.DeliveryAside) {
		t.Fatalf("notes = %+v, want continue then aside", notes)
	}
}

func adviceEvents(outputs map[string]string, notes ...call) []event.Seqd {
	toolCalls := make([]event.ToolCall, 0, len(notes))
	for _, item := range notes {
		toolCalls = append(toolCalls, event.ToolCall{ID: item.id, Name: item.name, Args: []byte(item.args)})
	}
	events := []event.Seqd{{Seq: 1, Event: event.AssistantMessage{ToolCalls: toolCalls}}}
	for i, item := range notes {
		events = append(events, event.Seqd{Seq: uint64(i + 2), Event: event.ToolCompleted{CallID: item.id, OK: true, Output: outputs[item.id]}})
	}
	return events
}

func TestPendingNotes_followsJournaledVerdictsAndDisplacement(t *testing.T) {
	accepted := string(VerdictAccepted)
	cases := []struct {
		name    string
		max     int
		calls   []call
		outputs map[string]string
		want    []string
	}{
		{
			name:    "budget and duplicate results are not queued",
			max:     2,
			calls:   []call{advise("1", "alpha", "nit"), advise("2", "beta", "nit"), advise("3", "gamma", "nit"), advise("4", "Alpha!", "nit")},
			outputs: map[string]string{"1": accepted, "2": accepted, "3": "budget", "4": "duplicate"},
			want:    []string{"nit:alpha", "nit:beta"},
		},
		{
			name:    "higher severity displaces the first lowest pending note",
			max:     2,
			calls:   []call{advise("1", "alpha", "nit"), advise("2", "beta", "nit"), advise("3", "gamma", "concern")},
			outputs: map[string]string{"1": accepted, "2": accepted, "3": accepted},
			want:    []string{"nit:beta", "concern:gamma"},
		},
		{
			name:    "escalation replaces the pending note",
			max:     3,
			calls:   []call{advise("1", "alpha", "nit"), advise("2", "alpha", "concern")},
			outputs: map[string]string{"1": accepted, "2": accepted},
			want:    []string{"concern:alpha"},
		},
		{
			name:    "a blocker re-raise removes the pending note",
			max:     3,
			calls:   []call{advise("1", "alpha", "nit"), advise("2", "beta", "nit"), advise("3", "alpha", "blocker")},
			outputs: map[string]string{"1": accepted, "2": accepted, "3": accepted},
			want:    []string{"nit:beta"},
		},
		{
			name:    "discarded, failed, and other tool results are ignored",
			max:     3,
			calls:   []call{advise("1", "alpha", "blocker"), advise("2", "beta", "nit"), {id: "3", name: "read", args: `{"path":"x"}`}},
			outputs: map[string]string{"1": verdictDiscarded, "2": "advise: severity must be one of nit, concern, blocker", "3": accepted},
			want:    nil,
		},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			var got []string
			for _, note := range pendingNotes(adviceEvents(test.outputs, test.calls...), test.max) {
				got = append(got, note.severity.String()+":"+note.text)
			}
			if strings.Join(got, ",") != strings.Join(test.want, ",") {
				t.Fatalf("pending = %v, want %v", got, test.want)
			}
		})
	}
}

func TestUnsafeOutput_rules(t *testing.T) {
	cases := []struct {
		name   string
		output string
		input  string
		unsafe bool
	}{
		{"plain advice", "Check the error path before you continue.", "", false},
		{"bare destructive line", "Do this:\nrm -rf /srv/app", "", true},
		{"prompt prefixed line", "$ git push --force origin main", "", true},
		{"inline code span", "Clean up with `rm -rf build` first.", "", true},
		{"publish directive", "npm publish", "", true},
		{"quoted from transcript", "You ran `rm -rf build`; that deleted the cache.", "<user>\nrm -rf build\n</user>", false},
		{"escaped transcript still matches", "git push --force &&", "<tool_call>\ngit push --force &amp;&amp; ok\n</tool_call>", false},
		{"prose mentioning rm", "Do not use rm -rf here.", "", false},
	}
	for _, test := range cases {
		events := []event.Seqd{{Seq: 1, Event: event.AssistantMessage{Text: test.output}}}
		if got := unsafeOutput(events, test.input); got != test.unsafe {
			t.Errorf("%s: unsafe = %v, want %v", test.name, got, test.unsafe)
		}
	}
	blob := []event.Seqd{{Seq: 1, Event: event.AssistantMessage{TextBlob: &event.BlobRef{Blob: "x", Bytes: 1}}}}
	if !unsafeOutput(blob, "") {
		t.Error("output stored as a blob was not treated as unsafe")
	}
}

func TestBoundedRender_keepsNewestEventsWithinBudget(t *testing.T) {
	events := make([]event.Seqd, 0, 40)
	for i := range 40 {
		events = append(events, event.Seqd{Seq: uint64(i + 1), Event: event.AssistantMessage{Text: strings.Repeat("x", 3000) + strconv.Itoa(i)}})
	}
	full := Render(events)
	if got := boundedRender(events[:3]); got != Render(events[:3]) {
		t.Fatalf("small input changed: %q", got)
	}
	got := boundedRender(events)
	if len(got) > inputBudget+64 || len(full) <= inputBudget {
		t.Fatalf("bounded length %d, full %d, budget %d", len(got), len(full), inputBudget)
	}
	if !strings.HasSuffix(got, Render(events[39:])) || !strings.HasPrefix(got, "[") || !strings.Contains(got, "earlier events omitted]") {
		t.Fatalf("bounded render lost the newest event or the omission marker:\n%.200s", got)
	}
}

func TestBoundedRender_clipsOneOversizedElementKeepingItsTags(t *testing.T) {
	huge := []event.Seqd{{Seq: 1, Event: event.AssistantMessage{Text: "head " + strings.Repeat("é", inputBudget) + " tail"}}}
	clipped := boundedRender(huge)
	if len(clipped) > inputBudget || !strings.HasPrefix(clipped, "<assistant>\nhead") || !strings.HasSuffix(clipped, "tail\n</assistant>\n") || !strings.Contains(clipped, "bytes omitted]") {
		t.Fatalf("clipped element lost its tags or exceeds the budget: %d bytes", len(clipped))
	}
	if len(reviewPrompt(1<<40, clipped)) > event.MaxInlineBytes {
		t.Fatal("review prompt exceeds the inline journal limit")
	}
}

func TestReviewSeq_roundTrip(t *testing.T) {
	for _, seq := range []uint64{0, 7, 1 << 40} {
		got, ok := parseReviewSeq(reviewPrompt(seq, "<user>\nhi\n</user>\n"))
		if !ok || got != seq {
			t.Fatalf("parseReviewSeq = %d, %v, want %d", got, ok, seq)
		}
	}
	if _, ok := parseReviewSeq("plain prompt"); ok {
		t.Fatal("plain prompt parsed as a review")
	}
}

func TestGuard_withdrawRestoresThePreviousAdmission(t *testing.T) {
	guard := NewGuard(3)
	nit := guard.Admit("check the cache", SeverityNit)
	blocker := guard.Admit("check the cache", SeverityBlocker)
	guard.Withdraw(blocker)
	if again := guard.Admit("check the cache", SeverityBlocker); again.Verdict != VerdictAccepted {
		t.Fatalf("blocker after withdraw = %s, want accepted", again.Verdict)
	}
	guard.Withdraw(nit)
	if again := guard.Admit("check the cache", SeverityNit); again.Verdict != VerdictAccepted {
		t.Fatalf("nit after withdrawing the first admission = %s, want accepted", again.Verdict)
	}
	fresh := NewGuard(3)
	fresh.Withdraw(fresh.Admit("x", SeverityConcern))
	if dup := fresh.Admit("x", SeverityNit); dup.Verdict != VerdictAccepted {
		t.Fatalf("withdrawn concern still blocks a nit: %s", dup.Verdict)
	}
}
