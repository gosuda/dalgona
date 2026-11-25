package rpcserver

import (
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const (
	terminalColumnsMin      = 20
	terminalColumnsMax      = 240
	terminalRowsMin         = 5
	terminalRowsMax         = 120
	terminalAttachmentLimit = 4
	daemonTerminals         = 16
	terminalListLimit       = 16
	terminalOutputRing      = 8 << 20
	terminalReadDefault     = 16 << 10
	terminalReadMax         = 64 << 10
	terminalWaitMax         = 1000
	terminalResizeRecords   = 1024
	terminalResizeBytes     = 256 << 10
	terminalReadResizes     = 64
	terminalReadResizeBytes = 16 << 10
	terminalInputLimit      = 16 << 10
	terminalSequenceMemory  = 256
	checkpointReadDefault   = 64 << 10
	checkpointReadMax       = 256 << 10
	checkpointBodyLimit     = 8 << 20
	checkpointBudget        = 128 << 20
	checkpointLifetime      = 60 * time.Second
	terminalLease           = 30 * time.Second
	terminalRetention       = 60 * time.Second
	terminalGraceful        = 2 * time.Second
	terminalDrain           = 3 * time.Second
	terminalCloseSettle     = time.Second
	terminalChunk           = 32 << 10
)

type TerminalScreen interface {
	Write(p []byte) (int, error)
	Resize(columns, rows int)
	Checkpoint() ([]byte, error)
	TakeReplies() []byte
}

type TerminalEngine struct {
	Version       int
	EngineVersion string
	New           func(columns, rows int) TerminalScreen
}

type TerminalHost struct {
	engine          TerminalEngine
	mu              sync.Mutex
	terminals       map[string]*terminal
	reserved        int
	checkpointBytes int
}

func NewTerminalHost(engine TerminalEngine) *TerminalHost {
	return &TerminalHost{engine: engine, terminals: map[string]*terminal{}}
}

func (h *TerminalHost) reserve() error {
	h.mu.Lock()
	defer h.mu.Unlock()
	if len(h.terminals)+h.reserved >= daemonTerminals {
		return rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	h.reserved++
	return nil
}

func (h *TerminalHost) unreserve() {
	h.mu.Lock()
	h.reserved--
	h.mu.Unlock()
}

func (h *TerminalHost) insert(t *terminal) {
	h.mu.Lock()
	h.reserved--
	h.terminals[t.id] = t
	h.mu.Unlock()
}

func (h *TerminalHost) remove(id string) {
	h.mu.Lock()
	delete(h.terminals, id)
	h.mu.Unlock()
}

func (h *TerminalHost) lookup(id string) (*terminal, error) {
	h.mu.Lock()
	t, ok := h.terminals[id]
	h.mu.Unlock()
	if !ok {
		return nil, rpc.NativeKind(rpc.ErrorExpired)
	}
	return t, nil
}

func (h *TerminalHost) all() []*terminal {
	h.mu.Lock()
	defer h.mu.Unlock()
	list := make([]*terminal, 0, len(h.terminals))
	for _, t := range h.terminals {
		list = append(list, t)
	}
	return list
}

func (h *TerminalHost) connectionTerminals(s *server) int {
	count := 0
	for _, t := range h.all() {
		if t.attachedBy(s) {
			count++
		}
	}
	return count
}

func (h *TerminalHost) chargeCheckpoint(size int) error {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.checkpointBytes+size > checkpointBudget {
		return rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	h.checkpointBytes += size
	return nil
}

func (h *TerminalHost) refundCheckpoint(size int) {
	h.mu.Lock()
	h.checkpointBytes -= size
	h.mu.Unlock()
}
