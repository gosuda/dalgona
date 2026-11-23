package client

import (
	"context"
	"errors"
	"fmt"
	"net"
	"sync"
)

var ErrSessionClosed = errors.New("client: session closed")

type Endpoint struct {
	Home   string
	Remote string
}

type Session struct {
	endpoint  Endpoint
	backend   Backend
	remotes   *Remotes
	gate      chan struct{}
	mu        sync.Mutex
	connected bool
	closed    bool
	conns     map[net.Conn]struct{}
}

func Local(home string, backend Backend) *Session {
	return &Session{endpoint: Endpoint{Home: home}, backend: backend, gate: make(chan struct{}, 1), conns: map[net.Conn]struct{}{}}
}

func Remote(remotes *Remotes, name string) *Session {
	return &Session{endpoint: Endpoint{Remote: name}, remotes: remotes, gate: make(chan struct{}, 1), conns: map[net.Conn]struct{}{}}
}

func (s *Session) Endpoint() Endpoint { return s.endpoint }

func (s *Session) Connect(ctx context.Context) (net.Conn, error) {
	if s.isClosed() {
		return nil, ErrSessionClosed
	}
	if s.remotes != nil || s.isConnected() {
		return s.Reconnect(ctx)
	}
	if err := s.enter(ctx); err != nil {
		return nil, err
	}
	defer s.leave()
	if s.isConnected() {
		return s.Reconnect(ctx)
	}
	conn, err := Ensure(ctx, s.endpoint.Home, s.backend.Path)
	if err != nil {
		return nil, err
	}
	if _, err := CheckBuild(ctx, s.endpoint.Home, s.backend.Build); err != nil {
		_ = conn.Close()
		return nil, err
	}
	return s.track(conn)
}

func (s *Session) Reconnect(ctx context.Context) (net.Conn, error) {
	if s.isClosed() {
		return nil, ErrSessionClosed
	}
	var conn net.Conn
	var err error
	if s.remotes != nil {
		conn, err = s.remotes.Bridge(ctx, s.endpoint.Remote)
	} else {
		conn, err = DialHome(s.endpoint.Home)
	}
	if err != nil {
		return nil, err
	}
	return s.track(conn)
}

func (s *Session) enter(ctx context.Context) error {
	select {
	case s.gate <- struct{}{}:
		return nil
	case <-ctx.Done():
		return context.Cause(ctx)
	}
}

func (s *Session) leave() { <-s.gate }

func (s *Session) isConnected() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.connected
}

func (s *Session) isClosed() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.closed
}

func (s *Session) Restart(ctx context.Context, running Info) (bool, error) {
	if s.remotes != nil {
		return false, fmt.Errorf("client: remote %s has no restart action", s.endpoint.Remote)
	}
	if err := s.enter(ctx); err != nil {
		return false, err
	}
	defer s.leave()
	home := s.endpoint.Home
	retired, err := Retire(ctx, home, running)
	if err != nil {
		return false, err
	}
	conn, err := Ensure(ctx, home, s.backend.Path)
	if err != nil {
		return false, err
	}
	return retired, conn.Close()
}

func (s *Session) Close() error {
	s.mu.Lock()
	s.closed = true
	conns := make([]net.Conn, 0, len(s.conns))
	for conn := range s.conns {
		conns = append(conns, conn)
	}
	clear(s.conns)
	s.mu.Unlock()
	var errs []error
	for _, conn := range conns {
		if err := conn.Close(); err != nil && !errors.Is(err, net.ErrClosed) {
			errs = append(errs, err)
		}
	}
	return errors.Join(errs...)
}

func (s *Session) track(conn net.Conn) (net.Conn, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		_ = conn.Close()
		return nil, ErrSessionClosed
	}
	s.connected = true
	s.conns[conn] = struct{}{}
	owned := ownedConn{Conn: conn, release: func() { s.release(conn) }}
	if half, ok := conn.(interface{ CloseWrite() error }); ok {
		return halfOwnedConn{ownedConn: owned, half: half}, nil
	}
	return owned, nil
}

func (s *Session) release(conn net.Conn) {
	s.mu.Lock()
	delete(s.conns, conn)
	s.mu.Unlock()
}

type ownedConn struct {
	net.Conn
	release func()
}

func (c ownedConn) Close() error {
	c.release()
	return c.Conn.Close()
}

type halfOwnedConn struct {
	ownedConn
	half interface{ CloseWrite() error }
}

func (c halfOwnedConn) CloseWrite() error { return c.half.CloseWrite() }
