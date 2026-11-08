package openai

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"

	"github.com/gosuda/dalgona/internal/model"
)

type responseState struct {
	stream    *model.EventStream
	tools     map[int]*model.ToolCall
	sawTool   bool
	done      bool
	errorBody []byte
}

type responseEvent struct {
	Type        string
	Delta       string
	OutputIndex int
	Item        responseItem
	Arguments   string
	Input       string
	Response    responseDone
	Error       *providerError
	Code        string
	Message     string
}

type responseItem struct {
	Type      string
	ID        string
	CallID    string
	Name      string
	Arguments string
	Input     string
}

type providerError struct {
	Code    string
	Message string
}

type responseDone struct {
	Status           string
	IncompleteReason string
	Usage            responseUsage
	HasUsage         bool
	Error            *providerError
}

type responseUsage struct {
	Input      int
	Output     int
	CacheRead  int
	CacheWrite int
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
	return model.AttemptResult{Status: 200, SawTool: s.sawTool, Body: s.errorBody, Err: err}
}

func (s *responseState) handle(_, data []byte) error {
	if bytes.Equal(data, []byte("[DONE]")) {
		return nil
	}
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
		return errors.New(failureMessage(value))
	}
	switch value.Type {
	case "response.output_text.delta":
		s.stream.Emit(model.TextDelta{Text: value.Delta})
	case "response.refusal.delta", "response.refusal.done":
		return errors.New("openai: refusal")
	case "response.reasoning_summary_text.delta", "response.reasoning_text.delta":
		s.stream.Emit(model.ReasoningDelta{Text: value.Delta})
	case "response.completed", "response.incomplete":
		return s.complete(value)
	case "response.failed", "error":
		return errors.New(failureMessage(value))
	default:
		return s.handleTool(value)
	}
	return nil
}

func (s *responseState) handleTool(value responseEvent) error {
	switch value.Type {
	case "response.output_item.added":
		s.addTool(value.OutputIndex, value.Item)
	case "response.function_call_arguments.delta":
		tool := s.tool(value.OutputIndex)
		tool.Args = append(tool.Args, value.Delta...)
	case "response.function_call_arguments.done":
		s.tool(value.OutputIndex).Args = jsontext.Value(value.Arguments)
	case "response.custom_tool_call_input.delta":
		tool := s.tool(value.OutputIndex)
		tool.Freeform = true
		tool.Args = append(tool.Args, value.Delta...)
	case "response.custom_tool_call_input.done":
		tool := s.tool(value.OutputIndex)
		tool.Freeform = true
		tool.Args = jsontext.Value(value.Input)
	case "response.output_item.done":
		return s.finishTool(value.OutputIndex, value.Item)
	}
	return nil
}

func (s *responseState) addTool(index int, item responseItem) {
	if item.Type != "function_call" && item.Type != "custom_tool_call" {
		return
	}
	tool := s.tool(index)
	tool.ID = item.CallID
	tool.Name = item.Name
	tool.Freeform = item.Type == "custom_tool_call"
	tool.Args = jsontext.Value(item.Arguments)
	if tool.Freeform {
		tool.Args = jsontext.Value(item.Input)
	}
	s.sawTool = true
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

func (s *responseState) finishTool(index int, item responseItem) error {
	if item.Type != "function_call" && item.Type != "custom_tool_call" {
		return nil
	}
	tool := s.tool(index)
	tool.ID = item.CallID
	tool.Name = item.Name
	tool.Freeform = item.Type == "custom_tool_call"
	if item.Arguments != "" {
		tool.Args = jsontext.Value(item.Arguments)
	}
	if item.Input != "" {
		tool.Args = jsontext.Value(item.Input)
	}
	if !tool.Freeform {
		if len(bytes.TrimSpace(tool.Args)) == 0 {
			tool.Args = jsontext.Value(`{}`)
		}
		if !tool.Args.IsValid() {
			return fmt.Errorf("openai: tool %s arguments are invalid JSON", tool.Name)
		}
	}
	s.sawTool = true
	s.stream.Emit(*tool)
	delete(s.tools, index)
	return nil
}

func (s *responseState) complete(value responseEvent) error {
	if value.Response.HasUsage {
		s.stream.Emit(normalizeUsage(value.Response.Usage))
	}
	reason, err := s.stopReason(value.Response.Status, value.Response.IncompleteReason)
	if err != nil {
		return err
	}
	s.done = true
	s.stream.Emit(model.Done{StopReason: reason})
	return nil
}

func normalizeUsage(usage responseUsage) model.Usage {
	return model.Usage{
		Input:      max(usage.Input-usage.CacheRead-usage.CacheWrite, 0),
		Output:     usage.Output,
		CacheRead:  usage.CacheRead,
		CacheWrite: usage.CacheWrite,
	}
}

func (s *responseState) stopReason(status, incomplete string) (string, error) {
	switch status {
	case "completed", "in_progress", "queued", "":
		if s.sawTool {
			return "toolUse", nil
		}
		return "stop", nil
	case "incomplete":
		if incomplete == "max_output_tokens" {
			return "length", nil
		}
		return "", errors.New("openai: response incomplete: " + incomplete)
	default:
		return "", errors.New("openai: response status: " + status)
	}
}

func failureMessage(value responseEvent) string {
	if value.Error != nil {
		return "openai: " + value.Error.Code + ": " + value.Error.Message
	}
	if value.Response.Error != nil {
		return "openai: " + value.Response.Error.Code + ": " + value.Response.Error.Message
	}
	if value.Message != "" || value.Code != "" {
		return "openai: " + value.Code + ": " + value.Message
	}
	return "openai: provider error"
}

func (s *responseState) fail(data []byte, err error) error {
	s.errorBody = bytes.Clone(data)
	return err
}

func (s *responseState) finish() error {
	if !s.done {
		return errors.New("openai: stream ended before a terminal response event")
	}
	if len(s.tools) != 0 {
		return errors.New("openai: stream completed with an unfinished tool call")
	}
	return nil
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
	case "delta":
		value.Delta, err = model.ReadString(decoder)
	case "arguments":
		value.Arguments, err = model.ReadString(decoder)
	case "input":
		value.Input, err = model.ReadString(decoder)
	case "code":
		value.Code, err = model.ReadScalar(decoder)
	case "message":
		value.Message, err = model.ReadString(decoder)
	default:
		return readResponseNestedField(decoder, key, value)
	}
	return err
}

func readResponseNestedField(decoder *jsontext.Decoder, key string, value *responseEvent) error {
	var err error
	switch key {
	case "output_index":
		value.OutputIndex, err = model.ReadInt(decoder)
	case "item":
		err = readResponseItem(decoder, &value.Item)
	case "response":
		err = readResponseDone(decoder, &value.Response)
	case "error":
		value.Error, err = readProviderError(decoder)
	default:
		err = decoder.SkipValue()
	}
	return err
}

func readResponseItem(decoder *jsontext.Decoder, item *responseItem) error {
	return model.ReadObjectFields(decoder, func(key string) error {
		var err error
		switch key {
		case "type":
			item.Type, err = model.ReadString(decoder)
		case "id":
			item.ID, err = model.ReadString(decoder)
		case "call_id":
			item.CallID, err = model.ReadString(decoder)
		case "name":
			item.Name, err = model.ReadString(decoder)
		case "arguments":
			item.Arguments, err = model.ReadString(decoder)
		case "input":
			item.Input, err = model.ReadString(decoder)
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

func readResponseDone(decoder *jsontext.Decoder, response *responseDone) error {
	return model.ReadObjectFields(decoder, func(key string) error {
		var err error
		switch key {
		case "status":
			response.Status, err = model.ReadString(decoder)
		case "incomplete_details":
			err = readIncompleteDetails(decoder, response)
		case "usage":
			response.HasUsage = true
			err = readResponseUsage(decoder, &response.Usage)
		case "error":
			response.Error, err = readProviderError(decoder)
		default:
			err = decoder.SkipValue()
		}
		return err
	})
}

func readIncompleteDetails(decoder *jsontext.Decoder, response *responseDone) error {
	return model.ReadObjectFields(decoder, func(key string) error {
		if key != "reason" {
			return decoder.SkipValue()
		}
		reason, err := model.ReadString(decoder)
		response.IncompleteReason = reason
		return err
	})
}

func readResponseUsage(decoder *jsontext.Decoder, usage *responseUsage) error {
	return model.ReadObjectFields(decoder, func(key string) error {
		var err error
		switch key {
		case "input_tokens":
			usage.Input, err = model.ReadInt(decoder)
		case "output_tokens":
			usage.Output, err = model.ReadInt(decoder)
		case "input_tokens_details":
			err = readInputUsage(decoder, usage)
		default:
			err = decoder.SkipValue()
		}
		return err
	})
}

func readInputUsage(decoder *jsontext.Decoder, usage *responseUsage) error {
	return model.ReadObjectFields(decoder, func(key string) error {
		var err error
		switch key {
		case "cached_tokens":
			usage.CacheRead, err = model.ReadInt(decoder)
		case "cache_write_tokens":
			usage.CacheWrite, err = model.ReadInt(decoder)
		default:
			err = decoder.SkipValue()
		}
		return err
	})
}
