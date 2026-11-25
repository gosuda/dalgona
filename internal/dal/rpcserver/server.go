package rpcserver

import (
	"cmp"
	"context"
	"encoding/json/jsontext"
	"errors"
	"io"
	"log/slog"
	"maps"
	"slices"
	"sync"
	"sync/atomic"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/advisor"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/auth/oauth"
)

type method func(s *server, ctx context.Context, params jsontext.Value) (jsontext.Value, error)

var methods map[string]method

func init() {
	methods = map[string]method{
		"thread/start":                   (*server).threadStart,
		"thread/resume":                  (*server).threadResume,
		"thread/list":                    (*server).threadList,
		"thread/read":                    (*server).threadRead,
		"thread/turns/list":              (*server).threadTurnsList,
		"thread/name/set":                (*server).threadNameSet,
		"thread/archive":                 (*server).threadArchive,
		"thread/unarchive":               (*server).threadUnarchive,
		"thread/compact/start":           (*server).threadCompact,
		"thread/revert":                  (*server).threadRevert,
		"thread/fork":                    (*server).threadFork,
		"turn/start":                     (*server).turnStart,
		"turn/steer":                     (*server).turnSteer,
		"turn/interrupt":                 (*server).turnInterrupt,
		"review/start":                   (*server).reviewStart,
		"model/list":                     (*server).modelList,
		"config/read":                    (*server).configRead,
		"config/value/write":             (*server).configValueWrite,
		"config/batchWrite":              (*server).configBatchWrite,
		"account/read":                   (*server).accountRead,
		"account/login/start":            (*server).loginStart,
		"account/login/cancel":           (*server).loginCancel,
		"dalgona/account/providers/list": (*server).accountProviders,
		"account/logout":                 (*server).logout,
		"skills/list":                    (*server).skillsList,
		"mcpServerStatus/list":           (*server).mcpServerStatusList,
		"command/exec":                   (*server).commandExec,
		"command/exec/terminate":         (*server).commandExecTerminate,
		"fs/readFile":                    (*server).fsReadFile,
		"fs/writeFile":                   (*server).fsWriteFile,
		"fs/createDirectory":             (*server).fsCreateDirectory,
		"fs/getMetadata":                 (*server).fsGetMetadata,
		"fs/readDirectory":               (*server).fsReadDirectory,
		"fs/remove":                      (*server).fsRemove,
		"fs/copy":                        (*server).fsCopy,
		"fs/watch":                       (*server).fsWatch,
		"fs/unwatch":                     (*server).fsUnwatch,
		"fuzzyFileSearch":                (*server).fuzzyFileSearch,
		"dalgona/keywords/list":          (*server).keywordsList,
		"dalgona/keywords/match":         (*server).keywordsMatch,
		"dalgona/thread/tree":            (*server).threadTree,
		"dalgona/thread/leaf/set":        (*server).leafSet,
		"dalgona/thread/search":          (*server).threadSearch,
		"dalgona/advisor/status":         (*server).advisorStatus,
		"dalgona/advisor/set":            (*server).advisorSet,
		"dalgona/advisor/reset":          (*server).advisorReset,
		"dalgona/settings/read":          (*server).settingsRead,
		"dalgona/item/output/read":       (*server).itemOutputRead,
	}
	maps.Copy(methods, capabilityMethods())
	maps.Copy(methods, historyMethods())
	maps.Copy(methods, decisionMethods())
	maps.Copy(methods, operationMethods())
	maps.Copy(methods, imageMethods())
	maps.Copy(methods, failureMethods())
	maps.Copy(methods, workspaceMethods())
	maps.Copy(methods, terminalMethods())
	coverMutations(methods)
}

type server struct {
	host       *agent.Host
	opts       ServeOptions
	providers  func(id string) (oauth.Provider, bool)
	closing    bool
	conn       *rpc.Conn
	log        *slog.Logger
	mu         sync.Mutex
	watches    map[string]*watch
	watchSlots int
	procs      map[string]context.CancelFunc
	pending    map[string]context.CancelFunc
	threads    map[string]string
	refused    map[string]struct{}
	logins     map[string]context.CancelFunc
	searches   map[string]*fuzzySearch
	watchers   sync.WaitGroup
	tasks      sync.WaitGroup
	fsWatches  map[string]*fsWatcher
	fsWatchers sync.WaitGroup
	ctx        context.Context
	connection string
	native     atomic.Pointer[nativeSession]
	workspace  workspaceTable
	terminalMu sync.Mutex
}

type watch struct {
	thread     *agent.Thread
	tracker    *tracker
	head       uint64
	ready      chan struct{}
	initErr    error
	overflowed func() bool
	mu         sync.Mutex
	ch         <-chan agent.Update
	stopped    bool
	apply      sync.Mutex
	live       liveProjection
}

type Extension func(ctx context.Context, params jsontext.Value) (jsontext.Value, error)

type ServeOptions struct {
	Extensions          map[string]Extension
	OperationExtensions map[string]Extension
	OperationResults    map[string]Extension
	BeforeArchive       func(ctx context.Context, threadID string) error
	Logger              *slog.Logger
	Advisors            *advisor.Registry
	ProviderLookup      func(id string) (oauth.Provider, bool)
	Terminals           *TerminalHost
}

func ServeWith(ctx context.Context, conn io.ReadWriteCloser, host *agent.Host, opts ServeOptions) error {
	if host == nil {
		return errors.New("rpc: serve needs a host")
	}
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	providerLookup := opts.ProviderLookup
	if providerLookup == nil {
		providerLookup = oauth.ProviderByID
	}
	s := &server{
		host: host, log: cmp.Or(opts.Logger, slog.Default()), opts: opts, providers: providerLookup,
		watches: map[string]*watch{}, pending: map[string]context.CancelFunc{}, threads: map[string]string{}, refused: map[string]struct{}{}, logins: map[string]context.CancelFunc{},
		fsWatches: map[string]*fsWatcher{}, ctx: ctx, connection: newHandle(),
	}
	s.conn = rpc.NewServerConn(ctx, conn, s.handle)
	s.conn.Start()
	select {
	case <-ctx.Done():
	case <-s.conn.Done():
	}
	cancel()
	s.shutdown()
	if err := s.conn.Close(); err != nil {
		s.log.Debug("rpc: close served connection", "error", err)
	}
	return nil
}

func (s *server) shutdown() {
	s.mu.Lock()
	s.closing = true
	for _, stop := range s.logins {
		stop()
	}
	for _, cancel := range s.procs {
		cancel()
	}
	s.mu.Unlock()
	s.tasks.Wait()
	s.mu.Lock()
	watches := make([]*watch, 0, len(s.watches))
	for _, w := range s.watches {
		watches = append(watches, w)
	}
	s.mu.Unlock()
	for _, w := range watches {
		w.stop()
	}
	s.watchers.Wait()
	s.releaseNative(watches)
	s.stopFsWatches()
	s.workspace.closeAll()
}

func (s *server) handle(ctx context.Context, name string, params jsontext.Value) (jsontext.Value, error) {
	if call, ok := methods[name]; ok {
		return s.invoke(ctx, name, call, params)
	}
	if call, ok := s.extension(name); ok {
		return s.invoke(context.WithValue(ctx, serverKey{}, s), name, call, params)
	}
	if name == "initialized" {
		return nil, nil
	}
	return nil, &rpc.RPCError{Code: rpc.CodeMethodNotFound, Message: "Method not found"}
}

func (s *server) send(method string, w rpc.Encoder) {
	value, err := rpc.Marshal(w)
	if err != nil {
		s.log.Warn("rpc: encode notification", "method", method, "error", err)
		return
	}
	if err := s.conn.Notify(method, value); err != nil {
		s.log.Debug("rpc: drop notification", "method", method, "error", err)
	}
}

func (w *watch) setChannel(ch <-chan agent.Update) bool {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.stopped {
		return false
	}
	w.ch = ch
	return true
}

func (w *watch) channel() <-chan agent.Update {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.ch
}

func (s *server) refuseRequest(requestID string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.refused[requestID] = struct{}{}
}

func (s *server) requestRefused(requestID string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	_, refused := s.refused[requestID]
	return refused
}

func (s *server) unrefuseRequest(requestID string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.refused, requestID)
}

func (w *watch) stop() {
	w.mu.Lock()
	w.stopped = true
	ch := w.ch
	w.mu.Unlock()
	w.thread.Unsubscribe(ch)
}

func (s *server) watching(id string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	_, ok := s.watches[id]
	return ok
}

func (s *server) newTracker(id string) *tracker {
	t := newTracker(id, s.host.Blob, s.childCwd, s.host.KeywordMatches, s.log)
	t.price = s.host.PriceOf
	return t
}

func (s *server) childCwd(id string) string {
	info, _ := s.info(id)
	return info.Cwd
}

type snapshot struct {
	created    event.SessionCreated
	turns      []rpc.Turn
	usage      *rpc.ThreadTokenUsage
	turnEffort *string
}

func (s *server) attach(ctx context.Context, thread *agent.Thread, replay replayer, slot *subscriptionReservation) (snapshot, error) {
	snap, fresh, err := s.watchWithReservation(ctx, thread, replay, slot)
	if err != nil || fresh {
		return snap, err
	}
	return s.snapshot(thread, replay)
}

func (s *server) watch(ctx context.Context, thread *agent.Thread, replay replayer) (snapshot, bool, error) {
	return s.watchWithReservation(ctx, thread, replay, nil)
}

type subscriptionReservation struct{ held bool }

func (s *server) reserveSubscription() (*subscriptionReservation, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closing {
		return nil, io.ErrClosedPipe
	}
	if s.native.Load() == nil {
		return nil, nil
	}
	if s.subscriptionsFullLocked() {
		return nil, rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	s.watchSlots++
	return &subscriptionReservation{held: true}, nil
}

func (s *server) releaseSubscription(slot *subscriptionReservation) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.releaseSubscriptionLocked(slot)
}

func (s *server) releaseSubscriptionLocked(slot *subscriptionReservation) {
	if slot != nil && slot.held {
		slot.held = false
		s.watchSlots--
	}
}

func (s *server) subscriptionsFullLocked() bool {
	return len(s.watches)+s.watchSlots >= connectionSubscriptions
}

func (s *server) registerWatch(thread *agent.Thread, slot *subscriptionReservation) (*watch, bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.releaseSubscriptionLocked(slot)
	if s.closing {
		return nil, false, io.ErrClosedPipe
	}
	id := thread.ID()
	if w := s.watches[id]; w != nil {
		return w, false, nil
	}
	if s.native.Load() != nil && s.subscriptionsFullLocked() {
		return nil, false, rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	w := &watch{thread: thread, live: liveProjection{threadID: id}, ready: make(chan struct{})}
	s.watches[id] = w
	return w, true, nil
}

func (w *watch) waitReady(ctx context.Context) error {
	select {
	case <-w.ready:
		return w.initErr
	case <-ctx.Done():
		return ctx.Err()
	}
}

func (s *server) watchWithReservation(ctx context.Context, thread *agent.Thread, replay replayer, slot *subscriptionReservation) (snapshot, bool, error) {
	w, fresh, err := s.registerWatch(thread, slot)
	if err != nil {
		return snapshot{}, false, err
	}
	if !fresh {
		return snapshot{}, false, w.waitReady(ctx)
	}
	snap, err := s.initializeWatch(ctx, w, replay)
	w.initErr = err
	close(w.ready)
	if err != nil {
		w.stop()
		w.closeProjection()
		s.forget(w)
		return snapshot{}, false, err
	}
	s.autoAdvisor(ctx, thread)
	return snap, true, nil
}

func (s *server) initializeWatch(ctx context.Context, w *watch, replay replayer) (snapshot, error) {
	w.tracker = s.newTracker(w.thread.ID())
	snap, err := s.loadWatch(ctx, w, replay)
	if err != nil {
		return snapshot{}, err
	}
	w.tracker.deferred = s.native.Load() != nil
	if !s.startWatcher(w) {
		return snapshot{}, io.ErrClosedPipe
	}
	return snap, nil
}

func (s *server) loadWatch(ctx context.Context, w *watch, replay replayer) (snapshot, error) {
	if s.native.Load() != nil {
		return s.loadSeededWatch(ctx, w, replay)
	}
	if err := w.listen(w.thread.Subscribe()); err != nil {
		return snapshot{}, err
	}
	snap, head, err := s.fill(w.thread, w.tracker, replay)
	w.head = head
	return snap, err
}

func (w *watch) listen(ch <-chan agent.Update) error {
	if !w.setChannel(ch) {
		w.thread.Unsubscribe(ch)
		return io.ErrClosedPipe
	}
	return nil
}

func (s *server) loadSeededWatch(ctx context.Context, w *watch, replay replayer) (snapshot, error) {
	sub, err := w.thread.SubscribeSeeded(ctx)
	if err != nil {
		return snapshot{}, err
	}
	defer sub.Release()
	if listenErr := w.listen(sub.Updates); listenErr != nil {
		return snapshot{}, listenErr
	}
	records, _, err := w.thread.Records()
	if err != nil {
		return snapshot{}, hostError(err, w.thread.ID(), "")
	}
	w.overflowed = sub.Overflowed
	w.head = sub.Head
	snap := replayRecords(w.tracker, replay, records[:min(sub.Head, uint64(len(records)))])
	return snap, w.seedProjection(ctx, sub.Seed)
}

func (s *server) startWatcher(w *watch) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closing || s.watches[w.thread.ID()] != w {
		return false
	}
	s.watchers.Go(func() { s.run(s.ctx, w) })
	return true
}

func (s *server) forget(w *watch) {
	s.mu.Lock()
	if s.watches[w.thread.ID()] == w {
		delete(s.watches, w.thread.ID())
	}
	s.mu.Unlock()
}

func (s *server) snapshot(thread *agent.Thread, replay replayer) (snapshot, error) {
	snap, _, err := s.fill(thread, s.newTracker(thread.ID()), replay)
	return snap, err
}

func (s *server) fill(thread *agent.Thread, tr *tracker, replay replayer) (snapshot, uint64, error) {
	records, _, err := thread.Records()
	if err != nil {
		return snapshot{}, 0, hostError(err, thread.ID(), "")
	}
	var head uint64
	if len(records) > 0 {
		head = records[len(records)-1].Seq
	}
	return replayRecords(tr, replay, records), head, nil
}

func replayRecords(tr *tracker, replay replayer, records []event.Seqd) snapshot {
	if created, ok := sessionCreated(records); ok {
		tr.created = created
	}
	turns := replay(tr, event.ActivePath(records))
	return snapshot{created: tr.created, turns: turns, usage: tr.usageSnapshot(), turnEffort: tr.turnEffort}
}

func (s *server) run(ctx context.Context, w *watch) {
	defer s.forget(w)
	defer w.closeProjection()
	w.tracker.sink = liveSink{s: s, thread: w.thread.ID(), w: w}
	ch := w.channel()
	for {
		s.drain(ctx, w, ch)
		if ctx.Err() != nil {
			return
		}
		if w.overflowed != nil {
			if w.overflowed() {
				s.closeStream(errors.New("native subscription overflow"))
			}
			return
		}
		ch = w.thread.Subscribe()
		if err := w.listen(ch); err != nil {
			return
		}
		if err := s.catchUp(w); err != nil {
			return
		}
	}
}

func (w *watch) closeProjection() {
	w.apply.Lock()
	defer w.apply.Unlock()
	w.live.close()
}

func (s *server) catchUp(w *watch) error {
	read, err := w.thread.ReadAfter(w.head)
	if err != nil {
		return err
	}
	for _, e := range read.Events {
		s.applyUpdate(w, agent.Update{Kind: agent.UpdateEvent, Seq: e.Seq, Event: e.Event})
	}
	return nil
}

func (s *server) drain(ctx context.Context, w *watch, ch <-chan agent.Update) {
	for update := range ch {
		switch update.Kind {
		case agent.UpdateEvent:
			s.applyUpdate(w, update)
		case agent.UpdateApproval:
			s.ask(ctx, w, ch, update)
		case agent.UpdateQuestion:
			s.question(ctx, w, ch, update)
		case agent.UpdateWarning:
			s.send("warning", rpc.WarningNotification{ThreadID: update.Thread, Message: update.Warning})
		case agent.UpdateTextDelta, agent.UpdateReasoningDelta, agent.UpdateExecOutput, agent.UpdateRetryWait:
			w.trackDelta(update)
		}
	}
}

func (w *watch) trackDelta(update agent.Update) {
	w.apply.Lock()
	defer w.apply.Unlock()
	if w.live.closed {
		return
	}
	switch update.Kind {
	case agent.UpdateTextDelta:
		w.tracker.agentDelta(update.ItemID, update.Delta)
	case agent.UpdateReasoningDelta:
		w.tracker.reasoningDelta(update.ItemID, update.Delta)
	case agent.UpdateExecOutput:
		w.tracker.execOutput(update.CallID, update.Delta)
	case agent.UpdateRetryWait:
		w.tracker.retryWait(update.Turn, update.Wait)
	case agent.UpdateEvent, agent.UpdateApproval, agent.UpdateQuestion, agent.UpdateWarning:
	}
}

func (s *server) applyUpdate(w *watch, update agent.Update) {
	w.apply.Lock()
	defer w.apply.Unlock()
	if update.Seq <= w.head || w.live.closed {
		return
	}
	w.head = update.Seq
	w.tracker.apply(update.Seq, update.Event)
	if resolved, ok := update.Event.(event.ApprovalResolved); ok {
		s.settle(resolved.RequestID)
	}
}

type collector struct {
	turns []*turnBuild
}

type turnBuild struct {
	turn  rpc.Turn
	items []*rpc.Item
}

func (c *collector) find(id string) *turnBuild {
	for _, b := range slices.Backward(c.turns) {
		if b.turn.ID == id {
			return b
		}
	}
	return nil
}

func (c *collector) turn(method string, turn rpc.Turn) {
	if method == methodTurnStarted {
		c.turns = append(c.turns, &turnBuild{turn: turn})
		return
	}
	if b := c.find(turn.ID); b != nil {
		b.turn.Status, b.turn.Error = turn.Status, turn.Error
		b.turn.CompletedAt, b.turn.DurationMs = turn.CompletedAt, turn.DurationMs
	}
}

func (c *collector) item(method, turnID string, it *rpc.Item) {
	if method != methodItemStarted {
		return
	}
	if b := c.find(turnID); b != nil {
		b.items = append(b.items, it)
	}
}

func (c *collector) notify(string, func() rpc.Encoder) {}

func (c *collector) result() []rpc.Turn {
	turns := make([]rpc.Turn, 0, len(c.turns))
	for _, b := range c.turns {
		turn := b.turn
		turn.Items = make([]rpc.Item, 0, len(b.items))
		for _, it := range b.items {
			turn.Items = append(turn.Items, *it)
		}
		turns = append(turns, turn)
	}
	return turns
}

type liveSink struct {
	s      *server
	thread string
	w      *watch
}

func (l liveSink) turn(method string, turn rpc.Turn) {
	if method == methodTurnCompleted && l.s.native.Load() != nil {
		l.w.live.endTurn(turn.ID)
	}
	l.s.send(method, rpc.TurnStartedNotification{ThreadID: l.thread, Turn: turn})
}

func (l liveSink) item(method, turnID string, it *rpc.Item) {
	now := nowMillis()
	if l.s.native.Load() != nil {
		l.s.nativeItem(l.w, method, turnID, it, now)
		return
	}
	if method == methodItemStarted {
		l.s.send(method, rpc.ItemStartedNotification{ThreadID: l.thread, TurnID: turnID, StartedAtMs: now, Item: *it})
		return
	}
	l.s.send(method, rpc.ItemCompletedNotification{ThreadID: l.thread, TurnID: turnID, CompletedAtMs: now, Item: *it})
}

func (l liveSink) notify(method string, build func() rpc.Encoder) {
	if l.s.native.Load() != nil {
		l.s.nativeNotify(l.w, method, build())
		return
	}
	l.s.send(method, build())
}
