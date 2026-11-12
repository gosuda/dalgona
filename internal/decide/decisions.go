package decide

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"net/http"
	"time"
)

const defaultDecisionsURL = "https://openrouter.ai/api/alpha/decisions"

type TokenFunc func(context.Context) (string, error)

type DecisionsConfig struct {
	BaseURL    string
	Model      string
	Token      TokenFunc
	HTTPClient *http.Client
	Timeout    time.Duration
}

type Decisions struct {
	endpoint string
	model    string
	token    TokenFunc
	client   *http.Client
	timeout  time.Duration
}

func NewDecisions(config DecisionsConfig) *Decisions {
	if config.BaseURL == "" {
		config.BaseURL = defaultDecisionsURL
	}
	if config.HTTPClient == nil {
		config.HTTPClient = &http.Client{Timeout: 0}
	}
	if config.Timeout <= 0 {
		config.Timeout = askTimeout
	}
	return &Decisions{endpoint: config.BaseURL, model: config.Model, token: config.Token, client: config.HTTPClient, timeout: config.Timeout}
}

func (d *Decisions) Ask(ctx context.Context, state jsontext.Value, questions map[string]Question) (map[string]Answer, error) {
	if d.model == "" {
		return nil, errors.New("decisions: model identifier is required")
	}
	requestCtx, cancel := context.WithTimeout(ctx, d.timeout)
	defer cancel()
	payload, err := encodeAsk(d.model, state, questions)
	if err != nil {
		return nil, err
	}
	token, err := d.bearerToken(requestCtx)
	if err != nil {
		return nil, err
	}
	req, err := http.NewRequestWithContext(requestCtx, http.MethodPost, d.endpoint, bytes.NewReader(payload))
	if err != nil {
		return nil, fmt.Errorf("decisions: create request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+token)
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json")
	resp, err := d.client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("decisions: request: %w", err)
	}
	defer func() { _ = resp.Body.Close() }()
	if resp.StatusCode < http.StatusOK || resp.StatusCode >= http.StatusMultipleChoices {
		return nil, decisionsHTTPError(resp)
	}
	answers, err := decodeDecisionAnswers(resp.Body, questions)
	if err != nil {
		return nil, fmt.Errorf("decisions: decode response: %w", err)
	}
	return answers, nil
}

func (d *Decisions) bearerToken(ctx context.Context) (string, error) {
	if d.token == nil {
		return "", fmt.Errorf("decisions: token function is required")
	}
	token, err := d.token(ctx)
	if err != nil {
		return "", fmt.Errorf("decisions: token: %w", err)
	}
	return token, nil
}

func decisionsHTTPError(resp *http.Response) error {
	body, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if err != nil {
		return fmt.Errorf("decisions: read HTTP %d response: %w", resp.StatusCode, err)
	}
	return fmt.Errorf("decisions: http %d: %s", resp.StatusCode, bytes.TrimSpace(body))
}

type wireAnswer struct {
	typeName      string
	choice        *string
	noul          *float64
	score         *float64
	probabilities map[string]float64
	confidence    float64
}

// source: https://openrouter.ai/docs/api/api-reference/alphadecisions/submit-a-decisions-request.md
func decodeDecisionAnswers(reader io.Reader, questions map[string]Question) (map[string]Answer, error) {
	decoder := jsontext.NewDecoder(reader)
	if err := readKind(decoder, jsontext.KindBeginObject); err != nil {
		return nil, err
	}
	wire, err := readDecisionFields(decoder)
	if err != nil {
		return nil, err
	}
	if err := requireEOF(decoder); err != nil {
		return nil, err
	}
	return convertAnswers(wire, questions)
}

func readDecisionFields(decoder *jsontext.Decoder) (map[string]wireAnswer, error) {
	var answers map[string]wireAnswer
	for decoder.PeekKind() != jsontext.KindEndObject {
		name, err := readStringToken(decoder)
		if err != nil {
			return nil, err
		}
		if name != "answers" {
			if skipErr := decoder.SkipValue(); skipErr != nil {
				return nil, skipErr
			}
			continue
		}
		answers, err = readAnswerMap(decoder)
		if err != nil {
			return nil, err
		}
	}
	if err := readKind(decoder, jsontext.KindEndObject); err != nil {
		return nil, err
	}
	if answers == nil {
		return nil, fmt.Errorf("missing answers")
	}
	return answers, nil
}

func readAnswerMap(decoder *jsontext.Decoder) (map[string]wireAnswer, error) {
	if err := readKind(decoder, jsontext.KindBeginObject); err != nil {
		return nil, err
	}
	answers := make(map[string]wireAnswer)
	for decoder.PeekKind() != jsontext.KindEndObject {
		name, err := readStringToken(decoder)
		if err != nil {
			return nil, err
		}
		answer, err := readWireAnswer(decoder)
		if err != nil {
			return nil, fmt.Errorf("answer %q: %w", name, err)
		}
		answers[name] = answer
	}
	if err := readKind(decoder, jsontext.KindEndObject); err != nil {
		return nil, err
	}
	return answers, nil
}

func readWireAnswer(decoder *jsontext.Decoder) (wireAnswer, error) {
	var answer wireAnswer
	if err := readKind(decoder, jsontext.KindBeginObject); err != nil {
		return answer, err
	}
	for decoder.PeekKind() != jsontext.KindEndObject {
		name, err := readStringToken(decoder)
		if err != nil {
			return answer, err
		}
		if err := readAnswerField(decoder, name, &answer); err != nil {
			return answer, err
		}
	}
	if err := readKind(decoder, jsontext.KindEndObject); err != nil {
		return answer, err
	}
	return answer, nil
}

func readAnswerField(decoder *jsontext.Decoder, name string, answer *wireAnswer) error {
	var err error
	switch name {
	case "type":
		answer.typeName, err = readStringToken(decoder)
	case "choice":
		var value string
		value, err = readStringToken(decoder)
		answer.choice = &value
	case "noul":
		var value float64
		value, err = readNumberToken(decoder)
		answer.noul = &value
	case "score":
		var value float64
		value, err = readNumberToken(decoder)
		answer.score = &value
	case "probabilities":
		answer.probabilities, err = readProbabilities(decoder)
	case "confidence":
		answer.confidence, err = readNumberToken(decoder)
	default:
		err = decoder.SkipValue()
	}
	return err
}

func readProbabilities(decoder *jsontext.Decoder) (map[string]float64, error) {
	if err := readKind(decoder, jsontext.KindBeginObject); err != nil {
		return nil, err
	}
	probabilities := make(map[string]float64)
	for decoder.PeekKind() != jsontext.KindEndObject {
		name, err := readStringToken(decoder)
		if err != nil {
			return nil, err
		}
		value, err := readNumberToken(decoder)
		if err != nil {
			return nil, err
		}
		probabilities[name] = value
	}
	if err := readKind(decoder, jsontext.KindEndObject); err != nil {
		return nil, err
	}
	return probabilities, nil
}

func convertAnswers(wire map[string]wireAnswer, questions map[string]Question) (map[string]Answer, error) {
	answers := make(map[string]Answer, len(questions))
	for name, question := range questions {
		encoded, ok := wire[name]
		if !ok {
			return nil, fmt.Errorf("missing answer %q", name)
		}
		answer, err := encoded.convert(question)
		if err != nil {
			return nil, fmt.Errorf("answer %q: %w", name, err)
		}
		answers[name] = answer
	}
	return answers, nil
}

func (w wireAnswer) convert(question Question) (Answer, error) {
	answer := Answer{Probabilities: w.probabilities, Confidence: w.confidence}
	if w.typeName != string(question.Type) {
		return answer, fmt.Errorf("type is %q, want %q", w.typeName, question.Type)
	}
	switch question.Type {
	case TypeChoice:
		if w.choice == nil {
			return answer, fmt.Errorf("missing choice")
		}
		answer.Choice = *w.choice
	case TypeNoul:
		if w.noul == nil {
			return answer, fmt.Errorf("missing noul")
		}
		answer.Noul = w.noul
	case TypeScore:
		if w.score == nil {
			return answer, fmt.Errorf("missing score")
		}
		answer.Score = w.score
	}
	return answer, nil
}

var _ Client = (*Decisions)(nil)
