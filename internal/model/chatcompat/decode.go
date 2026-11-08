package chatcompat

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
	calls     map[int]*model.ToolCall
	order     []int
	idIndexes map[string]int
	nextIndex int
	freeform  map[string]bool
	sawTool   bool
	done      bool
	errorBody []byte
}

type responseEvent struct {
	Choices  []choice
	Usage    usage
	HasUsage bool
	Error    *providerError
}

type choice struct {
	Index    int
	Delta    delta
	Finish   string
	Usage    usage
	HasUsage bool
}

type delta struct {
	Content          string
	ReasoningContent string
	Reasoning        string
	ReasoningText    string
	ToolCalls        []toolDelta
}

type toolDelta struct {
	Index    int
	HasIndex bool
	ID       string
	Function callFunction
	Custom   customFunction
}

type callFunction struct {
	Name      string
	Arguments jsontext.Value
}

type customFunction struct {
	Name  string
	Input string
}

type usage struct {
	Prompt          int
	Completion      int
	CacheRead       int
	CacheWrite      int
	cacheReadSource cacheSource
}

type providerError struct {
	Code    string
	Message string
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
		return errors.New("chatcompat: " + value.Error.Code + ": " + value.Error.Message)
	}
	if value.HasUsage {
		s.stream.Emit(value.Usage.normalized())
	} else if len(value.Choices) != 0 && value.Choices[0].HasUsage {
		s.stream.Emit(value.Choices[0].Usage.normalized())
	}
	if len(value.Choices) == 0 {
		return nil
	}
	return s.handleChoice(value.Choices[0])
}

func (s *responseState) handleChoice(choice choice) error {
	if choice.Delta.Content != "" {
		s.stream.Emit(model.TextDelta{Text: choice.Delta.Content})
	}
	if reasoning := choice.Delta.reasoning(); reasoning != "" {
		s.stream.Emit(model.ReasoningDelta{Text: reasoning})
	}
	for _, call := range choice.Delta.ToolCalls {
		s.applyToolDelta(call)
	}
	if choice.Finish == "" {
		return nil
	}
	reason, err := chatcompatStopReason(choice.Finish)
	if err != nil {
		return err
	}
	if err := s.emitCalls(); err != nil {
		return err
	}
	s.done = true
	s.stream.Emit(model.Done{StopReason: reason})
	return nil
}

func (d delta) reasoning() string {
	if d.ReasoningContent != "" {
		return d.ReasoningContent
	}
	if d.Reasoning != "" {
		return d.Reasoning
	}
	return d.ReasoningText
}

func chatcompatStopReason(reason string) (string, error) {
	switch reason {
	case "stop", "end":
		return "stop", nil
	case "length":
		return "length", nil
	case "function_call", "tool_calls":
		return "toolUse", nil
	default:
		return "", errors.New("chatcompat: provider finish_reason: " + reason)
	}
}

func (s *responseState) applyToolDelta(value toolDelta) {
	call := s.tool(value)
	name := value.Function.Name
	if name == "" {
		name = value.Custom.Name
	}
	if call.Name == "" {
		call.Name = name
	}
	if value.ID != "" {
		call.ID = value.ID
	}
	call.Args = append(call.Args, value.Function.Arguments...)
	if value.Custom.Input != "" {
		call.Freeform = true
		call.Args = append(call.Args, value.Custom.Input...)
	}
	s.sawTool = true
}

func (s *responseState) tool(value toolDelta) *model.ToolCall {
	index := s.toolIndex(value)
	if s.calls == nil {
		s.calls = make(map[int]*model.ToolCall)
	}
	if s.calls[index] == nil {
		s.calls[index] = &model.ToolCall{}
		s.order = append(s.order, index)
	}
	if value.ID != "" {
		if s.idIndexes == nil {
			s.idIndexes = make(map[string]int)
		}
		s.idIndexes[value.ID] = index
	}
	return s.calls[index]
}

func (s *responseState) toolIndex(value toolDelta) int {
	if value.HasIndex {
		s.nextIndex = max(s.nextIndex, value.Index+1)
		return value.Index
	}
	if index, ok := s.idIndexes[value.ID]; ok {
		return index
	}
	index := s.nextIndex
	s.nextIndex++
	return index
}

func (s *responseState) emitCalls() error {
	for _, index := range s.order {
		call := s.calls[index]
		if err := s.finishCall(call); err != nil {
			return err
		}
		s.stream.Emit(*call)
	}
	s.calls = nil
	s.order = nil
	return nil
}

func (s *responseState) finishCall(call *model.ToolCall) error {
	if s.freeform[call.Name] || call.Freeform {
		input, err := model.DecodeFreeformInput(call.Args)
		if err != nil && !call.Freeform {
			return fmt.Errorf("chatcompat: tool %s freeform input: %w", call.Name, err)
		}
		if err == nil {
			call.Args = input
		}
		call.Freeform = true
		return nil
	}
	if len(bytes.TrimSpace(call.Args)) == 0 {
		call.Args = jsontext.Value(`{}`)
	}
	if !call.Args.IsValid() {
		return fmt.Errorf("chatcompat: tool %s arguments are invalid JSON", call.Name)
	}
	return nil
}

func (s *responseState) fail(data []byte, err error) error {
	s.errorBody = bytes.Clone(data)
	return err
}

func (s *responseState) finish() error {
	if !s.done {
		return errors.New("chatcompat: stream ended without a finish reason")
	}
	if len(s.calls) != 0 {
		return errors.New("chatcompat: stream completed with unfinished tool calls")
	}
	return nil
}

func (u usage) normalized() model.Usage {
	return model.Usage{
		Input:      max(u.Prompt-u.CacheRead-u.CacheWrite, 0),
		Output:     u.Completion,
		CacheRead:  u.CacheRead,
		CacheWrite: u.CacheWrite,
	}
}

func decodeResponseEvent(data []byte) (responseEvent, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(data))
	var value responseEvent
	err := model.ReadObjectFields(decoder, func(key string) error {
		var err error
		switch key {
		case "choices":
			err = readChoices(decoder, &value.Choices)
		case "usage":
			value.HasUsage = true
			err = readUsage(decoder, &value.Usage)
		case "error":
			value.Error, err = readProviderError(decoder)
		default:
			err = decoder.SkipValue()
		}
		return err
	})
	if err != nil {
		return value, err
	}
	if err := model.RequireJSONEOF(decoder); err != nil {
		return value, err
	}
	return value, nil
}

func readChoices(decoder *jsontext.Decoder, choices *[]choice) error {
	return model.ReadArrayItems(decoder, func(index int) error {
		if index != 0 {
			return decoder.SkipValue()
		}
		var value choice
		if err := readChoice(decoder, &value); err != nil {
			return err
		}
		*choices = append(*choices, value)
		return nil
	})
}

func readChoice(decoder *jsontext.Decoder, choice *choice) error {
	return model.ReadObjectFields(decoder, func(key string) error {
		var err error
		switch key {
		case "index":
			choice.Index, err = model.ReadInt(decoder)
		case "delta":
			err = readDelta(decoder, &choice.Delta)
		case "finish_reason":
			err = readNullableString(decoder, &choice.Finish)
		case "usage":
			choice.HasUsage = true
			err = readUsage(decoder, &choice.Usage)
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
		case "content":
			err = readNullableString(decoder, &delta.Content)
		case "reasoning_content":
			err = readReasoningString(decoder, &delta.ReasoningContent)
		case "reasoning":
			err = readReasoningString(decoder, &delta.Reasoning)
		case "reasoning_text":
			err = readReasoningString(decoder, &delta.ReasoningText)
		case "tool_calls":
			err = readToolDeltas(decoder, &delta.ToolCalls)
		default:
			err = decoder.SkipValue()
		}
		return err
	})
}

func readNullableString(decoder *jsontext.Decoder, target *string) error {
	if decoder.PeekKind() == 'n' {
		return decoder.SkipValue()
	}
	value, err := model.ReadString(decoder)
	if err == nil {
		*target = value
	}
	return err
}

func readReasoningString(decoder *jsontext.Decoder, target *string) error {
	if decoder.PeekKind() != '"' {
		return decoder.SkipValue()
	}
	return readNullableString(decoder, target)
}

func readToolDeltas(decoder *jsontext.Decoder, calls *[]toolDelta) error {
	return model.ReadArrayItems(decoder, func(_ int) error {
		var value toolDelta
		err := model.ReadObjectFields(decoder, func(key string) error {
			return readToolDeltaField(decoder, key, &value)
		})
		if err == nil {
			*calls = append(*calls, value)
		}
		return err
	})
}

func readToolDeltaField(decoder *jsontext.Decoder, key string, value *toolDelta) error {
	var err error
	switch key {
	case "index":
		value.HasIndex = true
		value.Index, err = model.ReadInt(decoder)
	case "id":
		value.ID, err = model.ReadString(decoder)
	case "function":
		err = readCallFunction(decoder, &value.Function)
	case "custom":
		err = readCustomFunction(decoder, &value.Custom)
	default:
		err = decoder.SkipValue()
	}
	return err
}

func readCallFunction(decoder *jsontext.Decoder, function *callFunction) error {
	return model.ReadObjectFields(decoder, func(key string) error {
		var err error
		switch key {
		case "name":
			function.Name, err = model.ReadString(decoder)
		case "arguments":
			function.Arguments, err = readToolArguments(decoder)
		default:
			err = decoder.SkipValue()
		}
		return err
	})
}

func readToolArguments(decoder *jsontext.Decoder) (jsontext.Value, error) {
	if decoder.PeekKind() == 'n' {
		return nil, decoder.SkipValue()
	}
	if decoder.PeekKind() == '"' {
		value, err := model.ReadString(decoder)
		return jsontext.Value(value), err
	}
	return model.ReadValue(decoder)
}

func readCustomFunction(decoder *jsontext.Decoder, function *customFunction) error {
	return model.ReadObjectFields(decoder, func(key string) error {
		var err error
		switch key {
		case "name":
			function.Name, err = model.ReadString(decoder)
		case "input":
			function.Input, err = model.ReadString(decoder)
		default:
			err = decoder.SkipValue()
		}
		return err
	})
}

func readUsage(decoder *jsontext.Decoder, usage *usage) error {
	return model.ReadObjectFields(decoder, func(key string) error {
		var err error
		switch key {
		case "prompt_tokens":
			usage.Prompt, err = model.ReadInt(decoder)
		case "completion_tokens":
			usage.Completion, err = model.ReadInt(decoder)
		case "prompt_cache_hit_tokens":
			err = readCacheRead(decoder, usage, cacheFromPromptHit)
		case "cached_tokens":
			err = readCacheRead(decoder, usage, cacheFromCachedTokens)
		case "prompt_tokens_details":
			err = readPromptUsage(decoder, usage)
		default:
			err = decoder.SkipValue()
		}
		return err
	})
}

func readPromptUsage(decoder *jsontext.Decoder, usage *usage) error {
	return model.ReadObjectFields(decoder, func(key string) error {
		var err error
		switch key {
		case "cached_tokens":
			err = readCacheRead(decoder, usage, cacheFromDetails)
		case "cache_write_tokens":
			usage.CacheWrite, err = model.ReadInt(decoder)
		default:
			err = decoder.SkipValue()
		}
		return err
	})
}

type cacheSource uint8

const (
	cacheFromCachedTokens cacheSource = iota + 1
	cacheFromPromptHit
	cacheFromDetails
)

func readCacheRead(decoder *jsontext.Decoder, usage *usage, source cacheSource) error {
	value, err := model.ReadInt(decoder)
	if err == nil && source >= usage.cacheReadSource {
		usage.CacheRead = value
		usage.cacheReadSource = source
	}
	return err
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
