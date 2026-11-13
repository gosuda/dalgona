package cursor

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"net/http"
	"slices"
	"strconv"
	"strings"

	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/connectwire"
	"github.com/gosuda/dalgona/internal/model/cursor/wire"
)

const (
	inBandUnavailable = "in-band tool execution is unavailable"
	queryRefused      = "Cursor interaction queries are not supported by this client."
)

var (
	errExecAborted  = errors.New("cursor: exec aborted by server")
	errFrameBacklog = errors.New("cursor: 64 server frames waited during an in-band call")
)

type session struct {
	stream     *model.EventStream
	conv       *conversation
	inBand     func(context.Context, model.ToolCall) (model.ToolResult, error)
	tools      []wire.McpToolDefinition
	writer     *frameWriter
	body       *idleBody
	frames     *frameReader
	sawTool    bool
	sawContent bool
	aborted    []uint32
	failed     []byte
}

func (s *session) consume(ctx context.Context, gate *model.StartGate, body *idleBody) model.AttemptResult {
	s.body = body
	s.frames = newFrameReader(body)
	err := s.readFrames(ctx, gate)
	s.frames.stop()
	body.pause()
	err = gate.Error(ctx, err)
	if err != nil {
		return model.AttemptResult{Status: failureStatus(err), SawTool: s.sawTool, SawContent: s.sawContent, Body: s.failed, Err: err}
	}
	return model.AttemptResult{Status: http.StatusOK, SawTool: s.sawTool, SawContent: s.sawContent}
}

func (s *session) readFrames(ctx context.Context, gate *model.StartGate) error {
	for {
		frame, err := s.frames.next()
		if errors.Is(err, io.EOF) {
			return errStreamCut
		}
		if err != nil {
			return fmt.Errorf("cursor: read stream: %w", err)
		}
		gate.FirstEvent()
		if frame.Flags&connectwire.Compressed != 0 {
			return errors.New("cursor: compressed frames are not negotiated")
		}
		if frame.Flags&connectwire.EndStream != 0 {
			return s.endStream(frame.Payload)
		}
		ended, err := s.handle(ctx, frame.Payload)
		if err != nil {
			s.failed = bytes.Clone(frame.Payload)
			return err
		}
		if ended {
			return nil
		}
	}
}

func (s *session) endStream(payload []byte) error {
	if err := connectwire.TrailerError(payload); err != nil {
		s.failed = bytes.Clone(payload)
		return fmt.Errorf("cursor: stream: %w", err)
	}
	return errStreamCut
}

func (s *session) handle(ctx context.Context, payload []byte) (bool, error) {
	var message wire.AgentServerMessage
	if err := wire.ConsumeAgentServerMessage(payload, &message); err != nil {
		return false, fmt.Errorf("cursor: decode server message: %w", err)
	}
	switch {
	case message.Interaction != nil:
		return s.interaction(message.Interaction), nil
	case message.Exec != nil:
		return false, s.exec(ctx, message.Exec)
	case message.MessageNumber == wire.ServerKvNumber:
		return false, s.kv(message.MessageView)
	case message.MessageNumber == wire.ServerCheckpointNumber:
		return false, s.conv.saveCheckpoint(message.MessageView)
	case message.MessageNumber == wire.ServerInteractionQueryNumber:
		return false, s.refuseQuery(message.MessageView)
	}
	return false, nil
}

func (s *session) interaction(update *wire.InteractionUpdate) bool {
	switch {
	case update.Text != nil && len(update.Text.TextView) != 0:
		s.sawContent = true
		s.stream.Emit(model.TextDelta{Text: string(update.Text.TextView)})
	case update.Thinking != nil && len(update.Thinking.TextView) != 0:
		s.sawContent = true
		s.stream.Emit(model.ReasoningDelta{Text: string(update.Thinking.TextView)})
	case update.Ended != nil:
		s.stream.Emit(turnUsage(update.Ended))
		s.stream.Emit(model.Done{StopReason: "stop"})
		return true
	}
	return false
}

func turnUsage(ended *wire.TurnEndedUpdate) model.Usage {
	cacheRead, cacheWrite := tokenCount(ended.CacheRead), tokenCount(ended.CacheWrite)
	return model.Usage{
		Input:      max(tokenCount(ended.Input)-cacheRead-cacheWrite, 0),
		Output:     tokenCount(ended.Output),
		CacheRead:  cacheRead,
		CacheWrite: cacheWrite,
	}
}

func tokenCount(value int64) int {
	return int(min(max(value, 0), 1<<31-1))
}

func (s *session) refuseQuery(view []byte) error {
	var query wire.InteractionQuery
	if err := wire.ConsumeInteractionQuery(view, &query); err != nil {
		return fmt.Errorf("cursor: decode interaction query: %w", err)
	}
	refusal, err := queryRefusal(query.QueryNumber)
	if err != nil {
		return err
	}
	response := wire.AppendInteractionResponse(nil, &wire.InteractionResponse{ID: query.ID, ResultNumber: query.QueryNumber, ResultView: refusal})
	return sendError(s.writer.send(&wire.AgentClientMessage{MessageNumber: wire.ClientInteractionResponseNumber, MessageView: response}))
}

func queryRefusal(query int32) ([]byte, error) {
	switch query {
	case wire.QueryWebSearch, wire.QuerySwitchMode, wire.QueryExaSearch, wire.QueryExaFetch, wire.QueryWebFetch:
		return wire.AppendQueryRejected(nil, []byte(queryRefused)), nil
	case wire.QueryAskQuestion:
		return wire.AppendAskQuestionRejected(nil, []byte(queryRefused)), nil
	case wire.QueryCreatePlan:
		return wire.AppendCreatePlanError(nil, []byte(queryRefused)), nil
	}
	return nil, fmt.Errorf("cursor: interaction query %d has no refusal and cannot be answered", query)
}

func (s *session) kv(view []byte) error {
	var request wire.KvServerMessage
	if err := wire.ConsumeKvServerMessage(view, &request); err != nil {
		return fmt.Errorf("cursor: decode kv request: %w", err)
	}
	reply := wire.KvClientMessage{ID: request.ID}
	var stored error
	switch {
	case request.Get != nil:
		reply.Get = &wire.GetBlobResult{BlobDataView: s.conv.blob(request.Get.BlobIDView)}
	case request.Set != nil:
		stored = s.conv.setBlob(request.Set.BlobIDView, request.Set.BlobDataView)
		reply.Set = &wire.SetBlobResult{}
	default:
		return nil
	}
	sent := s.writer.send(&wire.AgentClientMessage{MessageNumber: wire.ClientKvNumber, MessageView: wire.AppendKvClientMessage(nil, &reply)})
	return errors.Join(stored, sendError(sent))
}

func sendError(err error) error {
	if err == nil || errors.Is(err, io.ErrClosedPipe) {
		return nil
	}
	return fmt.Errorf("cursor: send: %w", err)
}

func (s *session) exec(ctx context.Context, request *wire.ExecServerMessage) error {
	var err error
	if !s.takeAborted(request.ID) {
		err = s.dispatch(ctx, request)
	}
	closed := s.writer.execControl(wire.ExecStreamClose, request.ID, "")
	return errors.Join(err, sendError(closed))
}

func (s *session) takeAborted(id uint32) bool {
	index := slices.Index(s.aborted, id)
	if index < 0 {
		return false
	}
	s.aborted = slices.Delete(s.aborted, index, index+1)
	return true
}

func (s *session) dispatch(ctx context.Context, request *wire.ExecServerMessage) error {
	switch {
	case request.RequestContext != nil:
		result := &wire.RequestContextResult{Success: &wire.RequestContextSuccess{RequestContext: &wire.RequestContext{Tools: s.tools}}}
		return s.reply(request, &wire.ExecClientMessage{RequestContextResult: result})
	case request.Shell != nil:
		return s.shell(ctx, request, request.Shell, s.replyShell)
	case request.ShellStream != nil:
		return s.shell(ctx, request, request.ShellStream, s.replyShellStream)
	case request.Mcp != nil:
		return s.mcp(ctx, request)
	}
	return sendError(s.writer.execControl(wire.ExecThrow, request.ID, "unsupported exec request "+strconv.Itoa(int(request.MessageNumber))))
}

func (s *session) reply(request *wire.ExecServerMessage, message *wire.ExecClientMessage) error {
	message.ID, message.ExecIDView = request.ID, request.ExecIDView
	return sendError(s.writer.send(&wire.AgentClientMessage{Exec: message}))
}

type shellReply func(request *wire.ExecServerMessage, args *wire.ShellArgs, result model.ToolResult) error

func (s *session) shell(ctx context.Context, request *wire.ExecServerMessage, args *wire.ShellArgs, reply shellReply) error {
	call, err := shellCall(request, args)
	if err != nil {
		return err
	}
	result, ran, err := s.runInBand(ctx, request.ID, call)
	if err == nil {
		err = reply(request, args, result)
	}
	return s.settle(call, ran, err)
}

func (s *session) settle(call model.ToolCall, ran bool, err error) error {
	if err != nil && !errors.Is(err, errExecAborted) {
		return err
	}
	s.resolved(call, ran)
	return nil
}

func shellCall(request *wire.ExecServerMessage, args *wire.ShellArgs) (model.ToolCall, error) {
	var buffer bytes.Buffer
	encoder := model.NewJSONEncoder(&buffer)
	encoder.BeginObject()
	encoder.StringKey("command", string(args.CommandView))
	if len(args.WorkingDirectoryView) != 0 {
		encoder.StringKey("cwd", string(args.WorkingDirectoryView))
	}
	if args.HardTimeout > 0 {
		encoder.IntKey("timeout_ms", int(args.HardTimeout))
	}
	encoder.EndObject()
	if err := encoder.Err(); err != nil {
		return model.ToolCall{}, fmt.Errorf("cursor: encode shell call: %w", err)
	}
	id := string(args.ToolCallIDView)
	if id == "" {
		id = "cursor-exec-" + strconv.FormatUint(uint64(request.ID), 10)
	}
	return model.ToolCall{ID: id, Name: nativeShellTool, Args: bytes.TrimRight(buffer.Bytes(), "\n")}, nil
}

func (s *session) replyShell(request *wire.ExecServerMessage, args *wire.ShellArgs, result model.ToolResult) error {
	outcome := &wire.ShellOutcome{
		CommandView:          args.CommandView,
		WorkingDirectoryView: args.WorkingDirectoryView,
		StdoutView:           []byte(model.ToolResultText(result.Output)),
	}
	shell := wire.ShellResult{Success: outcome}
	if result.IsError {
		outcome.ExitCode = 1
		shell = wire.ShellResult{Failure: outcome}
	}
	return s.reply(request, &wire.ExecClientMessage{ResultNumber: wire.ExecShellResultNumber, ResultView: wire.AppendShellResult(nil, &shell)})
}

func (s *session) replyShellStream(request *wire.ExecServerMessage, args *wire.ShellArgs, result model.ToolResult) error {
	exit := &wire.ShellExit{CwdView: args.WorkingDirectoryView}
	if result.IsError {
		exit.Code = 1
	}
	events := []wire.ShellStreamEvent{
		{Kind: wire.ShellStreamStart},
		{Kind: wire.ShellStreamStdout, DataView: []byte(model.ToolResultText(result.Output))},
		{Kind: wire.ShellStreamExit, Exit: exit},
	}
	for index := range events {
		view := wire.AppendShellStreamEvent(nil, &events[index])
		if err := s.reply(request, &wire.ExecClientMessage{ResultNumber: wire.ExecShellStreamNumber, ResultView: view}); err != nil {
			return err
		}
	}
	return nil
}

func (s *session) mcp(ctx context.Context, request *wire.ExecServerMessage) error {
	args := request.Mcp
	name := string(args.ToolNameView)
	if name == "" {
		name = string(args.NameView)
	}
	if args.ApprovalOnly {
		reason := wire.AppendTextMessage(nil, []byte(`Tool "`+name+`" is not approved to run without asking.`))
		return s.reply(request, &wire.ExecClientMessage{McpResult: &wire.McpResult{ResultNumber: wire.McpResultRejectedNumber, ResultView: reason}})
	}
	callArgs, err := mcpArguments(args.Args)
	if err != nil {
		return err
	}
	call := model.ToolCall{ID: string(args.ToolCallIDView), Name: name, Args: callArgs}
	result, ran, err := s.runInBand(ctx, request.ID, call)
	if err == nil {
		err = s.reply(request, &wire.ExecClientMessage{McpResult: mcpResult(result, ran)})
	}
	return s.settle(call, ran, err)
}

func mcpResult(result model.ToolResult, ran bool) *wire.McpResult {
	text := wire.AppendTextMessage(nil, []byte(model.ToolResultText(result.Output)))
	if !ran {
		return &wire.McpResult{ResultNumber: wire.McpResultErrorNumber, ResultView: text}
	}
	content := []wire.McpContentItem{{ContentNumber: wire.McpTextContentNumber, ContentView: text}}
	return &wire.McpResult{Success: &wire.McpSuccess{Content: content, IsError: result.IsError}}
}

func mcpArguments(arguments []wire.McpArgument) (jsontext.Value, error) {
	var buffer bytes.Buffer
	encoder := model.NewJSONEncoder(&buffer)
	encoder.BeginObject()
	for _, argument := range arguments {
		value, err := wire.ValueJSON(argument.ValueView)
		if err != nil {
			return nil, fmt.Errorf("cursor: MCP argument %s: %w", argument.NameView, err)
		}
		encoder.ValueKey(string(argument.NameView), value)
	}
	encoder.EndObject()
	if err := encoder.Err(); err != nil {
		return nil, fmt.Errorf("cursor: encode MCP arguments: %w", err)
	}
	return bytes.TrimRight(buffer.Bytes(), "\n"), nil
}

func (s *session) runInBand(ctx context.Context, execID uint32, call model.ToolCall) (model.ToolResult, bool, error) {
	if s.inBand == nil {
		output, err := quoted(inBandUnavailable)
		return model.ToolResult{ID: call.ID, Name: call.Name, Output: output, IsError: true}, false, err
	}
	s.sawTool = true
	s.body.pause()
	defer s.body.resume()
	stopBeats := repeat(ctx, execBeatPeriod, func() error { return s.writer.execControl(wire.ExecHeartbeat, execID, "") })
	result, err := s.awaitInBand(ctx, execID, call)
	stopBeats()
	if err == nil {
		return result, true, nil
	}
	if errors.Is(err, errExecAborted) || errors.Is(err, errFrameBacklog) {
		return model.ToolResult{}, true, err
	}
	if ctxErr := ctx.Err(); ctxErr != nil {
		return model.ToolResult{}, true, fmt.Errorf("cursor: in-band %s: %w", call.Name, ctxErr)
	}
	output, quoteErr := quoted(err.Error())
	return model.ToolResult{ID: call.ID, Name: call.Name, Output: output, IsError: true}, true, quoteErr
}

type inBandOutcome struct {
	result model.ToolResult
	err    error
}

func (s *session) awaitInBand(ctx context.Context, execID uint32, call model.ToolCall) (model.ToolResult, error) {
	callCtx, cancel := context.WithCancelCause(ctx)
	defer cancel(nil)
	done := make(chan inBandOutcome, 1)
	go func() {
		result, err := s.inBand(callCtx, call)
		done <- inBandOutcome{result: result, err: err}
	}()
	ahead := s.frames.lookahead()
	for {
		select {
		case outcome := <-done:
			switch cause := context.Cause(callCtx); {
			case errors.Is(cause, errExecAborted):
				return outcome.result, errExecAborted
			case errors.Is(cause, errFrameBacklog):
				return outcome.result, errFrameBacklog
			}
			return outcome.result, outcome.err
		case read := <-ahead:
			ahead = s.duringCall(read, execID, cancel)
		}
	}
}

type callFrame int

const (
	callFrameQueue callFrame = iota
	callFrameHeartbeat
	callFrameAbort
)

func (s *session) duringCall(read frameRead, execID uint32, cancel context.CancelCauseFunc) <-chan frameRead {
	kind, abortID := classifyCallFrame(read)
	switch kind {
	case callFrameQueue:
		full := s.frames.queue(read)
		s.frames.landed(read)
		if read.err != nil {
			return nil
		}
		if full {
			cancel(errFrameBacklog)
			return nil
		}
	case callFrameHeartbeat:
		s.frames.landed(read)
	case callFrameAbort:
		s.frames.landed(read)
		s.abort(abortID, execID, cancel)
	}
	return s.frames.lookahead()
}

func (s *session) abort(abortID, execID uint32, cancel context.CancelCauseFunc) {
	if abortID == execID {
		cancel(errExecAborted)
		return
	}
	if len(s.aborted) >= maxQueuedFrames {
		copy(s.aborted, s.aborted[1:])
		s.aborted = s.aborted[:len(s.aborted)-1]
	}
	s.aborted = append(s.aborted, abortID)
}

func classifyCallFrame(read frameRead) (callFrame, uint32) {
	if read.err != nil || read.frame.Flags != 0 {
		return callFrameQueue, 0
	}
	var message wire.AgentServerMessage
	if wire.ConsumeAgentServerMessage(read.frame.Payload, &message) != nil {
		return callFrameQueue, 0
	}
	if message.Interaction != nil && message.Interaction.VariantNumber == wire.InteractionHeartbeatNumber {
		return callFrameHeartbeat, 0
	}
	if message.MessageNumber != wire.ServerExecControlNumber {
		return callFrameQueue, 0
	}
	var control wire.ExecServerControl
	if wire.ConsumeExecServerControl(message.MessageView, &control) != nil || !control.Abort {
		return callFrameQueue, 0
	}
	return callFrameAbort, control.AbortID
}

func (s *session) resolved(call model.ToolCall, ran bool) {
	if !ran {
		return
	}
	call.Resolved = true
	s.stream.Emit(call)
}

func quoted(text string) (jsontext.Value, error) {
	value, err := jsontext.AppendQuote(nil, strings.ToValidUTF8(text, "\uFFFD"))
	if err != nil {
		return nil, fmt.Errorf("cursor: quote tool output: %w", err)
	}
	return value, nil
}
