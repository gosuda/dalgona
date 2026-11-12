package rpc

import (
	"cmp"
	"context"
	"encoding/json/jsontext"
	"errors"
	"io"
	"log/slog"
	"sync"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/advisor"
	"github.com/gosuda/dalgona/internal/agent/event"
)

type method func(s *server, ctx context.Context, params jsontext.Value) (jsontext.Value, error)

var methods map[string]method

func init() {
	methods = map[string]method{
		"initialize":                     (*server).initialize,
		"thread/start":                   (*server).threadStart,
		"thread/resume":                  (*server).threadResume,
		"thread/list":                    (*server).threadList,
		"thread/read":                    (*server).threadRead,
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
		"dalgona/keywords/list":          (*server).keywordsList,
		"dalgona/keywords/match":         (*server).keywordsMatch,
		"dalgona/thread/tree":            (*server).threadTree,
		"dalgona/thread/leaf/set":        (*server).leafSet,
		"dalgona/advisor/status":         (*server).advisorStatus,
		"dalgona/advisor/set":            (*server).advisorSet,
		"dalgona/advisor/reset":          (*server).advisorReset,
		"dalgona/settings/read":          (*server).settingsRead,
		"dalgona/item/output/read":       (*server).itemOutputRead,
	}
}

type server struct {
	host     *agent.Host
	opts     ServeOptions
	closing  bool
	conn     *Conn
	log      *slog.Logger
	mu       sync.Mutex
	watches  map[string]*watch
	procs    map[string]context.CancelFunc
	pending  map[string]context.CancelFunc
	logins   map[string]context.CancelFunc
	watchers sync.WaitGroup
	tasks    sync.WaitGroup
}

type watch struct {
	thread  *agent.Thread
	tracker *tracker
	head    uint64
	mu      sync.Mutex
	ch      <-chan agent.Update
	stopped bool
}

type Extension func(ctx context.Context, params jsontext.Value) (jsontext.Value, error)

func Serve(ctx context.Context, conn io.ReadWriteCloser, host *agent.Host) error {
	return ServeWith(ctx, conn, host, ServeOptions{})
}

type ServeOptions struct {
	Extensions    map[string]Extension
	BeforeArchive func(ctx context.Context, threadID string) error
	Logger        *slog.Logger
	Advisors      *advisor.Registry
}

func ServeWith(ctx context.Context, conn io.ReadWriteCloser, host *agent.Host, opts ServeOptions) error {
	if host == nil {
		return errors.New("rpc: serve needs a host")
	}
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	s := &server{
		host: host, log: cmp.Or(opts.Logger, slog.Default()), opts: opts,
		watches: map[string]*watch{}, pending: map[string]context.CancelFunc{}, logins: map[string]context.CancelFunc{},
	}
	s.conn = newConnection(ctx, conn, s.handle)
	s.conn.start()
	select {
	case <-ctx.Done():
	case <-s.conn.closed:
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
}

func (s *server) handle(ctx context.Context, name string, params jsontext.Value) (jsontext.Value, error) {
	if call, ok := methods[name]; ok {
		return call(s, ctx, params)
	}
	if extension, ok := s.opts.Extensions[name]; ok {
		result, err := extension(ctx, params)
		if err != nil {
			return nil, internalError(err)
		}
		return result, nil
	}
	if name == "initialized" {
		return nil, nil
	}
	return nil, &RPCError{Code: codeMethodNotFound, Message: "Method not found"}
}

func (s *server) send(method string, w writer) {
	value, err := encode(w)
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
	return newTracker(id, s.host.Blob, s.childCwd, s.host.KeywordMatches, s.log)
}

func (s *server) childCwd(id string) string {
	info, _ := s.info(id)
	return info.Cwd
}

type snapshot struct {
	created event.SessionCreated
	turns   []Turn
	usage   *ThreadTokenUsage
}

func (s *server) attach(ctx context.Context, thread *agent.Thread) (snapshot, error) {
	snap, fresh, err := s.watch(ctx, thread)
	if err != nil || fresh {
		return snap, err
	}
	return s.snapshot(thread)
}

func (s *server) watch(ctx context.Context, thread *agent.Thread) (snapshot, bool, error) {
	id := thread.ID()
	s.mu.Lock()
	_, watched := s.watches[id]
	w := &watch{thread: thread}
	if !watched {
		s.watches[id] = w
	}
	s.mu.Unlock()
	if watched {
		return snapshot{}, false, nil
	}
	ch := thread.Subscribe()
	if !w.setChannel(ch) {
		thread.Unsubscribe(ch)
		return snapshot{}, false, io.ErrClosedPipe
	}
	w.tracker = s.newTracker(id)
	snap, head, err := s.fill(thread, w.tracker)
	if err != nil {
		w.stop()
		s.forget(id)
		return snapshot{}, false, err
	}
	w.head = head
	if !s.startWatcher(ctx, w) {
		w.stop()
		return snapshot{}, false, io.ErrClosedPipe
	}
	s.autoAdvisor(ctx, thread)
	return snap, true, nil
}

func (s *server) startWatcher(ctx context.Context, w *watch) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closing {
		return false
	}
	s.watchers.Go(func() { s.run(ctx, w) })
	return true
}

func (s *server) forget(id string) {
	s.mu.Lock()
	delete(s.watches, id)
	s.mu.Unlock()
}

func (s *server) snapshot(thread *agent.Thread) (snapshot, error) {
	snap, _, err := s.fill(thread, s.newTracker(thread.ID()))
	return snap, err
}

func (s *server) fill(thread *agent.Thread, tr *tracker) (snapshot, uint64, error) {
	records, _, err := thread.Records()
	if err != nil {
		return snapshot{}, 0, hostError(err, thread.ID(), "")
	}
	c := &collector{}
	tr.sink = c
	if len(records) > 0 {
		if created, ok := records[0].Event.(event.SessionCreated); ok {
			tr.created = created
		}
	}
	for _, e := range event.ActivePath(records) {
		tr.apply(e.Seq, e.Event)
	}
	var head uint64
	if len(records) > 0 {
		head = records[len(records)-1].Seq
	}
	return snapshot{created: tr.created, turns: c.result(), usage: tr.usageSnapshot()}, head, nil
}

func (s *server) run(ctx context.Context, w *watch) {
	defer s.forget(w.thread.ID())
	w.tracker.sink = liveSink{s: s, thread: w.thread.ID()}
	ch := w.channel()
	for {
		s.drain(ctx, w, ch)
		if ctx.Err() != nil {
			return
		}
		ch = w.thread.Subscribe()
		if !w.setChannel(ch) {
			w.thread.Unsubscribe(ch)
			return
		}
		if err := s.catchUp(w); err != nil {
			return
		}
	}
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
		case agent.UpdateTextDelta:
			w.tracker.agentDelta(update.ItemID, update.Delta)
		case agent.UpdateReasoningDelta:
			w.tracker.reasoningDelta(update.ItemID, update.Delta)
		case agent.UpdateApproval:
			s.ask(ctx, w, update)
		case agent.UpdateExecOutput:
			w.tracker.execOutput(update.CallID, update.Delta)
		case agent.UpdateRetryWait:
			w.tracker.retryWait(update.Turn, update.Wait)
		}
	}
}

func (s *server) applyUpdate(w *watch, update agent.Update) {
	if update.Seq <= w.head {
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
	turn  Turn
	items []*Item
}

func (c *collector) find(id string) *turnBuild {
	for i := len(c.turns) - 1; i >= 0; i-- {
		if c.turns[i].turn.ID == id {
			return c.turns[i]
		}
	}
	return nil
}

func (c *collector) turn(method string, turn Turn) {
	if method == methodTurnStarted {
		c.turns = append(c.turns, &turnBuild{turn: turn})
		return
	}
	if b := c.find(turn.ID); b != nil {
		b.turn.Status, b.turn.Error = turn.Status, turn.Error
		b.turn.CompletedAt, b.turn.DurationMs = turn.CompletedAt, turn.DurationMs
	}
}

func (c *collector) item(method, turnID string, it *Item) {
	if method != methodItemStarted {
		return
	}
	if b := c.find(turnID); b != nil {
		b.items = append(b.items, it)
	}
}

func (c *collector) notify(string, func() writer) {}

func (c *collector) result() []Turn {
	turns := make([]Turn, 0, len(c.turns))
	for _, b := range c.turns {
		turn := b.turn
		turn.Items = make([]Item, 0, len(b.items))
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
}

func (l liveSink) turn(method string, turn Turn) {
	l.s.send(method, TurnStartedNotification{ThreadID: l.thread, Turn: turn})
}

func (l liveSink) item(method, turnID string, it *Item) {
	now := nowMillis()
	if method == methodItemStarted {
		l.s.send(method, ItemStartedNotification{ThreadID: l.thread, TurnID: turnID, StartedAtMs: now, Item: *it})
		return
	}
	l.s.send(method, ItemCompletedNotification{ThreadID: l.thread, TurnID: turnID, CompletedAtMs: now, Item: *it})
}

func (l liveSink) notify(method string, build func() writer) { l.s.send(method, build()) }
