package scripthost

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"fmt"

	"github.com/gosuda/dalgona/internal/dal/decide"
	"github.com/gosuda/dalgona/internal/dal/model"
	"github.com/gosuda/dalgona/internal/dal/script"
)

func (fx effects) Judge(ctx context.Context, _ script.Slot, req script.JudgeRequest) (jsontext.Value, error) {
	if fx.judge == nil {
		return nil, errNoJudge
	}
	settings, err := fx.t.Settings()
	if err != nil {
		return nil, fmt.Errorf("scripthost: judge thread model: %w", err)
	}
	client, err := fx.judge(ctx, req.Model, settings.Model)
	if err != nil {
		return nil, fmt.Errorf("scripthost: judge %q: %w", req.Model, err)
	}
	questions, err := judgeQuestions(req.Questions)
	if err != nil {
		return nil, err
	}
	result, err := client.Ask(ctx, req.State, questions)
	if err != nil {
		return nil, fmt.Errorf("scripthost: judge: %w", err)
	}
	return judgeAnswers(result)
}

func judgeQuestions(raw jsontext.Value) (map[string]decide.Question, error) {
	questions := make(map[string]decide.Question)
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	err := model.ReadObjectFields(decoder, func(name string) error {
		question, err := judgeQuestion(decoder)
		questions[name] = question
		return err
	})
	if err != nil {
		return nil, fmt.Errorf("scripthost: judge questions: %w", err)
	}
	return questions, nil
}

func judgeQuestion(decoder *jsontext.Decoder) (decide.Question, error) {
	var question decide.Question
	err := model.ReadObjectFields(decoder, func(name string) error {
		switch name {
		case "type":
			kind, err := model.ReadString(decoder)
			question.Type = decide.QuestionType(kind)
			return err
		case "instructions":
			text, err := model.ReadString(decoder)
			question.Instructions = text
			return err
		case "criteria":
			value, err := decoder.ReadValue()
			question.Criteria = jsontext.Value(bytes.Clone(value))
			return err
		default:
			return decoder.SkipValue()
		}
	})
	return question, err
}

func judgeAnswers(result decide.Result) (jsontext.Value, error) {
	var out bytes.Buffer
	encoder := model.NewJSONEncoder(&out)
	encoder.BeginObject()
	encoder.Key("answers")
	encoder.BeginObject()
	for name, answer := range result.Answers {
		encoder.Key(name)
		writeAnswer(encoder, answer)
	}
	encoder.EndObject()
	encoder.StringKey("model", result.Model)
	if result.Fallback != "" {
		encoder.StringKey("fallback", result.Fallback)
	}
	encoder.EndObject()
	if err := encoder.Err(); err != nil {
		return nil, fmt.Errorf("scripthost: encode judge answers: %w", err)
	}
	return jsontext.Value(bytes.TrimSpace(out.Bytes())), nil
}

func writeAnswer(encoder *model.JSONEncoder, answer decide.Answer) {
	encoder.BeginObject()
	if answer.Choice != "" {
		encoder.StringKey("choice", answer.Choice)
	}
	writeNumber(encoder, "noul", answer.Noul)
	writeNumber(encoder, "score", answer.Score)
	if len(answer.Probabilities) > 0 {
		encoder.Key("probabilities")
		encoder.BeginObject()
		for label, p := range answer.Probabilities {
			encoder.Key(label)
			encoder.RawToken(jsontext.Float(p))
		}
		encoder.EndObject()
		encoder.Key("confidence")
		encoder.RawToken(jsontext.Float(answer.Confidence))
	}
	encoder.EndObject()
}

func writeNumber(encoder *model.JSONEncoder, key string, value *float64) {
	if value != nil {
		encoder.Key(key)
		encoder.RawToken(jsontext.Float(*value))
	}
}
