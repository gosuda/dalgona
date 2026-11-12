package model

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"io"
	"net/http"
	"sync"
	"time"
)

type Provider interface {
	Stream(context.Context, Request) (Stream, error)
	Models(context.Context) ([]Model, error)
}

type Stream interface {
	Next(context.Context) (Event, error)
	Events() <-chan Event
	Err() error
	Close() error
}

type Event interface{ event() }

type TextDelta struct{ Text string }

func (TextDelta) event() {}

type ReasoningDelta struct{ Text string }

func (ReasoningDelta) event() {}

type ToolCall struct {
	ID               string
	Name             string
	Args             jsontext.Value
	Freeform         bool
	Resolved         bool
	Signature        string
	RedactedThinking []string
}

func (ToolCall) event() {}

type Usage struct{ Input, Output, CacheRead, CacheWrite int }

func (Usage) event() {}

type Done struct{ StopReason string }

func (Done) event() {}

type Error struct {
	Err     error
	Message string
	Status  int
	Body    []byte
}

func (Error) event() {}

type Request struct {
	Model     string
	System    string
	Messages  []Message
	Tools     []Tool
	Reasoning string
}
type Message struct {
	Role        string
	Text        string
	Reasoning   string
	ToolCalls   []ToolCall
	ToolResults []ToolResult
}
type ToolResult struct {
	ID      string
	Name    string
	Output  jsontext.Value
	IsError bool
}
type Tool struct {
	Name        string
	Description string
	Parameters  jsontext.Value
	Schema      jsontext.Value
	Freeform    bool
}

func (t Tool) JSONSchema() jsontext.Value {
	if len(t.Parameters) != 0 {
		return t.Parameters
	}
	return t.Schema
}

type Model struct {
	ID         string
	Context    int
	MaxOutput  int
	Efforts    []string
	ToolChoice bool
}

func ContextWindow(modelID string, reported int, overrides map[string]int) int {
	if configured := overrides[modelID]; configured > 0 {
		return configured
	}
	if reported > 0 {
		return reported
	}
	return 128000
}

func ParseModelID(value string) (string, string, error) {
	for i := 0; i < len(value); i++ {
		if value[i] == '/' {
			if i == 0 || i == len(value)-1 {
				break
			}
			return value[:i], value[i+1:], nil
		}
	}
	return "", "", errors.New("model id must be <provider>/<model>")
}

func NewHTTPClient(client *http.Client) *http.Client {
	if client != nil {
		return client
	}
	return &http.Client{Timeout: 0}
}

type EventStream struct {
	events     chan Event
	err        error
	done       chan struct{}
	stop       chan struct{}
	finish     sync.Once
	stopOnce   sync.Once
	stopParent func() bool
	timers     *StreamTimers
	sending    sync.RWMutex
	closed     bool
}

func NewEventStream(ctx context.Context, capacity int) *EventStream {
	if capacity < 1 {
		capacity = 16
	}
	stream := &EventStream{
		events: make(chan Event, capacity),
		done:   make(chan struct{}),
		stop:   make(chan struct{}),
		timers: NewStreamTimers(),
	}
	stream.stopParent = context.AfterFunc(ctx, stream.initiateStop)
	return stream
}

func (s *EventStream) Context(ctx context.Context) context.Context {
	derived, cancel := context.WithCancel(ctx)
	go func() {
		select {
		case <-s.stop:
			cancel()
		case <-s.done:
			cancel()
		case <-derived.Done():
		}
	}()
	return derived
}

func (s *EventStream) Emit(event Event) bool {
	s.sending.RLock()
	defer s.sending.RUnlock()
	if s.closed {
		return false
	}
	select {
	case s.events <- event:
		return true
	case <-s.stop:
		return false
	}
}

func (s *EventStream) Finish(err error) {
	s.finish.Do(func() {
		if s.stopParent != nil {
			s.stopParent()
		}
		s.initiateStop()
		s.sending.Lock()
		defer s.sending.Unlock()
		s.closed = true
		s.err = err
		close(s.done)
		close(s.events)
	})
}

func (s *EventStream) Next(ctx context.Context) (Event, error) {
	select {
	case event, ok := <-s.events:
		if !ok {
			if s.err != nil {
				return nil, s.err
			}
			return nil, io.EOF
		}
		return event, nil
	case <-ctx.Done():
		return nil, ctx.Err()
	}
}

func (s *EventStream) Events() <-chan Event  { return s.events }
func (s *EventStream) Timers() *StreamTimers { return s.timers }
func (s *EventStream) Err() error            { return s.err }
func (s *EventStream) initiateStop()         { s.stopOnce.Do(func() { close(s.stop) }) }
func (s *EventStream) Close() error {
	s.initiateStop()
	select {
	case <-s.done:
		return nil
	case <-time.After(time.Second):
		return context.DeadlineExceeded
	}
}
