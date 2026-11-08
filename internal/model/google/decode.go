package google

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
	freeform  map[string]bool
	ids       map[string]bool
	counter   int
	sawTool   bool
	done      bool
	errorBody []byte
}

type responseEvent struct {
	Candidates  []candidate
	Usage       usage
	HasUsage    bool
	Error       *providerError
	BlockReason string
}

type candidate struct {
	Content content
	Finish  string
}

type content struct {
	Role  string
	Parts []part
}

type part struct {
	Text         string
	Thought      bool
	FunctionCall *functionCall
}

type functionCall struct {
	ID   string
	Name string
	Args jsontext.Value
}

type usage struct {
	Input    int
	Output   int
	Cached   int
	Thoughts int
}

type providerError struct {
	Code    string
	Message string
	Status  string
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
	if err := s.decodeData(data); err != nil {
		return s.fail(data, err)
	}
	return nil
}

func (s *responseState) decodeData(data []byte) error {
	decoder := jsontext.NewDecoder(bytes.NewReader(data))
	if decoder.PeekKind() == '[' {
		if err := model.ReadArrayItems(decoder, func(int) error { return s.decodeAndProcess(decoder) }); err != nil {
			return err
		}
	} else if err := s.decodeAndProcess(decoder); err != nil {
		return err
	}
	return model.RequireJSONEOF(decoder)
}

func (s *responseState) decodeAndProcess(decoder *jsontext.Decoder) error {
	value, err := decodeResponseEvent(decoder)
	if err != nil {
		return err
	}
	return s.process(value)
}

func (s *responseState) process(value responseEvent) error {
	if value.Error != nil {
		return errors.New("google: " + value.Error.Code + " " + value.Error.Status + ": " + value.Error.Message)
	}
	if value.BlockReason != "" {
		return errors.New("google: prompt blocked: " + value.BlockReason)
	}
	if value.HasUsage {
		s.stream.Emit(value.Usage.normalized())
	}
	if len(value.Candidates) == 0 {
		return nil
	}
	return s.processCandidate(value.Candidates[0])
}

func (s *responseState) processCandidate(candidate candidate) error {
	for _, part := range candidate.Content.Parts {
		if err := s.processPart(part); err != nil {
			return err
		}
	}
	if candidate.Finish == "" {
		return nil
	}
	reason, err := s.stopReason(candidate.Finish)
	if err != nil {
		return err
	}
	s.done = true
	s.stream.Emit(model.Done{StopReason: reason})
	return nil
}

func (s *responseState) processPart(part part) error {
	if part.FunctionCall != nil {
		return s.emitTool(*part.FunctionCall)
	}
	if part.Text == "" {
		return nil
	}
	if part.Thought {
		s.stream.Emit(model.ReasoningDelta{Text: part.Text})
		return nil
	}
	s.stream.Emit(model.TextDelta{Text: part.Text})
	return nil
}

func (s *responseState) emitTool(call functionCall) error {
	s.counter++
	id := call.ID
	if id == "" || s.ids[id] {
		id = fmt.Sprintf("%s_%d", call.Name, s.counter)
	}
	s.ids[id] = true
	args := call.Args
	if len(args) == 0 {
		args = jsontext.Value(`{}`)
	}
	tool := model.ToolCall{ID: id, Name: call.Name, Args: args, Freeform: s.freeform[call.Name]}
	if tool.Freeform {
		input, err := model.DecodeFreeformInput(args)
		if err != nil {
			return fmt.Errorf("google: tool %s freeform input: %w", call.Name, err)
		}
		tool.Args = input
	}
	s.sawTool = true
	s.stream.Emit(tool)
	return nil
}

func (s *responseState) stopReason(finish string) (string, error) {
	switch finish {
	case "STOP":
		if s.sawTool {
			return "toolUse", nil
		}
		return "stop", nil
	case "MAX_TOKENS":
		return "length", nil
	default:
		return "", errors.New("google: provider stopped with: " + finish)
	}
}

func (s *responseState) fail(data []byte, err error) error {
	s.errorBody = bytes.Clone(data)
	return err
}

func (s *responseState) finish() error {
	if !s.done {
		return errors.New("google: stream ended without a finish reason")
	}
	return nil
}

func (u usage) normalized() model.Usage {
	return model.Usage{
		Input:     max(u.Input-u.Cached, 0),
		Output:    u.Output + u.Thoughts,
		CacheRead: u.Cached,
	}
}

func decodeResponseEvent(decoder *jsontext.Decoder) (responseEvent, error) {
	var value responseEvent
	err := model.ReadObjectFields(decoder, func(key string) error {
		var err error
		switch key {
		case "candidates":
			err = readCandidates(decoder, &value.Candidates)
		case "usageMetadata":
			value.HasUsage = true
			err = readUsage(decoder, &value.Usage)
		case "promptFeedback":
			err = readPromptFeedback(decoder, &value.BlockReason)
		case "error":
			value.Error, err = readProviderError(decoder)
		default:
			err = decoder.SkipValue()
		}
		return err
	})
	return value, err
}

func readCandidates(decoder *jsontext.Decoder, candidates *[]candidate) error {
	return model.ReadArrayItems(decoder, func(index int) error {
		if index != 0 {
			return decoder.SkipValue()
		}
		var value candidate
		if err := readCandidate(decoder, &value); err != nil {
			return err
		}
		*candidates = append(*candidates, value)
		return nil
	})
}

func readCandidate(decoder *jsontext.Decoder, candidate *candidate) error {
	return model.ReadObjectFields(decoder, func(key string) error {
		var err error
		switch key {
		case "content":
			err = readContent(decoder, &candidate.Content)
		case "finishReason":
			err = readNullableString(decoder, &candidate.Finish)
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

func readContent(decoder *jsontext.Decoder, content *content) error {
	return model.ReadObjectFields(decoder, func(key string) error {
		var err error
		switch key {
		case "role":
			content.Role, err = model.ReadString(decoder)
		case "parts":
			err = readParts(decoder, &content.Parts)
		default:
			err = decoder.SkipValue()
		}
		return err
	})
}

func readParts(decoder *jsontext.Decoder, parts *[]part) error {
	return model.ReadArrayItems(decoder, func(_ int) error {
		var value part
		if err := readPart(decoder, &value); err != nil {
			return err
		}
		*parts = append(*parts, value)
		return nil
	})
}

func readPart(decoder *jsontext.Decoder, part *part) error {
	return model.ReadObjectFields(decoder, func(key string) error {
		var err error
		switch key {
		case "text":
			err = readNullableString(decoder, &part.Text)
		case "thought":
			part.Thought, err = model.ReadBool(decoder)
		case "functionCall":
			part.FunctionCall, err = readFunctionCall(decoder)
		default:
			err = decoder.SkipValue()
		}
		return err
	})
}

func readFunctionCall(decoder *jsontext.Decoder) (*functionCall, error) {
	value := &functionCall{}
	err := model.ReadObjectFields(decoder, func(key string) error {
		var err error
		switch key {
		case "id":
			value.ID, err = model.ReadString(decoder)
		case "name":
			value.Name, err = model.ReadString(decoder)
		case "args":
			value.Args, err = model.ReadValue(decoder)
		default:
			err = decoder.SkipValue()
		}
		return err
	})
	return value, err
}

func readUsage(decoder *jsontext.Decoder, usage *usage) error {
	return model.ReadObjectFields(decoder, func(key string) error {
		var err error
		switch key {
		case "promptTokenCount":
			usage.Input, err = model.ReadInt(decoder)
		case "candidatesTokenCount":
			usage.Output, err = model.ReadInt(decoder)
		case "cachedContentTokenCount":
			usage.Cached, err = model.ReadInt(decoder)
		case "thoughtsTokenCount":
			usage.Thoughts, err = model.ReadInt(decoder)
		default:
			err = decoder.SkipValue()
		}
		return err
	})
}

func readPromptFeedback(decoder *jsontext.Decoder, blockReason *string) error {
	return model.ReadObjectFields(decoder, func(key string) error {
		if key != "blockReason" {
			return decoder.SkipValue()
		}
		return readNullableString(decoder, blockReason)
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
		case "status":
			value.Status, err = model.ReadString(decoder)
		default:
			err = decoder.SkipValue()
		}
		return err
	})
	return value, err
}
