package runner

import (
	"bytes"
	"context"
	"errors"
	"io"
	"sync"
	"testing"
	"time"
)

type waitFake struct {
	io.Reader
	io.Writer
	waitDelay time.Duration
	waitCode  int
	waitErr   error
	closed    chan struct{}
}

func newWaitFake(code int, err error, delay time.Duration) *waitFake {
	return &waitFake{
		Reader:    bytes.NewReader(nil),
		Writer:    io.Discard,
		waitDelay: delay,
		waitCode:  code,
		waitErr:   err,
		closed:    make(chan struct{}),
	}
}

func (f *waitFake) Close() error {
	select {
	case <-f.closed:
	default:
		close(f.closed)
	}
	return nil
}

func (f *waitFake) Wait() (int, error) {
	select {
	case <-time.After(f.waitDelay):
		return f.waitCode, f.waitErr
	case <-f.closed:
		return f.waitCode, f.waitErr
	}
}

type hungCloseStream struct {
	io.Reader
	io.Writer
	mu       sync.Mutex
	closes   int
	released chan struct{}
	returned chan struct{}
	code     int
}

func newHungCloseStream(code int) *hungCloseStream {
	return &hungCloseStream{
		Reader:   bytes.NewReader(nil),
		Writer:   io.Discard,
		released: make(chan struct{}),
		returned: make(chan struct{}),
		code:     code,
	}
}

func (s *hungCloseStream) Close() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.closes++
	if s.closes >= 2 {
		select {
		case <-s.released:
		default:
			close(s.released)
		}
	}
	return nil
}

func (s *hungCloseStream) Wait() (int, error) {
	<-s.released
	select {
	case <-s.returned:
	default:
		close(s.returned)
	}
	return s.code, nil
}

func TestWaitStream_returnsCompletedResultOnCancel(t *testing.T) {
	st := newWaitFake(3, nil, 20*time.Millisecond)
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	code, err := waitStream(ctx, st)
	if err != nil {
		t.Fatalf("waitStream error = %v, want completed result", err)
	}
	if code != 3 {
		t.Fatalf("waitStream code = %d, want 3", code)
	}
}

func TestWaitStream_cancelWithFailedWaitReportsCancel(t *testing.T) {
	st := newWaitFake(0, errors.New("boom"), 0)
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	if _, err := waitStream(ctx, st); !errors.Is(err, context.Canceled) {
		t.Fatalf("waitStream error = %v, want context.Canceled", err)
	}
}

func TestWaitStream_cancelWithoutCompletionTimesOut(t *testing.T) {
	old := waitCancelGrace
	waitCancelGrace = 50 * time.Millisecond
	defer func() { waitCancelGrace = old }()
	st := newHungCloseStream(0)
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	done := make(chan error, 1)
	go func() {
		_, err := waitStream(ctx, st)
		done <- err
	}()
	select {
	case err := <-done:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("waitStream error = %v, want context.Canceled", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("waitStream blocked past the cancel grace period")
	}
	select {
	case <-st.returned:
	case <-time.After(5 * time.Second):
		t.Fatal("wait worker leaked after the cancel grace period")
	}
	st.mu.Lock()
	closes := st.closes
	st.mu.Unlock()
	if closes < 2 {
		t.Fatalf("stream closed %d times, want at least 2 to release the worker", closes)
	}
}
