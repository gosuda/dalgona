package agent

import (
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"fmt"
	"regexp"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/model"
	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dal/tool/patch"
	"github.com/gosuda/dalgona/internal/dal/tool/read"
)

var refPlaceholder = regexp.MustCompile(`\{ref:([^}]+)\}`)

const (
	unknownReference = "r00000000000000000000000000000001.1"
	threeLines       = "one\ntwo\nthree\n"
	createCall       = `{"input":"[n.txt#NEW]\nPUT >$:\n+body\n"}`
	editTwo          = "[a.txt@{ref:a.txt}]\nPUT 2:\n+TWO\n"
	unknownEdit      = "[a.txt@" + unknownReference + "]\nPUT 1:\n+x\n"
	oneRefSuffix     = "Read and copy an eligible new header"
)

func newProfileHarness(t *testing.T, style string, opts ...func(*harness)) *harness {
	t.Helper()
	tools := func(h *harness) {
		h.cfg.Tools = func(context.Context, string, string) ([]tool.Tool, error) {
			return []tool.Tool{read.New(), patch.New(style)}, nil
		}
	}
	h := newHarness(t, append([]func(*harness){tools}, opts...)...)
	h.model.route = resolveReferences(h.model)
	return h
}

func resolveReferences(f *fakeModel) func([]byte) (response, bool) {
	return func(body []byte) (response, bool) {
		if len(f.queue) == 0 || !refPlaceholder.MatchString(f.queue[0].body) {
			return response{}, false
		}
		next := f.queue[0]
		f.queue = f.queue[1:]
		next.body = refPlaceholder.ReplaceAllStringFunc(next.body, func(placeholder string) string {
			return headerReference(body, refPlaceholder.FindStringSubmatch(placeholder)[1], placeholder)
		})
		return next, true
	}
}

func headerReference(body []byte, path, fallback string) string {
	header := regexp.MustCompile(`\[` + regexp.QuoteMeta(path) + `@(r[0-9a-f]{32}\.[0-9a-z]+)\]`)
	if found := header.FindSubmatch(body); found != nil {
		return string(found[1])
	}
	return fallback
}

func readCall(id, args string) call { return call{id: id, name: "read", args: args} }

func patchCall(id, input string) call {
	return call{id: id, name: "patch", args: `{"input":` + string(jsonString(input)) + `}`}
}

func (h *harness) runRounds(rounds ...response) []event.Seqd {
	h.t.Helper()
	h.model.push(append(rounds, text("done"))...)
	thread := h.start()
	h.submit(thread, "edit the file")
	h.waitIdle(thread)
	return h.events(thread.ID())
}

func (h *harness) runRoundsAnswering(reply Reply, answered string, rounds ...response) []event.Seqd {
	h.t.Helper()
	h.model.push(append(rounds, text("done"))...)
	thread := h.start()
	client := answerApprovals(h.t, thread, func(prompt ApprovalPrompt) Reply {
		if prompt.CallID == answered {
			return reply
		}
		return ReplyAccept
	})
	h.submit(thread, "edit the file")
	h.waitIdle(thread)
	client.stop()
	return h.events(thread.ID())
}

type hashlineMember struct {
	ContractVersion int            `json:"contract_version"`
	Style           string         `json:"style"`
	Status          string         `json:"status"`
	CommitState     string         `json:"commit_state"`
	Reason          *string        `json:"reason"`
	ErrorClass      *string        `json:"error_class"`
	Replayed        bool           `json:"replayed"`
	Recovery        jsontext.Value `json:"recovery"`
	PathStates      []struct {
		Path  string `json:"path"`
		State string `json:"state"`
	} `json:"path_states"`
}

type hashlineWant struct {
	style, status, commit, reason, class string
}

func decodeHashline(t *testing.T, meta jsontext.Value) hashlineMember {
	t.Helper()
	var top map[string]jsontext.Value
	if err := json.Unmarshal(meta, &top); err != nil || len(top) != 1 {
		t.Fatalf("meta %q holds %d members, err %v; want only hashline", meta, len(top), err)
	}
	var object hashlineMember
	if err := json.Unmarshal(top["hashline"], &object); err != nil {
		t.Fatalf("meta %q: %v", meta, err)
	}
	return object
}

func requireHashline(t *testing.T, meta jsontext.Value, want hashlineWant) {
	t.Helper()
	requireHashlineRecovery(t, meta, want, "null")
}

func requireHashlineRecovery(t *testing.T, meta jsontext.Value, want hashlineWant, recovery string) {
	t.Helper()
	got := decodeHashline(t, meta)
	requireHashlineHead(t, meta, got, want)
	if string(got.Recovery) != recovery || len(got.PathStates) != 0 {
		t.Fatalf("meta %s, want recovery %s and no path states", meta, recovery)
	}
}

func requireHashlineHead(t *testing.T, meta jsontext.Value, got hashlineMember, want hashlineWant) {
	t.Helper()
	if got.ContractVersion != 1 || got.Style != want.style || got.Status != want.status || got.CommitState != want.commit || got.Replayed {
		t.Fatalf("meta %s, want %+v at version 1 and replayed false", meta, want)
	}
	if optional(got.Reason) != want.reason || optional(got.ErrorClass) != want.class {
		t.Fatalf("meta %s, want reason %q and class %q", meta, want.reason, want.class)
	}
}

func optional(value *string) string {
	if value == nil {
		return ""
	}
	return *value
}

type profileCase struct {
	row    string
	name   string
	style  string
	first  string
	edits  []string
	reply  Reply
	ok     bool
	output string
	suffix string
	file   string
	want   hashlineWant
}

func (c profileCase) rounds() []response {
	rounds := []response{calls(readCall("r1", c.first))}
	for i, input := range c.edits {
		rounds = append(rounds, calls(patchCall(fmt.Sprintf("p%d", i+1), input)))
	}
	return rounds
}

func (c profileCase) events(t *testing.T) (*harness, []event.Seqd) {
	t.Helper()
	if c.reply == "" {
		h := newProfileHarness(t, c.style)
		h.write("a.txt", threeLines)
		return h, h.runRounds(c.rounds()...)
	}
	h := newProfileHarness(t, c.style, withApproval(config.ApprovalAsk))
	h.write("a.txt", threeLines)
	return h, h.runRoundsAnswering(c.reply, fmt.Sprintf("p%d", len(c.edits)), c.rounds()...)
}

func (c profileCase) run(t *testing.T) {
	t.Parallel()
	h, events := c.events(t)
	got := completedOutputs(events)[fmt.Sprintf("p%d", len(c.edits))]
	if got.OK != c.ok || !strings.HasSuffix(got.Output, c.suffix) || (c.output != "" && got.Output != c.output) {
		t.Fatalf("completion = %+v, want ok=%v output %q ending %q", got, c.ok, c.output, c.suffix)
	}
	requireHashline(t, got.Meta, c.want)
	if content := h.read("a.txt"); content != c.file {
		t.Fatalf("file = %q, want %q", content, c.file)
	}
}

func profileCases() []profileCase {
	light := func(status, commit, reason, class string) hashlineWant {
		return hashlineWant{"hashline-light", status, commit, reason, class}
	}
	const whole, window = `{"path":"a.txt"}`, `{"path":"a.txt","limit":1}`
	return []profileCase{
		{
			row: "COMPAT-03", name: "applied", style: "hashline-light", first: whole, edits: []string{editTwo}, ok: true,
			file: "one\nTWO\nthree\n", want: light("applied", "complete", "", ""),
		},
		{
			row: "COMPAT-03", name: "applied enhanced", style: "hashline-enhanced", first: whole, edits: []string{editTwo}, ok: true,
			file: "one\nTWO\nthree\n", want: hashlineWant{"hashline-enhanced", "applied", "complete", "", ""},
		},
		{
			row: "COMPAT-03", name: "no_change", style: "hashline-light", first: whole, ok: true,
			edits: []string{"[a.txt@{ref:a.txt}]\nPUT 2:\n+two\n"}, file: threeLines, want: light("no_change", "none", "", ""),
		},
		{
			row: "L-04", name: "conflict", style: "hashline-light", first: whole,
			edits:  []string{"[a.txt@{ref:a.txt}]\nPUT <3:\n+inside\n", "[a.txt@{ref:a.txt}]\nCUT 2-3\n"},
			file:   "one\ntwo\ninside\nthree\n",
			suffix: "target changed. Read again",
			want:   light("conflict", "none", "target_changed", "stale"),
		},
		{
			row: "E-02", name: "needs_context", style: "hashline-enhanced", first: window,
			edits: []string{"[a.txt@{ref:a.txt}]\nPUT 3:\n+x\n"}, file: threeLines,
			suffix: "need prior observation. Read them, then resend",
			want:   hashlineWant{"hashline-enhanced", "needs_context", "none", "unobserved_range", "proof"},
		},
		{
			row: "C15", name: "reference_unknown", style: "hashline-light", first: whole, edits: []string{unknownEdit},
			file: threeLines, suffix: oneRefSuffix, want: light("needs_context", "none", "reference_unknown", "proof"),
		},
		{
			row: "AUTH-01 L-08", name: "user decline", style: "hashline-light", first: whole, edits: []string{editTwo},
			reply: ReplyDecline, output: "declined by user", file: threeLines, want: light("rejected", "none", "permission_denied", "blocked"),
		},
		{
			row: "approvals.md cancel", name: "user cancel", style: "hashline-light", first: whole, edits: []string{editTwo},
			reply: ReplyCancel, output: "cancelled by user", file: threeLines, want: light("interrupted", "none", "interrupted", ""),
		},
	}
}

func TestProfileEditCompletionCarriesHashlineObject(t *testing.T) {
	for _, test := range profileCases() {
		t.Run(test.row+"/"+test.name, test.run)
	}
}

func TestProfileEditCompletionPutsMembersInContractOrder(t *testing.T) {
	t.Run("C15/member order", requireContractOrder)
}

func requireContractOrder(t *testing.T) {
	t.Parallel()
	h := newProfileHarness(t, "hashline-light")
	h.write("a.txt", threeLines)
	events := h.runRounds(calls(readCall("r1", `{"path":"a.txt"}`)), calls(patchCall("p1", editTwo)))
	want := `{"hashline":{"contract_version":1,"style":"hashline-light","status":"applied","commit_state":"complete","reason":null,"error_class":null,"replayed":false,"recovery":null,"path_states":[]}}`
	if got := completedOutputs(events)["p1"].Meta; string(got) != want {
		t.Fatalf("meta\n%s\nwant\n%s", got, want)
	}
	if meta := completedOutputs(events)["r1"].Meta; strings.Contains(string(meta), "hashline") {
		t.Fatalf("read completion carries a hashline member: %s", meta)
	}
}

func TestLegacyEditCompletionHasNoMeta(t *testing.T) {
	cases := []struct {
		name  string
		reply Reply
		want  func(event.ToolCompleted) bool
	}{
		{"applied", ReplyAccept, func(c event.ToolCompleted) bool { return c.OK }},
		{"declined", ReplyDecline, func(c event.ToolCompleted) bool { return c.Output == "declined by user" }},
		{"cancelled", ReplyCancel, func(c event.ToolCompleted) bool { return c.Output == "cancelled by user" }},
	}
	for _, test := range cases {
		t.Run("COMPAT-01/"+test.name, func(t *testing.T) {
			t.Parallel()
			h := newHarness(t, withApproval(config.ApprovalAsk))
			h.write("a.txt", threeLines)
			change := call{id: "p1", name: "patch", args: `{"changes":[{"path":"a.txt","old":"two","new":"TWO"}]}`}
			got := completedOutputs(h.runRoundsAnswering(test.reply, "p1", calls(change)))["p1"]
			if !test.want(got) || len(got.Meta) != 0 {
				t.Fatalf("completion = %+v, want a %s call with no meta", got, test.name)
			}
		})
	}
	t.Run("COMPAT-01/refused plan", requireLegacyRefusalHasNoMeta)
}

func requireLegacyRefusalHasNoMeta(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.write("a.txt", threeLines)
	change := call{id: "p1", name: "patch", args: `{"changes":[{"path":"a.txt","old":"absent text","new":"TWO"}]}`}
	got := completedOutputs(h.runRounds(calls(change)))["p1"]
	if got.OK || got.Output == "" || len(got.Meta) != 0 {
		t.Fatalf("completion = %+v, want a refusal with no meta", got)
	}
}

func TestRefusalCompletionCarriesOutcome(t *testing.T) {
	t.Run("C15/plan error", requirePlanErrorCarriesOutcome)
	t.Run("AUTH-01 L-08/user decline", requireDeclineKeepsItsText)
	t.Run("approvals.md cancel/user cancel", requireCancelKeepsItsText)
	t.Run("AUTH-01/in-band decline after planning", requireInBandDecline)
	t.Run("approvals.md cancel/in-band cancel after planning", requireInBandCancel)
	t.Run("C15/in-band plan refusal", requireInBandPlanRefusal)
}

func requirePlanErrorCarriesOutcome(t *testing.T) {
	t.Parallel()
	h := newProfileHarness(t, "hashline-light")
	h.write("a.txt", threeLines)
	events := h.runRounds(calls(patchCall("p1", unknownEdit)))
	got := completedOutputs(events)["p1"]
	requireHashline(t, got.Meta, hashlineWant{"hashline-light", "needs_context", "none", "reference_unknown", "proof"})
	if resolved := resolutions(events)["p1"]; resolved.Decision != "decline" || resolved.Reason != got.Output {
		t.Fatalf("approval record = %+v, want the completion text %q", resolved, got.Output)
	}
	if !strings.HasSuffix(got.Output, oneRefSuffix) || strings.Contains(got.Output, "hashline") {
		t.Fatalf("completion output = %q, want today's refusal text without the object", got.Output)
	}
}

func requireDeclineKeepsItsText(t *testing.T) {
	t.Parallel()
	requireAnsweredRefusal(t, ReplyDecline, "declined by user", hashlineWant{"hashline-light", "rejected", "none", "permission_denied", "blocked"})
}

func requireCancelKeepsItsText(t *testing.T) {
	t.Parallel()
	requireAnsweredRefusal(t, ReplyCancel, "cancelled by user", hashlineWant{"hashline-light", "interrupted", "none", "interrupted", ""})
}

func requireAnsweredRefusal(t *testing.T, reply Reply, text string, want hashlineWant) {
	t.Helper()
	h := newProfileHarness(t, "hashline-light", withApproval(config.ApprovalAsk))
	h.write("a.txt", threeLines)
	events := h.runRoundsAnswering(reply, "p1", calls(readCall("r1", `{"path":"a.txt"}`)), calls(patchCall("p1", editTwo)))
	got := completedOutputs(events)["p1"]
	if got.OK || got.Output != text || got.Recovered != "none" {
		t.Fatalf("completion = %+v, want ok=false output %q recovered=none", got, text)
	}
	requireHashline(t, got.Meta, want)
	if resolved := resolutions(events)["p1"]; resolved.Reason != text {
		t.Fatalf("approval record reason = %q, want %q", resolved.Reason, text)
	}
}

func TestPrePlanRefusalCarriesNoObject(t *testing.T) {
	t.Run("C15/invalid arguments", requireInvalidArgumentsHaveNoObject)
	t.Run("C15/tool not found", requireUnknownToolHasNoObject)
	t.Run("C15/in-band tool not found", requireInBandPrePlanDenial)
	t.Run("C15/in-band cancel before planning", requireInBandCancelBeforePlanning)
	t.Run("C15/interrupt during the approval wait", requireSkippedCallHasNoObject)
	t.Run("C15/call skipped after a cancel", requireSiblingSkippedAfterCancel)
}

func requireSiblingSkippedAfterCancel(t *testing.T) {
	t.Parallel()
	h := newProfileHarness(t, "hashline-light", withApproval(config.ApprovalAsk))
	second := `{"input":"[m.txt#NEW]\nPUT >$:\n+body\n"}`
	events := h.runRoundsAnswering(ReplyCancel, "first", calls(call{id: "first", name: "patch", args: createCall}, call{id: "second", name: "patch", args: second}))
	completed := completedOutputs(events)
	if first := completed["first"]; first.Output != "cancelled by user" || len(first.Meta) == 0 {
		t.Fatalf("first completion = %+v, want the cancel text and an object", first)
	}
	got := completed["second"]
	if got.OK || got.Output != "skipped: the turn was interrupted" || got.Recovered != "skipped" || len(got.Meta) != 0 {
		t.Fatalf("second completion = %+v, want a skipped call with no meta", got)
	}
}

func requireInvalidArgumentsHaveNoObject(t *testing.T) {
	t.Parallel()
	h := newProfileHarness(t, "hashline-light")
	got := completedOutputs(h.runRounds(calls(call{id: "p1", name: "patch", args: `[1]`})))["p1"]
	if got.OK || !strings.HasPrefix(got.Output, "invalid arguments for patch") || len(got.Meta) != 0 {
		t.Fatalf("completion = %+v, want a pre-plan decline with no meta", got)
	}
}

func requireUnknownToolHasNoObject(t *testing.T) {
	t.Parallel()
	h := newProfileHarness(t, "hashline-light")
	got := completedOutputs(h.runRounds(calls(call{id: "p1", name: "absent", args: `{}`})))["p1"]
	if got.OK || got.Output != "Tool absent not found" || len(got.Meta) != 0 {
		t.Fatalf("completion = %+v, want a decline with no meta", got)
	}
}

func requireSkippedCallHasNoObject(t *testing.T) {
	t.Parallel()
	h := newProfileHarness(t, "hashline-light", withApproval(config.ApprovalAsk))
	h.write("a.txt", threeLines)
	h.model.push(calls(readCall("r1", `{"path":"a.txt"}`)), calls(patchCall("p1", editTwo)), text("done"))
	thread := h.start()
	updates := thread.Subscribe()
	done := make(chan struct{})
	go func() {
		defer close(done)
		for update := range updates {
			interruptOnEdit(thread, update)
		}
	}()
	h.submit(thread, "edit the file")
	h.waitIdle(thread)
	thread.Unsubscribe(updates)
	<-done
	got := completedOutputs(h.events(thread.ID()))["p1"]
	if got.OK || got.Output != "skipped: the turn was interrupted" || got.Recovered != "skipped" || len(got.Meta) != 0 {
		t.Fatalf("completion = %+v, want a skipped call with no meta", got)
	}
}

func interruptOnEdit(thread *Thread, update Update) {
	switch {
	case update.Kind != UpdateApproval || update.Approval.Resolved:
	case update.Approval.CallID == "p1":
		thread.Interrupt()
	default:
		_ = thread.Answer(update.Approval.RequestID, ReplyAccept)
	}
}

type inBandProfile struct {
	h      *harness
	thread *Thread
	w      *worker
}

func newInBandProfile(t *testing.T) inBandProfile {
	t.Helper()
	h := newProfileHarness(t, "hashline-light")
	h.write("a.txt", threeLines)
	thread := h.start()
	startInBandTurn(t, thread)
	ts, err := thread.toolsFor(t.Context(), "openai/gpt-test")
	if err != nil {
		t.Fatalf("agent: tools: %v", err)
	}
	w := &worker{t: thread, turnID: "turn-test", settings: Settings{Approval: config.ApprovalAsk}, tools: ts, inBand: newInBandCalls()}
	return inBandProfile{h: h, thread: thread, w: w}
}

func (p inBandProfile) call(t *testing.T, id, name, args string) event.ToolCompleted {
	t.Helper()
	completed, err := p.w.inBandEvents(t.Context(), model.ToolCall{ID: id, Name: name, Args: jsontext.Value(args)})
	if err != nil {
		t.Fatalf("in-band call %s: %v", id, err)
	}
	if journaled := completedOutputs(p.h.events(p.thread.ID()))[id]; string(journaled.Meta) != string(completed.Meta) {
		t.Fatalf("journaled meta %s differs from the returned meta %s", journaled.Meta, completed.Meta)
	}
	return completed
}

func requireInBandDecline(t *testing.T) {
	t.Parallel()
	p := newInBandProfile(t)
	got := p.call(t, "c1", "patch", createCall)
	if got.OK || got.Output != declineNoClient {
		t.Fatalf("completion = %+v, want the decline %q", got, declineNoClient)
	}
	requireHashline(t, got.Meta, hashlineWant{"hashline-light", "rejected", "none", "permission_denied", "blocked"})
}

func requireInBandCancel(t *testing.T) {
	t.Parallel()
	p := newInBandProfile(t)
	client := answerApprovals(t, p.thread, func(ApprovalPrompt) Reply { return ReplyCancel })
	got := p.call(t, "c1", "patch", createCall)
	client.stop()
	if got.OK || got.Output != "cancelled by user" {
		t.Fatalf("completion = %+v, want the cancel text", got)
	}
	requireHashline(t, got.Meta, hashlineWant{"hashline-light", "interrupted", "none", "interrupted", ""})
}

func requireInBandPlanRefusal(t *testing.T) {
	t.Parallel()
	p := newInBandProfile(t)
	got := p.call(t, "c1", "patch", `{"input":`+string(jsonString(unknownEdit))+`}`)
	if got.OK || !strings.HasSuffix(got.Output, oneRefSuffix) {
		t.Fatalf("completion = %+v, want a plan refusal", got)
	}
	requireHashline(t, got.Meta, hashlineWant{"hashline-light", "needs_context", "none", "reference_unknown", "proof"})
}

func requireInBandPrePlanDenial(t *testing.T) {
	t.Parallel()
	p := newInBandProfile(t)
	got := p.call(t, "c1", "absent", `{}`)
	if got.OK || got.Output != "Tool absent not found" || len(got.Meta) != 0 {
		t.Fatalf("completion = %+v, want a denial with no meta", got)
	}
}

func requireInBandCancelBeforePlanning(t *testing.T) {
	t.Parallel()
	p := newInBandProfile(t)
	p.w.inBand.markCancel()
	got := p.call(t, "c1", "patch", createCall)
	if got.OK || got.Output != "cancelled by user" || len(got.Meta) != 0 {
		t.Fatalf("completion = %+v, want the cancel text with no meta", got)
	}
}

func TestRecoveredDeclineCompletionCarriesNoObject(t *testing.T) {
	t.Run("C15/recovered decline", requireRecoveredDeclineHasNoObject)
}

func requireRecoveredDeclineHasNoObject(t *testing.T) {
	t.Parallel()
	h := newProfileHarness(t, "hashline-light", withApproval(config.ApprovalAsk))
	h.seed(prefix(openTurn("edit"), []event.Event{
		assistantCall("c1", "patch", createCall),
		event.ApprovalRequested{RequestID: "r-c1", CallID: "c1", Summary: "patch", Class: "edit"},
		event.ApprovalResolved{RequestID: "r-c1", Decision: "decline", By: "user", Reason: "declined by user"},
	})...)
	got := completedOutputs(h.recoverSession())["c1"]
	if got.OK || got.Output != "declined by user" || got.Recovered != "none" || len(got.Meta) != 0 {
		t.Fatalf("completion = %+v, want ok=false output=declined by user recovered=none and no meta", got)
	}
}
