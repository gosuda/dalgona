package devin

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"math"
	"net/http"
	"strconv"

	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/devin/wire"
)

// source: git show f2cb47b8:engine/dal-provider/src/family/devin/proto.rs stop_reason
const (
	stopUnspecified  = 0
	stopPattern      = 2
	stopMaxTokens    = 3
	stopFunctionCall = 10
)

type openCall struct {
	id   string
	name string
	args []byte
}

type usageMax struct {
	input, output, cacheRead, cacheWrite int
}

type chatState struct {
	stream     *model.EventStream
	freeform   map[string]bool
	inflate    inflater
	calls      []openCall
	active     int
	stop       int32
	usage      usageMax
	sawTool    bool
	sawContent bool
	failed     []byte
}

func newChatState(stream *model.EventStream, tools []model.Tool) *chatState {
	return &chatState{stream: stream, freeform: model.FreeformToolNames(tools), active: -1}
}

func (s *chatState) consume(ctx context.Context, gate *model.StartGate, idle *idleBody) model.AttemptResult {
	err := s.readFrames(gate, idle)
	idle.stop()
	err = gate.Error(ctx, err)
	if err != nil {
		return model.AttemptResult{Status: connectStatus(err, http.StatusOK), SawTool: s.sawTool, SawContent: s.sawContent, Body: s.failed, Err: err}
	}
	return model.AttemptResult{Status: http.StatusOK, SawTool: s.sawTool, SawContent: s.sawContent}
}

func (s *chatState) readFrames(gate *model.StartGate, body io.Reader) error {
	buffer := make([]byte, 0, 32<<10)
	for {
		frame, err := wire.ReadFrame(body, buffer, maxFramePayload)
		if errors.Is(err, io.EOF) {
			return s.finishAtEOF()
		}
		if err != nil {
			return fmt.Errorf("devin: read stream: %w", err)
		}
		gate.FirstEvent()
		buffer = frame.Payload[:0]
		payload, err := s.inflate.payload(frame)
		if err != nil {
			return fmt.Errorf("devin: %w", err)
		}
		if frame.Flags&wire.EndStream != 0 {
			return s.finishTrailer(payload)
		}
		if err := s.apply(payload); err != nil {
			s.failed = bytes.Clone(payload)
			return err
		}
	}
}

func (s *chatState) apply(payload []byte) error {
	var message wire.GetChatMessageResponse
	if err := wire.ConsumeGetChatMessageResponse(payload, &message); err != nil {
		return fmt.Errorf("devin: decode chat message: %w", err)
	}
	if len(message.DeltaThinkingView) != 0 {
		s.sawContent = true
		s.stream.Emit(model.ReasoningDelta{Text: string(message.DeltaThinkingView)})
	}
	if len(message.DeltaTextView) != 0 {
		s.sawContent = true
		s.stream.Emit(model.TextDelta{Text: string(message.DeltaTextView)})
	}
	for index := range message.ToolCalls {
		s.toolDelta(&message.ToolCalls[index])
	}
	if message.StopReason != stopUnspecified {
		s.stop = message.StopReason
	}
	if message.Usage != nil {
		s.usage.merge(message.Usage)
	}
	return nil
}

func (s *chatState) toolDelta(delta *wire.ChatToolCall) {
	index := s.active
	if len(delta.IDView) != 0 {
		index = s.callIndex(delta.IDView)
	}
	if index < 0 {
		return
	}
	s.active = index
	call := &s.calls[index]
	if len(delta.NameView) != 0 {
		call.name = string(delta.NameView)
	}
	if bytes.HasPrefix(delta.ArgumentsView, call.args) {
		call.args = append(call.args[:0], delta.ArgumentsView...)
		return
	}
	call.args = append(call.args, delta.ArgumentsView...)
}

func (s *chatState) callIndex(id []byte) int {
	for index := range s.calls {
		if s.calls[index].id == string(id) {
			return index
		}
	}
	s.calls = append(s.calls, openCall{id: string(id)})
	return len(s.calls) - 1
}

func (u *usageMax) merge(stats *wire.ModelUsageStats) {
	u.input = max(u.input, tokenCount(stats.Input))
	u.output = max(u.output, tokenCount(stats.Output))
	u.cacheRead = max(u.cacheRead, tokenCount(stats.CacheRead))
	u.cacheWrite = max(u.cacheWrite, tokenCount(stats.CacheWrite))
}

func tokenCount(value uint64) int {
	if value > math.MaxInt32 {
		return math.MaxInt32
	}
	return int(value)
}

func (s *chatState) finishAtEOF() error {
	if s.stop == stopUnspecified {
		return errors.New("devin: stream ended without a stop reason")
	}
	return s.finish()
}

func (s *chatState) finishTrailer(payload []byte) error {
	if err := wire.TrailerError(payload); err != nil {
		s.failed = bytes.Clone(payload)
		return fmt.Errorf("devin: stream: %w", err)
	}
	return s.finish()
}

func (s *chatState) finish() error {
	reason, err := s.stopReason()
	if err != nil {
		return err
	}
	for _, call := range s.calls {
		if err := s.emitCall(call); err != nil {
			return err
		}
	}
	s.stream.Emit(model.Usage{Input: s.usage.input, Output: s.usage.output, CacheRead: s.usage.cacheRead, CacheWrite: s.usage.cacheWrite})
	s.stream.Emit(model.Done{StopReason: reason})
	return nil
}

func (s *chatState) stopReason() (string, error) {
	switch {
	case s.stop == stopMaxTokens:
		return "length", nil
	case len(s.calls) != 0:
		return "toolUse", nil
	case s.stop == stopUnspecified || s.stop == stopPattern || s.stop == stopFunctionCall:
		return "stop", nil
	default:
		return "", errors.New("devin: provider stopped with reason " + strconv.Itoa(int(s.stop)))
	}
}

func (s *chatState) emitCall(call openCall) error {
	args := jsontext.Value(call.args)
	if len(args) == 0 {
		args = jsontext.Value(`{}`)
	}
	if !args.IsValid() {
		return fmt.Errorf("devin: tool %s arguments are not valid JSON", call.name)
	}
	tool := model.ToolCall{ID: call.id, Name: call.name, Args: args, Freeform: s.freeform[call.name]}
	if tool.Freeform {
		input, err := model.DecodeFreeformInput(args)
		if err != nil {
			return fmt.Errorf("devin: tool %s freeform input: %w", call.name, err)
		}
		tool.Args = input
	}
	s.sawTool = true
	s.stream.Emit(tool)
	return nil
}
