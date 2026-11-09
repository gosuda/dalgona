package tui

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

type Status struct {
	State    string
	Seq      uint64
	ThreadID string
}

type Options struct {
	OnStatus       func(Status)
	OnChildSpawned func(childID, cwd string)
	Input          io.Reader
	Output         io.Writer
	ThreadID       string
	Model          string
	Approval       string
	Sandbox        string
	Width          int
	Height         int
}

type programHandler struct {
	program *tea.Program
	ready   <-chan struct{}
	done    <-chan struct{}
}

func Run(ctx context.Context, conn io.ReadWriteCloser, opts Options) error {
	ready := make(chan struct{})
	done := make(chan struct{})
	defer close(done)
	handler := &programHandler{ready: ready, done: done}
	client, err := rpc.Dial(watchedConn{ReadWriteCloser: conn, lost: handler.connectionLost}, handler)
	if err != nil {
		return fmt.Errorf("dial tui rpc: %w", err)
	}
	program := tea.NewProgram(newModel(client, opts, done), programOptions(ctx, opts)...)
	handler.program = program
	close(ready)
	_, runErr := program.Run()
	closeErr := client.Close()
	if runErr != nil && !errors.Is(runErr, tea.ErrProgramKilled) {
		return fmt.Errorf("run tui: %w", runErr)
	}
	if runErr != nil && ctx.Err() == nil {
		return fmt.Errorf("run tui: %w", runErr)
	}
	if closeErr != nil {
		return fmt.Errorf("close tui rpc: %w", closeErr)
	}
	return nil
}

func programOptions(ctx context.Context, opts Options) []tea.ProgramOption {
	options := []tea.ProgramOption{tea.WithContext(ctx)}
	if opts.Input != nil {
		options = append(options, tea.WithInput(opts.Input))
	}
	if opts.Output != nil {
		options = append(options, tea.WithOutput(opts.Output))
	}
	if opts.Width > 0 && opts.Height > 0 {
		options = append(options, tea.WithWindowSize(opts.Width, opts.Height))
	}
	return options
}

func (h *programHandler) Notification(method string, params jsontext.Value) {
	h.send(notificationMsg{method: method, params: params.Clone()})
}

func (h *programHandler) connectionLost(err error) {
	h.send(disconnectedMsg{err: err})
}

func (h *programHandler) send(msg tea.Msg) {
	select {
	case <-h.ready:
	case <-h.done:
		return
	}
	h.program.Send(msg)
}

func (h *programHandler) Request(ctx context.Context, method string, params jsontext.Value) (any, error) {
	select {
	case <-h.ready:
	case <-h.done:
		return nil, errors.New("tui closed before the approval request")
	case <-ctx.Done():
		return nil, fmt.Errorf("approval request: %w", ctx.Err())
	}
	answer := make(chan requestAnswer, 1)
	h.program.Send(serverRequestMsg{method: method, params: params.Clone(), answer: answer})
	select {
	case response := <-answer:
		return response.value, response.err
	case <-h.done:
		return nil, errors.New("tui closed before the approval decision")
	case <-ctx.Done():
		return nil, fmt.Errorf("approval decision: %w", ctx.Err())
	}
}

type watchedConn struct {
	io.ReadWriteCloser
	lost func(error)
}

func (c watchedConn) Read(p []byte) (int, error) {
	n, err := c.ReadWriteCloser.Read(p)
	if err != nil {
		c.lost(err)
	}
	return n, err
}
