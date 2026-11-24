package rpcserver

import (
	"errors"
	"io"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/runner"
)

const (
	terminalRunning = "running"
	terminalExited  = "exited"
	terminalClosed  = "closed"
	inputReady      = "ready"
	inputUncertain  = "uncertain"
	inputClosed     = "closed"
)

type terminalIdentity struct {
	threadID        string
	runnerID        string
	bindingRevision string
	root            string
}

type terminalFinal struct {
	offset    uint64
	ordinal   uint64
	truncated bool
	expiresAt time.Time
}

type terminal struct {
	host     *TerminalHost
	id       string
	identity terminalIdentity
	proc     runner.Terminal
	screen   TerminalScreen
	input    sync.Mutex
	pumped   chan struct{}
	finished chan struct{}
	stopped  chan struct{}
	stopOnce sync.Once

	mu          sync.Mutex
	output      outputRing
	resizes     resizeRing
	columns     int
	rows        int
	attachments map[string]*attachment
	writer      *writerLease
	inputState  string
	state       string
	exit        *runner.Exit
	lost        bool
	unconfirmed bool
	screenFault error
	final       *terminalFinal
	changed     chan struct{}
	stopAt      time.Time
	timer       *time.Timer
	released    bool
}

func newTerminal(host *TerminalHost, identity terminalIdentity, proc runner.Terminal, columns, rows int) *terminal {
	t := &terminal{
		host: host, id: newHandle(), identity: identity, proc: proc, screen: host.engine.New(columns, rows),
		pumped: make(chan struct{}), finished: make(chan struct{}), stopped: make(chan struct{}),
		columns: columns, rows: rows, attachments: map[string]*attachment{},
		inputState: inputReady, state: terminalRunning, changed: make(chan struct{}),
	}
	t.timer = time.AfterFunc(terminalLease, t.expire)
	return t
}

func (t *terminal) start() {
	go t.pump()
	go t.watchExit()
}

func (t *terminal) notifyLocked() {
	close(t.changed)
	t.changed = make(chan struct{})
}

func (t *terminal) pump() {
	defer close(t.pumped)
	chunk := make([]byte, terminalChunk)
	for {
		n, err := t.proc.Read(chunk)
		if n > 0 {
			t.consume(chunk[:n])
		}
		if err != nil {
			t.outputEnded(err)
			return
		}
	}
}

func (t *terminal) consume(data []byte) {
	t.mu.Lock()
	if t.final != nil || t.released {
		t.mu.Unlock()
		return
	}
	t.output.append(data)
	if _, err := t.screen.Write(data); err != nil && t.screenFault == nil {
		t.screenFault = err
	}
	replies := t.screen.TakeReplies()
	t.notifyLocked()
	t.mu.Unlock()
	if len(replies) > 0 {
		t.dispatchReply(replies)
	}
}

func (t *terminal) outputEnded(err error) {
	if errors.Is(err, io.EOF) {
		return
	}
	t.mu.Lock()
	t.lost = true
	t.mu.Unlock()
}

func (t *terminal) dispatchReply(replies []byte) {
	t.input.Lock()
	defer t.input.Unlock()
	t.mu.Lock()
	ready := t.inputState == inputReady
	t.mu.Unlock()
	if !ready {
		return
	}
	if n, err := t.proc.Write(replies); err != nil || n != len(replies) {
		t.markUncertain()
	}
}

func (t *terminal) markUncertain() {
	t.mu.Lock()
	if t.inputState == inputReady {
		t.inputState = inputUncertain
	}
	t.notifyLocked()
	t.mu.Unlock()
}

func (t *terminal) watchExit() {
	<-t.proc.Done()
	exit := t.proc.Exit()
	t.mu.Lock()
	t.exit = &exit
	if t.state == terminalRunning {
		t.state = terminalExited
	}
	t.closeInputLocked()
	t.notifyLocked()
	t.mu.Unlock()
	t.finalize(t.awaitDrain())
}

func (t *terminal) awaitDrain() bool {
	drain := time.NewTimer(terminalDrain)
	defer drain.Stop()
	select {
	case <-t.pumped:
		return false
	case <-drain.C:
	}
	_ = t.proc.Close()
	settle := time.NewTimer(terminalCloseSettle)
	defer settle.Stop()
	select {
	case <-t.pumped:
	case <-settle.C:
	}
	return true
}

func (t *terminal) finalize(truncated bool) {
	now := time.Now()
	t.mu.Lock()
	t.final = &terminalFinal{
		offset: t.output.next(), ordinal: t.resizes.latest(), truncated: truncated || t.lost, expiresAt: now.Add(terminalRetention),
	}
	for _, att := range t.attachments {
		att.deadline = t.leaseDeadline(att.deadline)
	}
	t.scheduleLocked(now)
	t.notifyLocked()
	close(t.finished)
	t.mu.Unlock()
	_ = t.proc.Close()
}

func (t *terminal) closeInputLocked() {
	t.inputState = inputClosed
	if t.writer == nil {
		return
	}
	if t.writer.holder != nil {
		t.writer.holder.writer = nil
	}
	t.writer = nil
}

func (t *terminal) requestClose() {
	t.mu.Lock()
	if t.state == terminalRunning {
		t.state = terminalClosed
	}
	t.closeInputLocked()
	t.notifyLocked()
	t.mu.Unlock()
	t.stopOnce.Do(func() { go t.terminate() })
}

func (t *terminal) terminate() {
	defer close(t.stopped)
	if t.awaitExit(0) {
		return
	}
	_ = t.proc.Signal(runner.SignalTerm)
	if t.awaitExit(terminalGraceful) {
		return
	}
	_ = t.proc.Signal(runner.SignalKill)
	if t.awaitExit(terminalDrain) {
		return
	}
	t.mu.Lock()
	t.unconfirmed = true
	t.mu.Unlock()
}

func (t *terminal) awaitExit(limit time.Duration) bool {
	timer := time.NewTimer(limit)
	defer timer.Stop()
	select {
	case <-t.proc.Done():
		return true
	case <-timer.C:
		select {
		case <-t.proc.Done():
			return true
		default:
			return false
		}
	}
}

func (t *terminal) release() {
	t.mu.Lock()
	if t.released {
		t.mu.Unlock()
		return
	}
	t.released = true
	detached := t.dropAttachmentsLocked(func(*attachment) bool { return true })
	t.timer.Stop()
	t.output = outputRing{}
	t.resizes = resizeRing{}
	t.notifyLocked()
	t.mu.Unlock()
	t.host.remove(t.id)
	releaseWorkspaces(detached)
	_ = t.proc.Close()
}

type terminalStatus struct {
	state      string
	inputState string
	exit       *runner.Exit
	lost       bool
	final      *terminalFinal
}

func (t *terminal) statusLocked() terminalStatus {
	return terminalStatus{state: t.state, inputState: t.inputState, exit: t.exit, lost: t.lost, final: t.final}
}

func (s terminalStatus) Encode(o *rpc.Writer) {
	o.Open()
	o.Str("state", s.state)
	o.Str("inputState", s.inputState)
	if s.exit != nil && !s.lost {
		o.Int("exitCode", int64(s.exit.Code))
		o.OptStr("signal", s.exit.Signal)
	}
	if s.final != nil {
		o.Str("finalOffset", rpc.FormatDecimal(s.final.offset))
		o.Str("finalResizeOrdinal", rpc.FormatDecimal(s.final.ordinal))
		o.Bool("outputTruncated", s.final.truncated)
		o.Str("outputExpiresAt", rpc.FormatTimestamp(s.final.expiresAt))
	}
	o.Close()
}

func (i terminalIdentity) encode(o *rpc.Writer, id string) {
	o.Open()
	o.Str("terminalId", id)
	o.Str("daemonInstanceId", daemonInstance())
	o.Str("threadId", i.threadID)
	o.Str("runnerId", i.runnerID)
	o.Str("bindingRevision", i.bindingRevision)
	o.Str("root", i.root)
	o.Close()
}
