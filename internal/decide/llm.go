package decide

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/model"
)

const llmSystemPrompt = "Answer the questions about the supplied state. Return only one JSON object. Include every question name as a key and no other keys. For a choice question, the value is the selected criterion key as a string. For a noul question, the value is the probability of yes as a number from 0 to 1. For a score question, the value is a number from 0 to len(criteria)-1, the expected index into criteria. Do not return markdown, prose, explanations, probabilities objects, or confidence values."

type LLM struct {
	provider model.Provider
	model    string
	timeout  time.Duration
}

func NewLLM(provider model.Provider, modelID string) *LLM {
	return NewLLMWithTimeout(provider, modelID, askTimeout)
}

func NewLLMWithTimeout(provider model.Provider, modelID string, timeout time.Duration) *LLM {
	if timeout <= 0 {
		timeout = askTimeout
	}
	return &LLM{provider: provider, model: modelID, timeout: timeout}
}

func (l *LLM) Ask(ctx context.Context, state jsontext.Value, questions map[string]Question) (map[string]Answer, error) {
	requestCtx, cancel := context.WithTimeout(ctx, l.timeout)
	defer cancel()
	payload, err := encodeAsk("", state, questions)
	if err != nil {
		return nil, err
	}
	if l.provider == nil {
		return nil, errors.New("llm: provider is required")
	}
	stream, err := l.provider.Stream(requestCtx, model.Request{
		Model:  l.model,
		System: llmSystemPrompt,
		Messages: []model.Message{{
			Role: "user",
			Text: string(payload),
		}},
	})
	if err != nil {
		return nil, fmt.Errorf("llm: stream: %w", err)
	}
	text, err := collectText(requestCtx, stream)
	if err != nil {
		return nil, err
	}
	answers, err := parseLLMAnswers(strings.NewReader(text), questions)
	if err != nil {
		return nil, fmt.Errorf("llm: invalid JSON: %w", err)
	}
	return answers, nil
}

func collectText(ctx context.Context, stream model.Stream) (string, error) {
	defer func() { _ = stream.Close() }()
	var text strings.Builder
	for {
		event, err := stream.Next(ctx)
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			return "", fmt.Errorf("llm: stream: %w", err)
		}
		if err := consumeEvent(&text, event); err != nil {
			return "", err
		}
	}
	if err := stream.Err(); err != nil {
		return "", fmt.Errorf("llm: stream: %w", err)
	}
	return text.String(), nil
}

func consumeEvent(text *strings.Builder, event model.Event) error {
	switch value := event.(type) {
	case model.TextDelta:
		text.WriteString(value.Text)
		return nil
	case model.ReasoningDelta, model.Usage, model.Done:
		return nil
	case model.ToolCall:
		return errors.New("llm: model returned a tool call instead of JSON")
	case model.Error:
		if value.Err != nil {
			return fmt.Errorf("llm: provider: %w", value.Err)
		}
		return errors.New("llm: provider: " + value.Message)
	default:
		return errors.New("llm: unknown model event")
	}
}

func parseLLMAnswers(reader io.Reader, questions map[string]Question) (map[string]Answer, error) {
	decoder := jsontext.NewDecoder(reader)
	if err := readKind(decoder, jsontext.KindBeginObject); err != nil {
		return nil, err
	}
	answers := make(map[string]Answer, len(questions))
	for decoder.PeekKind() != jsontext.KindEndObject {
		name, answer, err := readNamedLLMAnswer(decoder, questions, answers)
		if err != nil {
			return nil, err
		}
		answers[name] = answer
	}
	if err := readKind(decoder, jsontext.KindEndObject); err != nil {
		return nil, err
	}
	if err := requireEOF(decoder); err != nil {
		return nil, err
	}
	if len(answers) != len(questions) {
		return nil, errors.New("missing answers")
	}
	return answers, nil
}

func readNamedLLMAnswer(decoder *jsontext.Decoder, questions map[string]Question, answers map[string]Answer) (string, Answer, error) {
	name, err := readStringToken(decoder)
	if err != nil {
		return "", Answer{}, err
	}
	question, ok := questions[name]
	if !ok {
		return "", Answer{}, fmt.Errorf("unexpected answer %q", name)
	}
	if _, duplicate := answers[name]; duplicate {
		return "", Answer{}, fmt.Errorf("duplicate answer %q", name)
	}
	answer, err := readLLMAnswer(decoder, question)
	if err != nil {
		return "", Answer{}, fmt.Errorf("answer %q: %w", name, err)
	}
	return name, answer, nil
}

func readLLMAnswer(decoder *jsontext.Decoder, question Question) (Answer, error) {
	var answer Answer
	switch question.Type {
	case TypeChoice:
		choice, err := readStringToken(decoder)
		answer.Choice = choice
		return answer, err
	case TypeNoul:
		value, err := readNumberToken(decoder)
		answer.Noul = &value
		return answer, err
	case TypeScore:
		value, err := readNumberToken(decoder)
		if err != nil {
			return answer, err
		}
		limit, err := scoreCriteriaLength(question.Criteria)
		if err != nil {
			return answer, err
		}
		if value < 0 || value > float64(limit-1) {
			return answer, fmt.Errorf("score %v is outside 0..%d", value, limit-1)
		}
		answer.Score = &value
		return answer, nil
	default:
		return answer, fmt.Errorf("unsupported question type %q", question.Type)
	}
}

var _ Client = (*LLM)(nil)
