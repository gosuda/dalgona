package agent

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"os"
	"runtime"
	"slices"
	"sync"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/agent/prompt"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/journal"
	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

const subscriberBuffer = 4096

var errInterrupted = errors.New("the turn was interrupted")

type loadedSession struct {
	created event.SessionCreated
	records []event.Seqd
	sizes   []int
	state   event.State
	damage  error
}

type Thread struct {
	host           *Host
	id             string
	created        event.SessionCreated
	fixedTools     []tool.Tool
	toolsMu        sync.Mutex
	sandboxMu      sync.Mutex
	sandboxPending *sandboxAsk
	toolset        *toolset
	ext            Extension
	system         string
	runner         runner.Runner
	snapshots      *snapshot.Store
	release        func() error

	children  chan struct{}
	inbox     chan func(*actor)
	quit      chan struct{}
	stopped   chan struct{}
	closeOnce sync.Once
	closeErr  error
	workers   sync.WaitGroup
	output    outputRelay
	skipped   *skippedCompaction
	images    verifiedImages
}

type actor struct {
	t            *Thread
	records      []event.Seqd
	sizes        []int
	state        event.State
	damage       error
	subs         map[chan Update]struct{}
	busy         bool
	cancel       context.CancelCauseFunc
	approvals    map[string]*pendingApproval
	questions    map[string]*pendingQuestion
	questionSeq  uint64
	autoContinue bool
	hook         BoundaryHook
}

func (h *Host) load(id string) (loadedSession, error) {
	var out loadedSession
	var acc event.State
	err := h.journal.Replay(id, func(seq uint64, raw []byte) error {
		decoded, err := event.Decode(raw)
		if err != nil {
			return &journal.DamageError{Session: id, Seq: seq}
		}
		status := acc.Status
		if err := acc.Apply(seq, decoded); err != nil {
			acc.Status = status
			return &journal.DamageError{Session: id, Seq: seq}
		}
		if created, ok := decoded.(event.SessionCreated); ok && seq == 1 {
			out.created = created
		}
		out.records = append(out.records, event.Seqd{Seq: seq, Event: decoded})
		out.sizes = append(out.sizes, len(raw))
		return nil
	})
	if _, damaged := errors.AsType[*journal.DamageError](err); damaged {
		out.damage = err
		err = nil
	}
	out.state = acc
	if err != nil {
		return out, fmt.Errorf("agent: replay %s: %w", id, err)
	}
	if out.created.ID == "" {
		return out, fmt.Errorf("agent: session %s has no session.created", id)
	}
	return out, nil
}

func (h *Host) openThread(ctx context.Context, loaded loadedSession, overrides Overrides) (*Thread, error) {
	release, err := h.lockSession(loaded.created.ID)
	if err != nil {
		return nil, err
	}
	return h.openLocked(ctx, loaded, overrides, release)
}

func (h *Host) openLocked(ctx context.Context, loaded loadedSession, overrides Overrides, release func() error) (*Thread, error) {
	t := &Thread{
		host: h, id: loaded.created.ID, created: loaded.created, snapshots: snapshot.New(),
		inbox: make(chan func(*actor)), quit: make(chan struct{}), stopped: make(chan struct{}),
		children: make(chan struct{}, maxChildren), ext: baseExtension{}, release: release,
		output: outputRelay{ch: make(chan Update, outputBuffer)},
	}
	if err := t.prepare(ctx, loaded, overrides); err != nil {
		return nil, errors.Join(err, t.unlock())
	}
	h.mu.Lock()
	if existing, ok := h.threads[t.id]; ok {
		h.mu.Unlock()
		return existing, errors.Join(t.runner.Close(), t.unlock())
	}
	h.threads[t.id] = t
	h.mu.Unlock()
	a := &actor{
		t: t, records: loaded.records, sizes: loaded.sizes, state: loaded.state, damage: loaded.damage,
		subs: make(map[chan Update]struct{}), approvals: make(map[string]*pendingApproval), questions: make(map[string]*pendingQuestion), autoContinue: !endedByInterrupt(loaded.records),
	}
	if loaded.state.TurnID != "" && loaded.damage == nil {
		a.spawn(func(ctx context.Context) { (&worker{t: t, turnID: loaded.state.TurnID}).recoverTurn(ctx) })
	}
	go a.loop()
	return t, nil
}

func (h *Host) lockSession(id string) (func() error, error) {
	if h.cfg.Product != config.ProductDal {
		return func() error { return nil }, nil
	}
	release, err := h.journal.LockSession(id)
	if err != nil {
		return nil, fmt.Errorf("%w", err)
	}
	return release, nil
}

func endedByInterrupt(records []event.Seqd) bool {
	for _, item := range slices.Backward(records) {
		switch value := item.Event.(type) {
		case event.UserMessage:
			return false
		case event.TurnCompleted:
			return value.Status == statusInterrupted
		}
	}
	return false
}

func (t *Thread) unlock() error { return t.release() }

func (t *Thread) prepare(ctx context.Context, loaded loadedSession, overrides Overrides) error {
	t.fixedTools = overrides.Tools
	if t.fixedTools == nil && t.host.cfg.Extension != nil {
		t.ext = t.host.cfg.Extension(t)
	}
	system, err := t.systemPrompt(overrides.System)
	if err != nil {
		return err
	}
	t.system = system
	run, err := t.host.cfg.Runner(ctx, t.id, t.created.Runner)
	if err != nil {
		return fmt.Errorf("agent: runner %s: %w", t.created.Runner, err)
	}
	t.runner = run
	if err := t.restoreSnapshots(loaded.state); err != nil {
		return err
	}
	return nil
}

type toolset struct {
	model  string
	byName map[string]tool.Tool
	specs  []model.Tool
	names  []string
}

func (s *toolset) lookup(name string) (tool.Tool, bool) {
	if s == nil {
		return nil, false
	}
	item, ok := s.byName[name]
	return item, ok
}

func (t *Thread) Settings() (Settings, error) {
	v, err := t.view()
	if err != nil {
		return Settings{}, err
	}
	return pathSettings(t.created, v.records), nil
}

func (t *Thread) currentTools() *toolset {
	t.toolsMu.Lock()
	defer t.toolsMu.Unlock()
	return t.toolset
}

func (t *Thread) toolNames() []string {
	set := t.currentTools()
	if set == nil {
		return nil
	}
	return set.names
}

func (t *Thread) ensureTools(ctx context.Context) error {
	if t.currentTools() != nil {
		return nil
	}
	v, err := t.view()
	if err != nil {
		return err
	}
	_, err = t.toolsFor(ctx, pathSettings(t.created, v.records).Model)
	return err
}

func (t *Thread) toolsFor(ctx context.Context, modelID string) (*toolset, error) {
	if current := t.currentTools(); t.reuses(current, modelID) {
		return current, nil
	}
	tools, err := t.buildTools(ctx, modelID)
	if err != nil {
		return nil, err
	}
	built := t.newToolset(modelID, tools)
	t.toolsMu.Lock()
	defer t.toolsMu.Unlock()
	if t.reuses(t.toolset, modelID) {
		return t.toolset, nil
	}
	t.toolset = built
	return built, nil
}

func (t *Thread) reuses(set *toolset, modelID string) bool {
	return set != nil && (t.fixedTools != nil || set.model == modelID)
}

func (t *Thread) buildTools(ctx context.Context, modelID string) ([]tool.Tool, error) {
	if t.fixedTools != nil {
		return t.fixedTools, nil
	}
	built, err := t.host.cfg.Tools(ContextWithThread(ctx, t), modelID, t.created.Cwd)
	if err != nil {
		return nil, fmt.Errorf("agent: build tools: %w", err)
	}
	return t.ext.Tools(t.withThreadTools(built)), nil
}

func (t *Thread) newToolset(modelID string, tools []tool.Tool) *toolset {
	out := &toolset{model: modelID, byName: make(map[string]tool.Tool, len(tools))}
	specs := make([]tool.Spec, len(tools))
	for i, item := range tools {
		specs[i] = item.Spec()
		out.byName[specs[i].Name] = item
	}
	visible := t.ext.Visible(specs)
	out.specs = make([]model.Tool, len(visible))
	out.names = make([]string, len(visible))
	for i, spec := range visible {
		out.names[i] = spec.Name
		out.specs[i] = model.Tool{Name: spec.Name, Description: spec.Description, Schema: spec.Schema, Freeform: spec.Freeform}
	}
	return out
}

func (t *Thread) systemPrompt(override string) (string, error) {
	if override != "" {
		return override, nil
	}
	instructions, err := t.host.loadText("", t.created.InstructionsBlob)
	if err != nil {
		return "", err
	}
	in := prompt.Input{
		ToolNotes: t.host.cfg.ToolNotes, Instructions: instructions, CWD: t.created.Cwd,
		OS: runtime.GOOS, Shell: shellName(),
	}
	if t.host.cfg.Product == config.ProductDalgona {
		if in.Skills, err = t.host.Skills(t.created.Cwd); err != nil {
			return "", fmt.Errorf("agent: skills for %s: %w", t.created.Cwd, err)
		}
	}
	return prompt.Assemble(in), nil
}

func shellName() string {
	if runtime.GOOS == "windows" {
		return "cmd"
	}
	return cmp.Or(os.Getenv("SHELL"), "sh")
}

func (t *Thread) restoreSnapshots(state event.State) error {
	snapshots := make([]event.Snapshot, 0, len(state.Snapshots))
	for _, item := range state.Snapshots {
		snapshots = append(snapshots, item)
	}
	slices.SortFunc(snapshots, func(a, b event.Snapshot) int {
		return cmp.Or(cmp.Compare(a.Epoch, b.Epoch), cmp.Compare(a.Revision, b.Revision), cmp.Compare(a.Ref, b.Ref))
	})
	for _, item := range snapshots {
		masked, err := t.host.journal.Blob(item.Blob)
		if err != nil {
			t.host.log.Warn("agent: restore read snapshot", "thread", t.id, "ref", item.Ref, "error", err)
			continue
		}
		if digest := snapshot.Digest(masked); digest != item.Blob {
			t.host.log.Warn("agent: restore read snapshot", "thread", t.id, "ref", item.Ref, "error", "blob digest mismatch")
			continue
		}
		raw, err := t.host.unmaskStrict(string(masked))
		if err != nil {
			return fmt.Errorf("agent: restore snapshot %s: %w", item.Ref, err)
		}
		if err := t.snapshots.Restore(snapshotMetaWithDigest(item, snapshot.Digest([]byte(raw))), []byte(raw)); err != nil {
			t.host.log.Warn("agent: restore read snapshot", "thread", t.id, "ref", item.Ref, "error", err)
		}
	}
	return nil
}

func snapshotMetaWithDigest(item event.Snapshot, digest string) []byte {
	shown := make([]snapshot.Range, len(item.Shown))
	for i, line := range item.Shown {
		shown[i] = snapshot.Range{Start: line.Start, End: line.End}
	}
	entry := snapshot.Entry{Ref: item.Ref, Path: item.Path, Blob: digest, Shown: shown, Revision: item.Revision, Epoch: item.Epoch}
	return entry.Meta()
}

func (t *Thread) ID() string { return t.id }

func (t *Thread) TopLevel() bool { return t.created.Parent == "" && t.created.Advisor == "" }

func (a *actor) loop() {
	defer close(a.t.stopped)
	for {
		select {
		case fn := <-a.t.inbox:
			a.drainOutput()
			fn(a)
		case update := <-a.t.output.ch:
			a.publishOutput(update)
		case <-a.t.quit:
			a.shutdown()
			return
		}
	}
}

func (a *actor) shutdown() {
	if a.cancel != nil {
		a.cancel(ErrClosed)
	}
	for ch := range a.subs {
		close(ch)
	}
	a.subs = nil
}

func (t *Thread) do(fn func(*actor)) error {
	select {
	case t.inbox <- fn:
		return nil
	case <-t.quit:
		return ErrClosed
	}
}

func ask[T any](t *Thread, fn func(*actor) T) (T, error) {
	reply := make(chan T, 1)
	var zero T
	if err := t.do(func(a *actor) { reply <- fn(a) }); err != nil {
		return zero, err
	}
	select {
	case value := <-reply:
		return value, nil
	case <-t.stopped:
		return zero, ErrClosed
	}
}

func askErr(t *Thread, fn func(*actor) error) error {
	result, transport := ask(t, fn)
	return errors.Join(transport, result)
}

func (t *Thread) Close() error {
	t.closeOnce.Do(func() {
		close(t.quit)
		<-t.stopped
		t.workers.Wait()
		t.closeErr = errors.Join(t.runner.Close(), t.unlock())
		t.host.forget(t.id)
	})
	return t.closeErr
}

type appendResult struct {
	head uint64
	err  error
}

func (t *Thread) appendEvents(ctx context.Context, events ...event.Event) (uint64, error) {
	result, err := ask(t, func(a *actor) appendResult {
		head, err := a.appendCtx(ctx, events...)
		return appendResult{head: head, err: err}
	})
	return result.head, errors.Join(err, result.err)
}

func (a *actor) head() uint64 { return uint64(len(a.records)) }

func (a *actor) append(events ...event.Event) (uint64, error) {
	return a.appendCtx(context.Background(), events...)
}

func (a *actor) appendCtx(ctx context.Context, events ...event.Event) (uint64, error) {
	if a.damage != nil {
		return a.head(), fmt.Errorf("%w: %w", ErrDamaged, a.damage)
	}
	if len(events) == 0 {
		return a.head(), nil
	}
	events = stampTimes(events, a.t.host.clock().UTC())
	state, raws, err := a.prepareAppend(events)
	if err != nil {
		return a.head(), err
	}
	last, err := a.t.host.journal.Append(ctx, a.t.id, raws...)
	if last == a.head()+uint64(len(events)) {
		a.state = state
		for i, item := range events {
			seq := a.head() + 1
			a.records = append(a.records, event.Seqd{Seq: seq, Event: item})
			a.sizes = append(a.sizes, len(raws[i]))
			a.publish(Update{Kind: UpdateEvent, Thread: a.t.id, Seq: seq, Event: item})
		}
		a.t.host.noteAppend(a.t.id, a.head(), state)
		if err != nil {
			a.t.host.log.Warn("agent: journal mirror or volume sync failed after the fsynced append", "thread", a.t.id, "seq", last, "error", err)
		}
		return a.head(), nil
	}
	if err != nil {
		return a.head(), err
	}
	a.damage = fmt.Errorf("agent: append to %s: journal head %d after appending %d events at %d", a.t.id, last, len(events), a.head())
	return a.head(), a.damage
}

func (a *actor) prepareAppend(events []event.Event) (event.State, [][]byte, error) {
	acc := a.state.Clone()
	raws := make([][]byte, len(events))
	for i, item := range events {
		raw, err := event.Encode(item)
		if err != nil {
			return a.state, nil, err
		}
		if err := acc.Apply(a.head()+uint64(i)+1, item); err != nil {
			return a.state, nil, err
		}
		raws[i] = raw
	}
	return acc, raws, nil
}

func (a *actor) publish(update Update) {
	for ch := range a.subs {
		a.sendUpdate(ch, update)
	}
}

func (t *Thread) publish(update Update) {
	update.Thread = t.id
	if err := t.do(func(a *actor) { a.publish(update) }); err != nil {
		t.host.log.Debug("agent: drop update after close", "thread", t.id)
	}
}

func (t *Thread) Subscribe() <-chan Update {
	ch := make(chan Update, subscriberBuffer)
	if err := t.do(func(a *actor) { a.subscribe(ch) }); err != nil {
		close(ch)
	}
	return ch
}

func (t *Thread) Unsubscribe(ch <-chan Update) {
	if err := t.do(func(a *actor) { a.unsubscribe(ch) }); err != nil {
		t.host.log.Debug("agent: unsubscribe after close", "thread", t.id)
	}
}

func (a *actor) unsubscribe(target <-chan Update) {
	for ch := range a.subs {
		if ch == target {
			a.drop(ch)
		}
	}
	a.settleQuestions()
	a.settleApprovals()
}

type view struct {
	records []event.Seqd
	sizes   []int
	state   event.State
}

func (v view) head() uint64 { return uint64(len(v.records)) }

func (t *Thread) view() (view, error) {
	return ask(t, func(a *actor) view { return view{records: a.records, sizes: a.sizes, state: a.state} })
}

func (t *Thread) Status() (Status, error) {
	return ask(t, func(a *actor) Status {
		return Status{
			State: a.state.Status, Busy: a.busy, AutoContinue: a.autoContinue, Head: a.head(),
			Turn: a.state.TurnID, Subscribers: len(a.subs), Questions: len(a.questions), OutputDropped: a.t.output.dropped.Load(),
		}
	})
}
