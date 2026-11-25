package tui

import (
	"cmp"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"os"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const framesPerSecond = 60

type Status struct {
	Endpoint string
	Remote   string
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
	Latest         bool
	InitialMachine string
	Model          string
	Approval       string
	Sandbox        string
	Cwd            string
	ExplicitCwd    bool
	Width          int
	Height         int
}

type programHandler struct {
	program *tea.Program
	ready   <-chan struct{}
	done    <-chan struct{}
}

func Run(ctx context.Context, open OpenMachines, opts Options) (runErr error) {
	input, err := openTerminalInput(opts.Input)
	if err != nil {
		return err
	}
	defer input.close()
	ready := make(chan struct{})
	done := make(chan struct{})
	handler := &programHandler{ready: ready, done: done}
	set, err := open(MachineEvents{Phase: handler.phase, Notify: handler.notification, Request: handler.request})
	if err != nil {
		return err
	}
	defer func() {
		close(done)
		if err = set.Close(); err != nil {
			runErr = errors.Join(runErr, fmt.Errorf("close terminal machine connections: %w", err))
		}
	}()
	output := cmp.Or[io.Writer](opts.Output, os.Stdout)
	opts.Input = input.stream
	opts.Output = newDockOutput(output, input.paste.source, func(msg dockWrittenMsg) { handler.send(msg) })
	root, err := newMachineModel(set, opts, done)
	if err != nil {
		return err
	}
	program := tea.NewProgram(root, programOptions(ctx, opts, input.paste)...)
	handler.program = program
	close(ready)
	_, err = program.Run()
	if err != nil && (!errors.Is(err, tea.ErrProgramKilled) || ctx.Err() == nil) {
		return fmt.Errorf("run tui: %w", err)
	}
	return nil
}

func programOptions(ctx context.Context, opts Options, paste *terminalPasteReader) []tea.ProgramOption {
	options := []tea.ProgramOption{
		tea.WithContext(ctx), tea.WithFPS(framesPerSecond),
		tea.WithInput(opts.Input), tea.WithOutput(opts.Output), tea.WithFilter(paste.restore),
	}
	if opts.Width > 0 && opts.Height > 0 {
		options = append(options, tea.WithWindowSize(opts.Width, opts.Height))
	}
	return options
}

func (h *programHandler) notification(link Link, method string, params jsontext.Value) {
	h.send(machineMsg{link: link, body: notificationMsg{method: method, params: params.Clone()}})
}

func (h *programHandler) phase(change MachinePhase) {
	h.send(change)
}

func (h *programHandler) send(msg tea.Msg) {
	select {
	case <-h.ready:
	case <-h.done:
		return
	}
	h.program.Send(msg)
}

func (h *programHandler) request(ctx context.Context, link Link, method string, params jsontext.Value) (any, error) {
	select {
	case <-h.ready:
	case <-h.done:
		return nil, errors.New("tui closed before the approval request")
	case <-ctx.Done():
		return nil, fmt.Errorf("approval request: %w", ctx.Err())
	}
	answer := make(chan requestAnswer, 1)
	requestID, err := rpc.ClientRequestID(ctx)
	if err != nil {
		return nil, fmt.Errorf("approval request id: %w", err)
	}
	h.program.Send(machineMsg{link: link, body: serverRequestMsg{method: method, requestID: requestID, params: params.Clone(), answer: answer}})
	if method == "item/tool/requestUserInput" {
		return h.answerQuestion(ctx, answer)
	}
	select {
	case response := <-answer:
		return response.value, response.err
	case <-h.done:
		return nil, errors.New("tui closed before the approval decision")
	case <-ctx.Done():
		return nil, fmt.Errorf("approval decision: %w", ctx.Err())
	}
}

func (h *programHandler) answerQuestion(ctx context.Context, answer <-chan requestAnswer) (any, error) {
	respond := rpc.ClientResponder(ctx)
	for {
		select {
		case response := <-answer:
			if response.err != nil {
				return nil, response.err
			}
			if err := respond(response.value); err != nil {
				return nil, fmt.Errorf("answer question: %w", err)
			}
		case <-h.done:
			return nil, errors.New("tui closed before the question was answered")
		case <-ctx.Done():
			return nil, rpc.ErrResponded
		}
	}
}
