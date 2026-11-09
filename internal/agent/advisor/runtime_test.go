package advisor

import (
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/config"
)

func TestRuntime_blockerSteersRunningPrimaryIntoNextRequest(t *testing.T) {
	h := newHarness(t, nil)
	h.write("a.txt", "hello\n")
	primaryGate, advisorGate := make(chan struct{}), make(chan struct{})
	h.model.push(rolePrimary,
		calls(call{id: "r1", name: "read", args: `{"path":"a.txt"}`}),
		gated(text("working"), primaryGate))
	h.model.push(roleAdvisor,
		gated(calls(advise("n1", "stop and run the tests first", "blocker")), advisorGate),
		text("done"))
	thread, _ := h.start()
	h.submit(thread, "fix the bug")
	h.model.waitRequests(rolePrimary, 2)
	h.model.waitRequests(roleAdvisor, 1)
	close(advisorGate)
	h.waitFor("the blocker note", func() bool { return len(notesIn(journalEvents(t, h.store, thread.ID()))) == 1 })
	close(primaryGate)
	h.model.waitRequests(rolePrimary, 3)
	h.waitIdle(thread)

	note := notesIn(journalEvents(t, h.store, thread.ID()))[0]
	if note.Delivery != string(agent.DeliverySteer) || note.Severity != "blocker" || note.Advisor != config.DefaultAdvisorName {
		t.Fatalf("note = %+v, want a blocker steer from advisor", note)
	}
	want := `<advisory advisor="advisor" severity="blocker">stop and run the tests first</advisory>`
	if !slices.ContainsFunc(userTexts(h.model.request(rolePrimary, 2)), func(s string) bool { return strings.Contains(s, want) }) {
		t.Fatalf("third primary request lacks %s: %v", want, userTexts(h.model.request(rolePrimary, 2)))
	}
	if slices.ContainsFunc(userTexts(h.model.request(rolePrimary, 1)), func(s string) bool { return strings.Contains(s, "<advisory") }) {
		t.Fatal("the request that was in flight before the note already carried it")
	}
	for _, info := range h.host.Threads() {
		if info.Advisor != "" {
			t.Fatalf("thread list shows the advisor session %+v", info)
		}
	}
}

func TestRuntime_syncFinalHoldsTurnCompletedUntilReviewEnds(t *testing.T) {
	cases := []struct {
		sync config.AdvisorSync
		hold bool
	}{
		{config.AdvisorSyncOff, false},
		{config.AdvisorSyncFinal, true},
	}
	for _, test := range cases {
		t.Run(string(test.sync), func(t *testing.T) {
			h := newHarness(t, func(cfg *config.Config) {
				cfg.Advisor.Review, cfg.Advisor.Sync = config.AdvisorReviewAgentEnd, test.sync
			})
			gate := make(chan struct{})
			h.model.push(roleAdvisor, gated(text("looks fine"), gate))
			thread, _ := h.start()
			h.submit(thread, "write the report")
			h.model.waitRequests(roleAdvisor, 1)
			completed := func() bool { return len(turnEnds(journalEvents(t, h.store, thread.ID()))) == 1 }
			if !test.hold {
				h.waitFor("turn.completed while the review runs", completed)
				close(gate)
				return
			}
			requireTurnHeldWhileReviewRuns(t, completed)
			close(gate)
			h.waitFor("turn.completed after the review", completed)
			child := journalEvents(t, h.store, childOf(t, h.host, thread.ID()))
			if ends := turnEnds(child); len(ends) != 1 || ends[0].Status != statusDone {
				t.Fatalf("advisor turns = %+v, want one completed review", ends)
			}
		})
	}
}

func TestRuntime_crashDuringReviewRecoversAndDeliversEachNoteOnce(t *testing.T) {
	h := newHarness(t, func(cfg *config.Config) { cfg.Advisor.Review = config.AdvisorReviewAgentEnd })
	hang := make(chan struct{})
	t.Cleanup(func() { close(hang) })
	h.model.push(roleAdvisor,
		calls(advise("b1", "the migration drops a column", "blocker"), advise("c1", "rename the helper", "nit")),
		gated(text("never sent"), hang))
	thread, _ := h.start()
	h.submit(thread, "migrate the schema")
	h.model.waitRequests(roleAdvisor, 2)
	h.waitFor("the blocker to reach the primary", func() bool { return len(notesIn(journalEvents(t, h.store, thread.ID()))) == 1 })
	h.waitIdle(thread)

	crashed := t.TempDir()
	copyRegularFiles(t, h.home, crashed)
	store, host := h.open(crashed)
	primary, err := host.Resume(t.Context(), thread.ID())
	if err != nil {
		t.Fatal(err)
	}
	h.attach(host, primary)
	h.waitFor("the pending nit after recovery", func() bool { return len(notesIn(journalEvents(t, store, thread.ID()))) == 2 })
	h.waitIdle(primary)

	notes := notesIn(journalEvents(t, store, thread.ID()))
	if len(notes) != 2 || notes[0].Note != "the migration drops a column" || notes[1].Note != "rename the helper" {
		t.Fatalf("notes = %+v, want the blocker once then the nit once", notes)
	}
	if notes[0].ReviewSeq != notes[1].ReviewSeq {
		t.Fatalf("review_seq changed across recovery: %d then %d", notes[0].ReviewSeq, notes[1].ReviewSeq)
	}
	child := journalEvents(t, store, childOf(t, host, thread.ID()))
	if ends := turnEnds(child); len(ends) != 1 || ends[0].Status != statusDone {
		t.Fatalf("advisor turns = %+v, want the one recovered review completed", ends)
	}
	if got := len(notesIn(journalEvents(t, store, thread.ID()))); got != 2 {
		t.Fatalf("journal holds %d notes after recovery settled, want 2", got)
	}
}

func TestRuntime_recoveredReviewKeepsBlockerThatDisplacedPendingNit(t *testing.T) {
	h := newHarness(t, func(cfg *config.Config) { cfg.Advisor.Review = config.AdvisorReviewAgentEnd })
	hang := make(chan struct{})
	t.Cleanup(func() { close(hang) })
	h.model.push(roleAdvisor,
		calls(advise("n1", "check the error path", "nit"), advise("b1", "check the error path", "blocker")),
		gated(text("never sent"), hang))
	thread, _ := h.start()
	h.submit(thread, "handle errors")
	h.model.waitRequests(roleAdvisor, 2)
	h.waitFor("the blocker", func() bool { return len(notesIn(journalEvents(t, h.store, thread.ID()))) == 1 })
	h.waitIdle(thread)

	crashed := t.TempDir()
	copyRegularFiles(t, h.home, crashed)
	store, host := h.open(crashed)
	primary, err := host.Resume(t.Context(), thread.ID())
	if err != nil {
		t.Fatal(err)
	}
	rt := h.attach(host, primary)
	childID := childOf(t, host, thread.ID())
	h.waitFor("the recovered review to end", func() bool { return len(turnEnds(journalEvents(t, store, childID))) == 1 })
	waitReviewSettled(h, rt)
	h.waitIdle(primary)
	notes := notesIn(journalEvents(t, store, thread.ID()))
	if len(notes) != 1 || notes[0].Severity != "blocker" {
		t.Fatalf("notes = %+v, want only the blocker; the displaced nit must not be flushed", notes)
	}
}

func TestRuntime_haltsAfterThreeFailedReviewsAndReleasesWaits(t *testing.T) {
	h := newHarness(t, func(cfg *config.Config) {
		cfg.Advisor.Review, cfg.Advisor.Sync = config.AdvisorReviewAgentEnd, config.AdvisorSyncFinal
	})
	h.model.setFallback(roleAdvisor, response{status: 400, body: `{"error":{"message":"bad request"}}`})
	thread, rt := h.start()
	for i := range 3 {
		h.submit(thread, "step "+string(rune('a'+i)))
		h.model.waitRequests(roleAdvisor, i+1)
		h.waitIdle(thread)
	}
	h.waitFor("halted status", func() bool { return advisorState(t, rt) == StateHalted })
	h.submit(thread, "step d")
	h.waitIdle(thread)
	if got := h.model.count(roleAdvisor); got != 3 {
		t.Fatalf("a halted advisor made %d requests, want 3", got)
	}
	rt.Reset()
	if state := advisorState(t, rt); state != StateIdle {
		t.Fatalf("state after reset = %s, want idle", state)
	}
	h.model.setFallback(roleAdvisor, text("ok"))
	h.submit(thread, "step e")
	h.waitIdle(thread)
	if got := h.model.count(roleAdvisor); got != 4 {
		t.Fatalf("advisor requests after reset = %d, want 4", got)
	}
}

func advisorState(t *testing.T, rt *Runtime) State {
	t.Helper()
	statuses, err := rt.Status()
	if err != nil {
		t.Fatal(err)
	}
	return statuses[0].State
}

func TestRuntime_destructiveAdvisorOutputDiscardsTheReview(t *testing.T) {
	cases := []struct {
		name   string
		prompt string
		notes  int
	}{
		{name: "generated directive", prompt: "deploy the app", notes: 0},
		{name: "quoted from the transcript", prompt: "I ran rm -rf /srv/app by mistake", notes: 2},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			h := newHarness(t, func(cfg *config.Config) { cfg.Advisor.Review = config.AdvisorReviewAgentEnd })
			h.model.push(roleAdvisor,
				reply("Run this:\nrm -rf /srv/app", advise("c1", "use a temp dir", "concern"), advise("b1", "stop now", "blocker")),
				text("done"))
			thread, rt := h.start()
			h.submit(thread, test.prompt)
			childID := ""
			h.waitFor("the review to end", func() bool {
				id, ok := h.host.AdvisorChild(thread.ID(), config.DefaultAdvisorName)
				childID = id
				return ok && len(turnEnds(journalEvents(t, h.store, id))) == 1
			})
			waitReviewSettled(h, rt)
			h.waitIdle(thread)
			if got := len(notesIn(journalEvents(t, h.store, thread.ID()))); got != test.notes {
				t.Fatalf("delivered %d notes, want %d", got, test.notes)
			}
			wantBlocker := string(VerdictAccepted)
			if test.notes == 0 {
				wantBlocker = verdictDiscarded
			}
			if got := toolOutputs(journalEvents(t, h.store, childID))["b1"]; got != wantBlocker {
				t.Fatalf("blocker result = %q, want %q", got, wantBlocker)
			}
		})
	}
}

func TestRuntime_intervalReviewsEveryNthBoundaryWithSkippedEvents(t *testing.T) {
	h := newHarness(t, func(cfg *config.Config) {
		cfg.Advisor.Interval, cfg.Advisor.Sync = 2, config.AdvisorSyncStrict
	})
	h.write("a.txt", "a\n")
	h.write("b.txt", "b\n")
	h.model.push(rolePrimary,
		calls(call{id: "r1", name: "read", args: `{"path":"a.txt"}`}),
		calls(call{id: "r2", name: "read", args: `{"path":"b.txt"}`}),
		text("all read"))
	thread, _ := h.start()
	h.submit(thread, "read both files")
	h.waitFor("turn.completed", func() bool { return len(turnEnds(journalEvents(t, h.store, thread.ID()))) == 1 })
	if got := h.model.count(roleAdvisor); got != 1 {
		t.Fatalf("advisor requests = %d, want 1 for three boundaries at interval 2", got)
	}
	input := strings.Join(userTexts(h.model.request(roleAdvisor, 0)), "\n")
	for _, want := range []string{`<tool_result id="r1"`, `<tool_result id="r2"`, "read both files"} {
		if !strings.Contains(input, want) {
			t.Fatalf("review input lacks %s:\n%s", want, input)
		}
	}
	if strings.Contains(input, "all read") {
		t.Fatalf("review at the second boundary saw the final step:\n%s", input)
	}
}

func TestRuntime_advisorRequestUsesRoleModelMediumEffortAndOnlyGrantedTools(t *testing.T) {
	h := newHarness(t, func(cfg *config.Config) {
		cfg.Model, cfg.Advisor.Review, cfg.Advisor.Tools = "openai/gpt-6.1-sol", config.AdvisorReviewAgentEnd, []string{"read"}
	})
	h.model.models = []string{"gpt-6.1-sol", "gpt-6-astra"}
	h.model.push(roleAdvisor, calls(call{id: "x1", name: "exec", args: `{"command":"ls"}`}), text("done"))
	h.write("secret.txt", "mention-body-7731\n")
	thread, _ := h.start()
	h.submit(thread, "ultrathink about the cache @secret.txt")
	h.model.waitRequests(roleAdvisor, 2)
	h.waitIdle(thread)
	primary := h.model.request(rolePrimary, 0)
	if effort(primary) != "max" {
		t.Fatalf("primary effort = %q, want the keyword's max", effort(primary))
	}
	request := h.model.request(roleAdvisor, 0)
	if request["model"] != "gpt-6-astra" {
		t.Fatalf("advisor model = %v, want the slow preset gpt-6-astra", request["model"])
	}
	if effort(request) != "medium" {
		t.Fatalf("advisor effort = %q, want medium", effort(request))
	}
	if names := toolNames(request); !slices.Equal(names, []string{"advise", "read"}) {
		t.Fatalf("advisor tools = %v, want [advise read]", names)
	}
	if advisorInput := strings.Join(userTexts(request), "\n"); strings.Contains(advisorInput, "mention-body-7731") {
		t.Fatalf("a primary @mention attached a file to the advisor review:\n%s", advisorInput)
	}
	childID := childOf(t, h.host, thread.ID())
	if got := toolOutputs(journalEvents(t, h.store, childID))["x1"]; !strings.Contains(got, "Tool exec not found") {
		t.Fatalf("ungranted call result = %q", got)
	}
}

func effort(request map[string]any) string {
	reasoning, _ := request["reasoning"].(map[string]any)
	value, _ := reasoning["effort"].(string)
	return value
}

func toolNames(request map[string]any) []string {
	var out []string
	items, _ := request["tools"].([]any)
	for _, raw := range items {
		if item, ok := raw.(map[string]any); ok {
			name, _ := item["name"].(string)
			out = append(out, name)
		}
	}
	return out
}

func TestRuntime_enableMidSessionSeedsCursorAtHead(t *testing.T) {
	h := newHarness(t, func(cfg *config.Config) { cfg.Advisor.Enabled = false })
	thread, rt := h.start()
	h.submit(thread, "first prompt before enabling")
	h.waitIdle(thread)
	if got := advisorState(t, rt); got != StateDisabled {
		t.Fatalf("state = %s, want disabled", got)
	}
	if err := rt.SetEnabled(true); err != nil {
		t.Fatal(err)
	}
	h.submit(thread, "second prompt after enabling")
	h.model.waitRequests(roleAdvisor, 1)
	input := strings.Join(userTexts(h.model.request(roleAdvisor, 0)), "\n")
	if strings.Contains(input, "first prompt") || !strings.Contains(input, "second prompt") {
		t.Fatalf("review input after enabling:\n%s", input)
	}
	if h.model.count(roleAdvisor) != 1 {
		t.Fatalf("a disabled advisor reviewed: %d requests", h.model.count(roleAdvisor))
	}
}

func TestRuntime_reattachBindsTheAdvisorSessionToTheNewRuntime(t *testing.T) {
	h := newHarness(t, func(cfg *config.Config) {
		cfg.Advisor.Review, cfg.Advisor.Sync, cfg.Advisor.MaxNotes = config.AdvisorReviewAgentEnd, config.AdvisorSyncFinal, 1
	})
	h.model.push(roleAdvisor, calls(advise("a1", "first nit", "nit")), text("done"))
	thread, first := h.start()
	h.submit(thread, "first task")
	h.waitIdle(thread)
	if err := first.Close(); err != nil {
		t.Fatal(err)
	}
	h.attach(h.host, thread)
	h.model.push(roleAdvisor, calls(advise("b1", "second nit", "nit")), text("done"))
	h.submit(thread, "second task")
	h.waitIdle(thread)
	notes := notesIn(journalEvents(t, h.store, thread.ID()))
	if len(notes) != 2 || notes[1].Note != "second nit" {
		t.Fatalf("notes = %+v, want both nits; the re-attached advisor must start a fresh review budget", notes)
	}
}

func TestRuntime_failedRecoveredReviewKeepsItsRangeForTheNextReview(t *testing.T) {
	h := newHarness(t, func(cfg *config.Config) { cfg.Advisor.Review = config.AdvisorReviewAgentEnd })
	hang := make(chan struct{})
	t.Cleanup(func() { close(hang) })
	thread, _ := h.start()
	h.submit(thread, "first prompt reviewed")
	h.model.waitRequests(roleAdvisor, 1)
	childID := ""
	h.waitFor("the first review", func() bool {
		id, ok := h.host.AdvisorChild(thread.ID(), config.DefaultAdvisorName)
		childID = id
		return ok && len(turnEnds(journalEvents(t, h.store, id))) == 1
	})
	h.model.push(roleAdvisor, gated(text("never sent"), hang))
	h.submit(thread, "second prompt before the crash")
	h.model.waitRequests(roleAdvisor, 2)
	h.waitIdle(thread)

	crashed := t.TempDir()
	copyRegularFiles(t, h.home, crashed)
	h.model.setFallback(roleAdvisor, response{status: 400, body: `{"error":{"message":"bad request"}}`})
	store, host := h.open(crashed)
	primary, err := host.Resume(t.Context(), thread.ID())
	if err != nil {
		t.Fatal(err)
	}
	h.attach(host, primary)
	h.waitFor("the recovered review to fail", func() bool {
		ends := turnEnds(journalEvents(t, store, childID))
		return len(ends) == 2 && ends[1].Status != statusDone
	})
	h.model.setFallback(roleAdvisor, text("ok"))
	h.submit(primary, "third prompt after the restart")
	h.model.waitRequests(roleAdvisor, 4)
	texts := userTexts(h.model.request(roleAdvisor, 3))
	input := texts[len(texts)-1]
	if !strings.Contains(input, "second prompt before the crash") || !strings.Contains(input, "third prompt after the restart") {
		t.Fatalf("review after a failed recovery skipped the unreviewed range:\n%s", input)
	}
	if strings.Contains(input, "first prompt reviewed") {
		t.Fatalf("review after a failed recovery repeated the completed range:\n%s", input)
	}
}

func waitReviewSettled(h *harness, rt *Runtime) {
	h.t.Helper()
	h.waitFor("the review to settle", func() bool { return advisorState(h.t, rt) != StateReviewing })
}

func requireTurnHeldWhileReviewRuns(t *testing.T, completed func() bool) {
	t.Helper()
	hold := time.Now().Add(time.Second)
	for time.Now().Before(hold) {
		if completed() {
			t.Fatal("turn.completed was journaled before the review ended")
		}
		time.Sleep(5 * time.Millisecond)
	}
}
