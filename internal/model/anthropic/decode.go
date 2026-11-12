package anthropic

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"slices"

	"github.com/gosuda/dalgona/internal/model"
)

type responseState struct {
	stream     *model.EventStream
	freeform   map[string]bool
	tools      map[int]*model.ToolCall
	signature  string
	redacted   []string
	usage      usage
	sawTool    bool
	sawContent bool
	done       bool
	stopped    bool
	errorBody  []byte
}

type responseEvent struct {
	Type         string
	Index        int
	Message      messageStart
	ContentBlock block
	Delta        delta
	Usage        usage
	HasUsage     bool
	Error        *providerError
}

type messageStart struct {
	Usage usage
}

type block struct {
	Type  string
	ID    string
	Name  string
	Input jsontext.Value
	Data  string
}

type delta struct {
	Type       string
	Text       string
	Thinking   string
	Signature  string
	Partial    string
	StopReason string
}

type providerError struct {
	Type    string
	Code    string
	Message string
}

type usage struct {
	Input      int
	Output     int
	CacheRead  int
	CacheWrite int
	inputSet   bool
	outputSet  bool
	readSet    bool
	writeSet   bool
}

func (s *responseState) consume(ctx context.Context, gate *model.StartGate, body io.ReadCloser, options model.RetryOptions) model.AttemptResult {
	err := model.ReadSSEWithTimeouts(ctx, body, model.SSEOptions{IdleTimeout: options.IdleTimeout, Control: s.stream.Timers()}, func(event, data []byte) error {
		gate.FirstEvent()
		return s.handle(event, data)
	})
	err = gate.Error(ctx, err)
	if err == nil {
		err = s.finish()
	}
	return model.AttemptResult{Status: 200, SawTool: s.sawTool, SawContent: s.sawContent, Body: s.errorBody, Err: err}
}

func (s *responseState) handle(_ []byte, data []byte) error {
	value, err := decodeResponseEvent(data)
	if err == nil {
		err = s.dispatch(value)
	}
	if err != nil {
		return s.fail(data, err)
	}
	return nil
}

func (s *responseState) dispatch(value responseEvent) error {
	if value.Error != nil {
		return errors.New(errorMessage(value.Error))
	}
	switch value.Type {
	case "message_start":
		s.usage = value.Message.Usage
		s.stream.Emit(s.usage.normalized())
	case "content_block_start", "content_block_delta", "content_block_stop":
		return s.handleContent(value)
	case "message_delta":
		return s.handleMessageDelta(value)
	case "message_stop":
		s.stopped = true
	case "error":
		return errors.New(errorMessage(value.Error))
	}
	return nil
}

func (s *responseState) handleContent(value responseEvent) error {
	switch value.Type {
	case "content_block_start":
		s.startBlock(value.Index, value.ContentBlock)
	case "content_block_delta":
		return s.contentDelta(value.Index, value.Delta)
	case "content_block_stop":
		return s.finishTool(value.Index)
	}
	return nil
}

func (s *responseState) startBlock(index int, block block) {
	if block.Type == "redacted_thinking" {
		s.redacted = append(s.redacted, block.Data)
		return
	}
	if block.Type != "tool_use" {
		return
	}
	tool := s.tool(index)
	tool.ID = block.ID
	tool.Name = block.Name
	tool.Freeform = s.freeform[block.Name]
	if len(block.Input) != 0 && !bytes.Equal(bytes.TrimSpace(block.Input), []byte("{}")) {
		tool.Args = block.Input
	}
	s.sawTool = true
}

func (s *responseState) contentDelta(index int, delta delta) error {
	switch delta.Type {
	case "text_delta":
		s.sawContent = true
		s.stream.Emit(model.TextDelta{Text: delta.Text})
	case "thinking_delta":
		s.sawContent = true
		s.stream.Emit(model.ReasoningDelta{Text: delta.Thinking})
	case "signature_delta":
		s.signature = delta.Signature
	case "input_json_delta":
		tool := s.tool(index)
		tool.Args = append(tool.Args, delta.Partial...)
		s.sawTool = true
	}
	return nil
}

func (s *responseState) tool(index int) *model.ToolCall {
	if s.tools == nil {
		s.tools = make(map[int]*model.ToolCall)
	}
	if s.tools[index] == nil {
		s.tools[index] = &model.ToolCall{}
	}
	return s.tools[index]
}

func (s *responseState) finishTool(index int) error {
	tool, ok := s.tools[index]
	if !ok {
		return nil
	}
	if len(bytes.TrimSpace(tool.Args)) == 0 {
		tool.Args = jsontext.Value(`{}`)
	}
	if !tool.Args.IsValid() {
		return fmt.Errorf("anthropic: tool %s arguments are invalid JSON", tool.Name)
	}
	if tool.Freeform {
		input, err := model.DecodeFreeformInput(tool.Args)
		if err != nil {
			return fmt.Errorf("anthropic: tool %s freeform input: %w", tool.Name, err)
		}
		tool.Args = input
	}
	tool.Signature = s.signature
	tool.RedactedThinking = slices.Clone(s.redacted)
	s.stream.Emit(*tool)
	delete(s.tools, index)
	return nil
}

func (s *responseState) handleMessageDelta(value responseEvent) error {
	if value.HasUsage {
		s.usage.merge(value.Usage)
		s.stream.Emit(s.usage.normalized())
	}
	reason, err := anthropicStopReason(value.Delta.StopReason)
	if err != nil {
		return err
	}
	s.done = true
	s.stream.Emit(model.Done{StopReason: reason})
	return nil
}

func anthropicStopReason(reason string) (string, error) {
	switch reason {
	case "end_turn", "pause_turn", "stop_sequence":
		return "stop", nil
	case "max_tokens":
		return "length", nil
	case "tool_use":
		return "toolUse", nil
	default:
		return "", errors.New("anthropic: provider stop reason: " + reason)
	}
}

func errorMessage(value *providerError) string {
	if value == nil {
		return "anthropic: provider error"
	}
	return "anthropic: " + value.Type + " " + value.Code + ": " + value.Message
}

func (s *responseState) fail(data []byte, err error) error {
	s.errorBody = bytes.Clone(data)
	return err
}

func (s *responseState) finish() error {
	if !s.done || !s.stopped {
		return errors.New("anthropic: stream ended before message_stop")
	}
	if len(s.tools) != 0 {
		return errors.New("anthropic: stream completed with an unfinished tool call")
	}
	return nil
}

func (u usage) normalized() model.Usage {
	return model.Usage{Input: u.Input, Output: u.Output, CacheRead: u.CacheRead, CacheWrite: u.CacheWrite}
}

func (u *usage) merge(other usage) {
	if other.inputSet {
		u.Input = other.Input
	}
	if other.outputSet {
		u.Output = other.Output
	}
	if other.readSet {
		u.CacheRead = other.CacheRead
	}
	if other.writeSet {
		u.CacheWrite = other.CacheWrite
	}
}

func decodeResponseEvent(data []byte) (responseEvent, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(data))
	var value responseEvent
	err := model.ReadObjectFields(decoder, func(key string) error {
		return readResponseField(decoder, key, &value)
	})
	if err != nil {
		return value, err
	}
	if err := model.RequireJSONEOF(decoder); err != nil {
		return value, err
	}
	return value, nil
}

func readResponseField(decoder *jsontext.Decoder, key string, value *responseEvent) error {
	var err error
	switch key {
	case "type":
		value.Type, err = model.ReadString(decoder)
	case "index":
		value.Index, err = model.ReadInt(decoder)
	case "message":
		err = readMessageStart(decoder, &value.Message)
	case "content_block":
		err = readBlock(decoder, &value.ContentBlock)
	case "delta":
		err = readDelta(decoder, &value.Delta)
	default:
		return readResponseNestedField(decoder, key, value)
	}
	return err
}

func readResponseNestedField(decoder *jsontext.Decoder, key string, value *responseEvent) error {
	var err error
	switch key {
	case "usage":
		value.HasUsage = true
		err = readUsage(decoder, &value.Usage)
	case "error":
		value.Error, err = readProviderError(decoder)
	default:
		err = decoder.SkipValue()
	}
	return err
}

func readMessageStart(decoder *jsontext.Decoder, message *messageStart) error {
	return model.ReadObjectFields(decoder, func(key string) error {
		if key != "usage" {
			return decoder.SkipValue()
		}
		return readUsage(decoder, &message.Usage)
	})
}

func readBlock(decoder *jsontext.Decoder, block *block) error {
	return model.ReadObjectFields(decoder, func(key string) error {
		var err error
		switch key {
		case "type":
			block.Type, err = model.ReadString(decoder)
		case "id":
			block.ID, err = model.ReadString(decoder)
		case "name":
			block.Name, err = model.ReadString(decoder)
		case "input":
			block.Input, err = model.ReadValue(decoder)
		case "data":
			block.Data, err = model.ReadString(decoder)
		default:
			err = decoder.SkipValue()
		}
		return err
	})
}

func readDelta(decoder *jsontext.Decoder, delta *delta) error {
	return model.ReadObjectFields(decoder, func(key string) error {
		var err error
		switch key {
		case "type":
			delta.Type, err = model.ReadString(decoder)
		case "text":
			delta.Text, err = model.ReadString(decoder)
		case "thinking":
			delta.Thinking, err = model.ReadString(decoder)
		case "signature":
			delta.Signature, err = model.ReadString(decoder)
		case "partial_json":
			delta.Partial, err = model.ReadString(decoder)
		case "stop_reason":
			delta.StopReason, err = model.ReadString(decoder)
		default:
			err = decoder.SkipValue()
		}
		return err
	})
}

func readProviderError(decoder *jsontext.Decoder) (*providerError, error) {
	if decoder.PeekKind() == 'n' {
		return nil, decoder.SkipValue()
	}
	value := &providerError{}
	err := model.ReadObjectFields(decoder, func(key string) error {
		var err error
		switch key {
		case "type":
			value.Type, err = model.ReadString(decoder)
		case "code":
			value.Code, err = model.ReadScalar(decoder)
		case "message":
			value.Message, err = model.ReadString(decoder)
		default:
			err = decoder.SkipValue()
		}
		return err
	})
	return value, err
}

func readUsage(decoder *jsontext.Decoder, usage *usage) error {
	return model.ReadObjectFields(decoder, func(key string) error {
		switch key {
		case "input_tokens":
			return readUsageInt(decoder, &usage.Input, &usage.inputSet)
		case "output_tokens":
			return readUsageInt(decoder, &usage.Output, &usage.outputSet)
		case "cache_read_input_tokens":
			return readUsageInt(decoder, &usage.CacheRead, &usage.readSet)
		case "cache_creation_input_tokens":
			return readUsageInt(decoder, &usage.CacheWrite, &usage.writeSet)
		default:
			return decoder.SkipValue()
		}
	})
}

func readUsageInt(decoder *jsontext.Decoder, target *int, present *bool) error {
	if decoder.PeekKind() == 'n' {
		return decoder.SkipValue()
	}
	value, err := model.ReadInt(decoder)
	if err == nil {
		*target = value
		*present = true
	}
	return err
}
