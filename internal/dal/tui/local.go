package tui

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"io"
	"sync"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const localMachineID = "local"

type localMachine struct {
	mu        sync.Mutex
	work      sync.WaitGroup
	ctx       context.Context
	cancel    context.CancelFunc
	dial      func(context.Context) (io.ReadWriteCloser, error)
	events    MachineEvents
	conn      *rpc.Conn
	phase     string
	announced chan struct{}
}

func Local(dial func(context.Context) (io.ReadWriteCloser, error)) OpenMachines {
	return func(events MachineEvents) (Machines, error) {
		ctx, cancel := context.WithCancel(context.Background())
		return &localMachine{ctx: ctx, cancel: cancel, dial: dial, events: events, phase: "idle", announced: make(chan struct{})}, nil
	}
}

func (s *localMachine) Endpoints() []Machine {
	s.mu.Lock()
	defer s.mu.Unlock()
	return []Machine{{ID: localMachineID, Label: localMachineID, Kind: localMachineID, Phase: s.phase}}
}

func (s *localMachine) Connect(id string) error {
	if id != localMachineID {
		return errors.New("dal only connects to its in-process backend")
	}
	s.mu.Lock()
	if s.ctx.Err() != nil || s.phase != "idle" {
		s.mu.Unlock()
		return errors.New("the local connection has already started or closed")
	}
	s.work.Add(1)
	s.phase = "connecting"
	s.mu.Unlock()
	defer s.work.Done()
	initialized, err := s.connect()
	if err != nil {
		s.change("offline", initialized, err)
		return nil
	}
	s.change("connected", initialized, nil)
	close(s.announced)
	s.work.Go(s.watch)
	return nil
}

func (s *localMachine) connect() (rpc.InitializeResult, error) {
	ctx, cancel := context.WithTimeout(s.ctx, requestTimeout)
	defer cancel()
	var initialized rpc.InitializeResult
	raw, err := s.dial(ctx)
	if err != nil {
		return initialized, err
	}
	s.mu.Lock()
	if s.ctx.Err() != nil {
		s.mu.Unlock()
		return initialized, errors.Join(s.ctx.Err(), raw.Close())
	}
	conn := rpc.Dial(s.ctx, raw, s)
	s.conn = conn
	s.mu.Unlock()
	params := rpc.InitializeParams{ClientInfo: rpc.ClientInfo{Name: "dalgona", Title: "dalgona TUI", Version: clientVersion()}}
	if err := conn.Initialize(ctx, params, &initialized); err != nil {
		return initialized, errors.Join(err, conn.Close())
	}
	if err := conn.Notify("initialized", nil); err != nil {
		return initialized, errors.Join(err, conn.Close())
	}
	return initialized, nil
}

func (s *localMachine) change(phase string, initialized rpc.InitializeResult, err error) {
	s.mu.Lock()
	s.phase = phase
	s.mu.Unlock()
	if s.ctx.Err() == nil {
		s.events.Phase(MachinePhase{Link: localLink(), Phase: phase, Initialized: initialized, Err: err})
	}
}

func (s *localMachine) watch() {
	<-s.conn.Done()
	s.change("offline", rpc.InitializeResult{}, io.ErrClosedPipe)
}

func (s *localMachine) Disconnect(id string) error {
	if id != localMachineID {
		return errors.New("dal only connects to its in-process backend")
	}
	return s.Close()
}

func (s *localMachine) Close() error {
	s.cancel()
	s.mu.Lock()
	conn := s.conn
	s.mu.Unlock()
	var err error
	if conn != nil {
		err = conn.Close()
	}
	s.work.Wait()
	return err
}

func (s *localMachine) connection(link Link) (*rpc.Conn, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if link != localLink() || s.phase != "connected" || s.ctx.Err() != nil {
		return nil, errors.New("the local connection has ended")
	}
	return s.conn, nil
}

func (s *localMachine) Call(ctx context.Context, link Link, method string, params, result any) error {
	conn, err := s.connection(link)
	if err != nil {
		return err
	}
	return conn.Call(ctx, method, params, result)
}

func (s *localMachine) Respond(ctx context.Context, link Link, requestID string, result any) error {
	conn, err := s.connection(link)
	if err != nil {
		return err
	}
	return conn.Respond(ctx, requestID, result)
}

func (s *localMachine) Notification(method string, params jsontext.Value) {
	select {
	case <-s.announced:
		s.events.Notify(localLink(), method, params)
	case <-s.ctx.Done():
	}
}

func (s *localMachine) Request(ctx context.Context, method string, params jsontext.Value) (any, error) {
	select {
	case <-s.announced:
		return s.events.Request(ctx, localLink(), method, params)
	case <-ctx.Done():
		return nil, ctx.Err()
	case <-s.ctx.Done():
		return nil, s.ctx.Err()
	}
}

func localLink() Link { return Link{Endpoint: localMachineID, Generation: 1} }
