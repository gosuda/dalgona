package advisor

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"strconv"
	"strings"
	"sync"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
)

const (
	maxFailures   = 3
	reviewEffort  = "medium"
	reviewOpen    = `<review seq="`
	reviewClose   = "</review>\n"
	inputBudget   = event.MaxInlineBytes - 1024
	statusDone    = "completed"
	minClipLength = 64
)

var errDiscarded = errors.New("advisor: review discarded: destructive shell directive in advisor output")

type review struct {
	seq       uint64
	start     uint64
	continued bool
}

type reviewInput struct {
	text  string
	head  uint64
	reset bool
}

type advisor struct {
	rt        *Runtime
	host      *agent.Host
	name      string
	spec      config.AdvisorSpec
	model     string
	maxNotes  int
	guard     *Guard
	overrides agent.Overrides
	ready     chan struct{}
	wake      chan struct{}

	mu        sync.Mutex
	changed   chan struct{}
	child     *agent.Thread
	cwd       string
	on        bool
	halted    bool
	reviewing bool
	failures  int
	eligible  int
	requested uint64
	settled   uint64
	cursor    uint64
	current   review
	recovered *review
}

func (a *advisor) attach(ctx context.Context, info agent.ThreadInfo) error {
	a.cwd = info.Cwd
	read, err := a.rt.primary.ReadAfter(0)
	if err != nil {
		return fmt.Errorf("advisor: read primary: %w", err)
	}
	a.seedGuard(read.Events)
	a.cursor = read.Head
	if info.ForkOf != "" {
		a.cursor = 0
	}
	id, ok := a.host.AdvisorChild(info.ID, a.name)
	if !ok {
		return nil
	}
	child, err := a.host.ResumeWith(ctx, id, a.overrides)
	if err != nil {
		return fmt.Errorf("advisor: resume %s: %w", a.name, err)
	}
	a.child = child
	past, err := child.ReadAfter(0)
	if err != nil {
		return fmt.Errorf("advisor: read %s: %w", a.name, err)
	}
	a.restore(past.Events)
	return nil
}

func (a *advisor) seedGuard(events []event.Seqd) {
	for _, item := range events {
		note, ok := item.Event.(event.AdvisorNote)
		if !ok || note.Advisor != a.name {
			continue
		}
		severity, _ := ParseSeverity(note.Severity)
		a.guard.BeginReview()
		if admission := a.guard.Admit(note.Note, severity); admission.Verdict == VerdictAccepted {
			a.guard.Delivered(admission.Key)
		}
	}
	a.guard.BeginReview()
}

func (a *advisor) restore(events []event.Seqd) {
	var completed uint64
	for _, item := range events {
		switch value := item.Event.(type) {
		case event.UserMessage:
			if seq, ok := parseReviewSeq(value.Text); ok {
				a.recovered = &review{seq: seq, start: item.Seq - 1}
			}
		case event.TurnCompleted:
			if a.recovered != nil && value.Status == statusDone {
				completed = a.recovered.seq
			}
		}
	}
	if a.recovered != nil {
		a.current, a.reviewing = *a.recovered, true
	}
	if completed != 0 {
		a.cursor = completed
	}
}

func reviewPrompt(seq uint64, text string) string {
	var out strings.Builder
	out.Grow(len(reviewOpen) + len(text) + len(reviewClose) + 24)
	out.WriteString(reviewOpen)
	out.WriteString(strconv.FormatUint(seq, 10))
	out.WriteString("\">\n")
	out.WriteString(text)
	out.WriteString(reviewClose)
	return out.String()
}

func parseReviewSeq(text string) (uint64, bool) {
	rest, ok := strings.CutPrefix(text, reviewOpen)
	if !ok {
		return 0, false
	}
	digits, _, ok := strings.Cut(rest, `"`)
	if !ok {
		return 0, false
	}
	seq, err := strconv.ParseUint(digits, 10, 64)
	return seq, err == nil
}

func (a *advisor) run(ctx context.Context) {
	a.mu.Lock()
	recovered := a.recovered
	a.recovered = nil
	a.mu.Unlock()
	if recovered != nil {
		if err := a.complete(ctx, *recovered); err != nil {
			a.fail(0, err)
		} else {
			a.succeed(0, recovered.seq)
		}
	}
	for {
		select {
		case <-a.wake:
		case <-ctx.Done():
			return
		}
		for a.due() {
			a.review(ctx)
		}
	}
}

func (a *advisor) due() bool {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.activeLocked() && a.requested > a.settled
}

func (a *advisor) activeLocked() bool {
	return a.on && a.spec.Enabled && !a.halted
}

func (a *advisor) enabled() bool {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.activeLocked()
}

func (a *advisor) review(ctx context.Context) {
	a.mu.Lock()
	target, cursor := a.requested, a.cursor
	a.reviewing = true
	a.mu.Unlock()
	in, err := a.input(cursor)
	if err != nil {
		a.fail(target, err)
		return
	}
	if in.reset {
		a.guard.Reset()
	}
	if in.text == "" {
		a.succeed(target, in.head)
		return
	}
	if err := a.submit(ctx, in); err != nil {
		a.fail(target, err)
		return
	}
	a.mu.Lock()
	rv := a.current
	a.mu.Unlock()
	if err := a.complete(ctx, rv); err != nil {
		a.fail(target, err)
		return
	}
	a.succeed(target, in.head)
}

func (a *advisor) input(cursor uint64) (reviewInput, error) {
	read, err := a.rt.primary.ReadAfter(cursor)
	if err != nil {
		return reviewInput{}, fmt.Errorf("advisor: read primary: %w", err)
	}
	return reviewInput{text: boundedRender(read.Events, inputBudget), head: read.Head, reset: read.Reset}, nil
}

func (a *advisor) submit(ctx context.Context, in reviewInput) error {
	child, err := a.ensureChild(ctx)
	if err != nil {
		return err
	}
	status, err := child.Status()
	if err != nil {
		return fmt.Errorf("advisor: session status: %w", err)
	}
	a.guard.BeginReview()
	a.mu.Lock()
	a.current = review{seq: in.head, start: status.Head}
	a.mu.Unlock()
	prompt := agent.Input{Kind: agent.InputPrompt, Text: reviewPrompt(in.head, in.text), Effort: reviewEffort, Verbatim: true}
	if err := child.Submit(ctx, prompt); err != nil {
		return fmt.Errorf("advisor: submit review: %w", err)
	}
	return nil
}

func (a *advisor) ensureChild(ctx context.Context) (*agent.Thread, error) {
	a.mu.Lock()
	child := a.child
	a.mu.Unlock()
	if child != nil {
		return child, nil
	}
	child, err := a.host.Start(ctx, agent.ThreadSpec{
		Cwd: a.cwd, Model: a.model, Parent: a.rt.primary.ID(), Advisor: a.name, Overrides: a.overrides,
	})
	if err != nil {
		return nil, fmt.Errorf("advisor: start %s session: %w", a.name, err)
	}
	a.mu.Lock()
	a.child = child
	a.mu.Unlock()
	return child, nil
}

func (a *advisor) complete(ctx context.Context, rv review) error {
	a.mu.Lock()
	child := a.child
	a.mu.Unlock()
	if err := waitTurnEnd(ctx, child); err != nil {
		return err
	}
	events, err := reviewEvents(child, rv)
	if err != nil {
		return err
	}
	if ended := lastTurnEnd(events); ended.Status != statusDone {
		return fmt.Errorf("advisor: review ended %q: %s", ended.Status, ended.Error)
	}
	if unsafeOutput(events, reviewText(events)) {
		return errDiscarded
	}
	if !a.enabled() {
		return nil
	}
	var errs []error
	for _, note := range pendingNotes(events, a.maxNotes) {
		errs = append(errs, a.deliver(ctx, note))
	}
	return errors.Join(errs...)
}

func reviewEvents(child *agent.Thread, rv review) ([]event.Seqd, error) {
	read, err := child.ReadAfter(rv.start)
	if err != nil {
		return nil, fmt.Errorf("advisor: read review: %w", err)
	}
	events := make([]event.Seqd, 0, len(read.Events))
	for _, item := range read.Events {
		if item.Seq > rv.start {
			events = append(events, item)
		}
	}
	return events, nil
}

func lastTurnEnd(events []event.Seqd) event.TurnCompleted {
	var ended event.TurnCompleted
	for _, item := range events {
		if value, ok := item.Event.(event.TurnCompleted); ok {
			ended = value
		}
	}
	return ended
}

func reviewText(events []event.Seqd) string {
	for _, item := range events {
		if message, ok := item.Event.(event.UserMessage); ok {
			return message.Text
		}
	}
	return ""
}

func waitTurnEnd(ctx context.Context, child *agent.Thread) error {
	updates := child.Subscribe()
	defer func() { child.Unsubscribe(updates) }()
	for {
		status, err := child.Status()
		if err != nil {
			return fmt.Errorf("advisor: session status: %w", err)
		}
		if status.Turn == "" {
			return nil
		}
		if updates, err = nextTurnEnd(ctx, child, updates); err != nil {
			return err
		}
	}
}

func nextTurnEnd(ctx context.Context, child *agent.Thread, updates <-chan agent.Update) (<-chan agent.Update, error) {
	for {
		select {
		case <-ctx.Done():
			return updates, fmt.Errorf("advisor: wait for review: %w", context.Cause(ctx))
		case update, ok := <-updates:
			if !ok {
				return child.Subscribe(), nil
			}
			if _, ended := update.Event.(event.TurnCompleted); ended {
				return updates, nil
			}
		}
	}
}

func (a *advisor) admit(ctx context.Context, parsed advice) (string, error) {
	if err := a.awaitReady(ctx); err != nil {
		return "", err
	}
	if parsed.severity == SeverityBlocker {
		return a.admitBlocker(ctx, parsed)
	}
	return string(a.guard.Admit(parsed.note, parsed.severity).Verdict), nil
}

func (a *advisor) admitBlocker(ctx context.Context, parsed advice) (string, error) {
	if safe, err := a.safeSoFar(); err != nil || !safe || !a.enabled() {
		return verdictDiscarded, err
	}
	admission := a.guard.Admit(parsed.note, parsed.severity)
	if admission.Verdict != VerdictAccepted {
		return string(admission.Verdict), nil
	}
	if err := a.deliver(ctx, pendingNote{key: admission.Key, text: parsed.note, advice: parsed}); err != nil {
		a.guard.Withdraw(admission)
		return "", err
	}
	return string(admission.Verdict), nil
}

func (a *advisor) awaitReady(ctx context.Context) error {
	select {
	case <-a.ready:
		return nil
	case <-ctx.Done():
		return fmt.Errorf("advise: %w", context.Cause(ctx))
	}
}

func (a *advisor) safeSoFar() (bool, error) {
	a.mu.Lock()
	child, rv := a.child, a.current
	a.mu.Unlock()
	events, err := reviewEvents(child, rv)
	if err != nil {
		return false, err
	}
	return !unsafeOutput(events, reviewText(events)), nil
}

func (a *advisor) deliver(ctx context.Context, note pendingNote) error {
	a.mu.Lock()
	rv := a.current
	a.mu.Unlock()
	delivery, err := a.rt.route(note.severity, rv.continued)
	if err != nil {
		return err
	}
	got, err := a.rt.primary.Deliver(ctx, agent.Note{
		Advisor: a.name, Severity: note.severity.String(), Text: note.text, ReviewSeq: rv.seq, Delivery: delivery,
	})
	if err != nil {
		return fmt.Errorf("advisor: deliver note: %w", err)
	}
	a.rt.delivered(got, note.severity)
	a.guard.Delivered(note.key)
	if got == agent.DeliveryContinue {
		a.mu.Lock()
		a.current.continued = true
		a.mu.Unlock()
	}
	return nil
}

func (a *advisor) succeed(target, cursor uint64) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.cursor, a.failures = cursor, 0
	a.settleLocked(target)
}

func (a *advisor) fail(target uint64, err error) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.activeLocked() {
		a.failures++
		a.halted = a.failures >= maxFailures
		slog.Warn("advisor: review failed", "advisor", a.name, "failures", a.failures, "error", err)
	}
	a.settleLocked(target)
}

func (a *advisor) settleLocked(target uint64) {
	a.reviewing = false
	a.settled = max(a.settled, target)
	if !a.activeLocked() {
		a.settled = a.requested
	}
	a.broadcastLocked()
}

func (a *advisor) broadcastLocked() {
	close(a.changed)
	a.changed = make(chan struct{})
}

func (a *advisor) boundary(b agent.Boundary) agent.Wait {
	a.mu.Lock()
	defer a.mu.Unlock()
	if !a.activeLocked() || b.Continuation {
		return nil
	}
	scheduled := a.scheduleLocked(b)
	if b.Ended != "" || !a.waitsLocked(b.Kind, scheduled) || a.settled >= a.requested {
		return nil
	}
	target := a.requested
	return func(ctx context.Context) error { return a.waitSettled(ctx, target) }
}

func (a *advisor) scheduleLocked(b agent.Boundary) bool {
	if a.spec.Review == config.AdvisorReviewAgentEnd && b.Kind != agent.BoundaryAgentEnd {
		return false
	}
	a.eligible++
	if a.eligible%max(a.spec.Interval, 1) != 0 {
		return false
	}
	a.requested = max(a.requested, b.Head)
	select {
	case a.wake <- struct{}{}:
	default:
	}
	return true
}

func (a *advisor) waitsLocked(kind agent.BoundaryKind, scheduled bool) bool {
	switch a.spec.Sync {
	case config.AdvisorSyncStrict:
		return scheduled || kind == agent.BoundaryAgentEnd
	case config.AdvisorSyncFinal:
		return kind == agent.BoundaryAgentEnd
	case config.AdvisorSyncOff:
	}
	return false
}

func (a *advisor) waitSettled(ctx context.Context, target uint64) error {
	for {
		a.mu.Lock()
		done := a.settled >= target || !a.activeLocked()
		changed := a.changed
		a.mu.Unlock()
		if done {
			return nil
		}
		select {
		case <-changed:
		case <-ctx.Done():
			return nil
		case <-a.rt.done:
			return nil
		}
	}
}

func (a *advisor) setEnabled(enabled bool, head uint64) {
	a.mu.Lock()
	if enabled && !a.on {
		a.cursor, a.settled = head, a.requested
	}
	a.on = enabled
	child, reviewing := a.child, a.reviewing
	a.broadcastLocked()
	a.mu.Unlock()
	if !enabled && reviewing && child != nil {
		child.Interrupt()
	}
}

func (a *advisor) reset() {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.halted, a.failures, a.settled = false, 0, a.requested
	a.broadcastLocked()
}

func (a *advisor) status(primary []event.Seqd) (Status, error) {
	a.mu.Lock()
	out := Status{Name: a.name, Model: a.model, State: a.stateLocked()}
	child := a.child
	a.mu.Unlock()
	for _, item := range primary {
		if note, ok := item.Event.(event.AdvisorNote); ok && note.Advisor == a.name {
			out.Notes++
		}
	}
	if child == nil {
		return out, nil
	}
	read, err := child.ReadAfter(0)
	if err != nil {
		return out, fmt.Errorf("advisor: read %s: %w", a.name, err)
	}
	for _, item := range read.Events {
		if message, ok := item.Event.(event.AssistantMessage); ok {
			out.Usage.Input += message.Usage.Input
			out.Usage.Output += message.Usage.Output
			out.Usage.CacheRead += message.Usage.CacheRead
			out.Usage.CacheWrite += message.Usage.CacheWrite
		}
	}
	return out, nil
}

func (a *advisor) stateLocked() State {
	switch {
	case !a.on || !a.spec.Enabled:
		return StateDisabled
	case a.halted:
		return StateHalted
	case a.reviewing:
		return StateReviewing
	default:
		return StateIdle
	}
}

func boundedRender(events []event.Seqd, budget int) string {
	parts := make([]string, 0, min(len(events), 64))
	remaining, first := budget, len(events)
	for first > 0 {
		part := Render(events[first-1 : first])
		if len(part) > remaining {
			if remaining >= minClipLength {
				parts = append(parts, clip(part, remaining))
				first--
			}
			break
		}
		parts = append(parts, part)
		remaining -= len(part)
		first--
	}
	var out strings.Builder
	out.Grow(budget - remaining + 48)
	if first > 0 {
		out.WriteString("[" + strconv.Itoa(first) + " earlier events omitted]\n")
	}
	for i := len(parts) - 1; i >= 0; i-- {
		out.WriteString(parts[i])
	}
	return out.String()
}

func clip(text string, size int) string {
	half := size/2 - 24
	omitted := strconv.Itoa(len(text) - 2*half)
	return strings.ToValidUTF8(text[:half], "") + "\n[" + omitted + " bytes omitted]\n" + strings.ToValidUTF8(text[len(text)-half:], "")
}
