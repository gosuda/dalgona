package decide

import (
	"bytes"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"maps"
	"slices"
)

func encodeAsk(modelID string, state jsontext.Value, questions map[string]Question) ([]byte, error) {
	if err := validateState(state); err != nil {
		return nil, err
	}
	var out bytes.Buffer
	out.Grow(len(state) + 256)
	encoder := jsontext.NewEncoder(&out)
	if err := writeAsk(encoder, modelID, state, questions); err != nil {
		return nil, fmt.Errorf("decide: encode request: %w", err)
	}
	return out.Bytes(), nil
}

func validateState(state jsontext.Value) error {
	if !state.IsValid() {
		return errors.New("decide: state must be valid JSON")
	}
	kind := state.Kind()
	if kind != jsontext.KindString && kind != jsontext.KindBeginObject && kind != jsontext.KindBeginArray {
		return errors.New("decide: state must be a JSON string, object, or array")
	}
	return nil
}

func writeAsk(encoder *jsontext.Encoder, modelID string, state jsontext.Value, questions map[string]Question) error {
	if err := encoder.WriteToken(jsontext.BeginObject); err != nil {
		return err
	}
	if modelID != "" {
		if err := writeNameToken(encoder, "model", jsontext.String(modelID)); err != nil {
			return err
		}
	}
	if err := writeNameValue(encoder, "state", state); err != nil {
		return err
	}
	if err := encoder.WriteToken(jsontext.String("questions")); err != nil {
		return err
	}
	if err := writeQuestions(encoder, questions); err != nil {
		return err
	}
	return encoder.WriteToken(jsontext.EndObject)
}

func writeQuestions(encoder *jsontext.Encoder, questions map[string]Question) error {
	if err := encoder.WriteToken(jsontext.BeginObject); err != nil {
		return err
	}
	for _, name := range slices.Sorted(maps.Keys(questions)) {
		question := questions[name]
		if err := validateQuestion(question); err != nil {
			return fmt.Errorf("question %q: %w", name, err)
		}
		if err := encoder.WriteToken(jsontext.String(name)); err != nil {
			return err
		}
		if err := writeQuestion(encoder, question); err != nil {
			return err
		}
	}
	return encoder.WriteToken(jsontext.EndObject)
}

func validateQuestion(question Question) error {
	switch question.Type {
	case TypeNoul:
		if question.Criteria == nil {
			return nil
		}
		return validateObjectCriteria(question.Criteria)
	case TypeChoice:
		if question.Criteria == nil {
			return errors.New("criteria are required")
		}
		return validateObjectCriteria(question.Criteria)
	case TypeScore:
		if question.Criteria == nil {
			return errors.New("criteria are required")
		}
		_, err := scoreCriteriaLength(question.Criteria)
		return err
	default:
		return fmt.Errorf("unsupported question type %q", question.Type)
	}
}

func validateObjectCriteria(criterion any) error {
	switch value := criterion.(type) {
	case map[string]string, map[string]any:
		return nil
	case jsontext.Value:
		if value.Kind() == jsontext.KindBeginObject && value.IsValid() {
			return nil
		}
		return errors.New("criteria must be an object")
	default:
		return errors.New("criteria must be an object")
	}
}

func scoreCriteriaLength(criterion any) (int, error) {
	var length int
	switch value := criterion.(type) {
	case []string:
		length = len(value)
	case []any:
		length = len(value)
	case jsontext.Value:
		if value.Kind() != jsontext.KindBeginArray || !value.IsValid() {
			return 0, errors.New("score criteria must be a non-empty array")
		}
		var err error
		length, err = jsonArrayLength(value)
		if err != nil {
			return 0, fmt.Errorf("score criteria: %w", err)
		}
	default:
		return 0, errors.New("score criteria must be a non-empty array")
	}
	if length == 0 {
		return 0, errors.New("score criteria must be a non-empty array")
	}
	return length, nil
}

func jsonArrayLength(raw jsontext.Value) (int, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	if err := readKind(decoder, jsontext.KindBeginArray); err != nil {
		return 0, err
	}
	length := 0
	for decoder.PeekKind() != jsontext.KindEndArray {
		if err := decoder.SkipValue(); err != nil {
			return 0, err
		}
		length++
	}
	if err := readKind(decoder, jsontext.KindEndArray); err != nil {
		return 0, err
	}
	if err := requireEOF(decoder); err != nil {
		return 0, err
	}
	return length, nil
}

func writeQuestion(encoder *jsontext.Encoder, question Question) error {
	if err := encoder.WriteToken(jsontext.BeginObject); err != nil {
		return err
	}
	if err := writeNameToken(encoder, "type", jsontext.String(string(question.Type))); err != nil {
		return err
	}
	if question.Instructions != "" {
		if err := writeNameToken(encoder, "instructions", jsontext.String(question.Instructions)); err != nil {
			return err
		}
	}
	if question.Criteria != nil {
		if err := encoder.WriteToken(jsontext.String("criteria")); err != nil {
			return err
		}
		if err := writeCriterion(encoder, question.Criteria); err != nil {
			return err
		}
	}
	return encoder.WriteToken(jsontext.EndObject)
}

func writeCriterion(encoder *jsontext.Encoder, criterion any) error {
	switch value := criterion.(type) {
	case map[string]string:
		return writeStringMap(encoder, value)
	case map[string]any:
		return writeAnyMap(encoder, value)
	case []string:
		return writeStrings(encoder, value)
	case []any:
		return writeAnySlice(encoder, value)
	default:
		return writeScalarCriterion(encoder, criterion)
	}
}

func writeScalarCriterion(encoder *jsontext.Encoder, criterion any) error {
	switch value := criterion.(type) {
	case nil:
		return encoder.WriteToken(jsontext.Null)
	case string:
		return encoder.WriteToken(jsontext.String(value))
	case bool:
		return encoder.WriteToken(jsontext.Bool(value))
	case float64:
		return encoder.WriteToken(jsontext.Float(value))
	case jsontext.Value:
		if !value.IsValid() {
			return errors.New("criterion must be valid JSON")
		}
		return encoder.WriteValue(value)
	default:
		return fmt.Errorf("criterion type %T is not supported", criterion)
	}
}

func writeStringMap(encoder *jsontext.Encoder, values map[string]string) error {
	if err := encoder.WriteToken(jsontext.BeginObject); err != nil {
		return err
	}
	for _, name := range slices.Sorted(maps.Keys(values)) {
		if err := writeNameToken(encoder, name, jsontext.String(values[name])); err != nil {
			return err
		}
	}
	return encoder.WriteToken(jsontext.EndObject)
}

func writeAnyMap(encoder *jsontext.Encoder, values map[string]any) error {
	if err := encoder.WriteToken(jsontext.BeginObject); err != nil {
		return err
	}
	for _, name := range slices.Sorted(maps.Keys(values)) {
		if err := encoder.WriteToken(jsontext.String(name)); err != nil {
			return err
		}
		if err := writeCriterion(encoder, values[name]); err != nil {
			return err
		}
	}
	return encoder.WriteToken(jsontext.EndObject)
}

func writeStrings(encoder *jsontext.Encoder, values []string) error {
	if err := encoder.WriteToken(jsontext.BeginArray); err != nil {
		return err
	}
	for _, value := range values {
		if err := encoder.WriteToken(jsontext.String(value)); err != nil {
			return err
		}
	}
	return encoder.WriteToken(jsontext.EndArray)
}

func writeAnySlice(encoder *jsontext.Encoder, values []any) error {
	if err := encoder.WriteToken(jsontext.BeginArray); err != nil {
		return err
	}
	for _, value := range values {
		if err := writeCriterion(encoder, value); err != nil {
			return err
		}
	}
	return encoder.WriteToken(jsontext.EndArray)
}

func writeNameToken(encoder *jsontext.Encoder, name string, value jsontext.Token) error {
	if err := encoder.WriteToken(jsontext.String(name)); err != nil {
		return err
	}
	return encoder.WriteToken(value)
}

func writeNameValue(encoder *jsontext.Encoder, name string, value jsontext.Value) error {
	if err := encoder.WriteToken(jsontext.String(name)); err != nil {
		return err
	}
	return encoder.WriteValue(value)
}

func readStringToken(decoder *jsontext.Decoder) (string, error) {
	token, err := decoder.ReadToken()
	if err != nil {
		return "", err
	}
	if token.Kind() != jsontext.KindString {
		return "", errors.New("expected a JSON string")
	}
	return token.String(), nil
}

func readNumberToken(decoder *jsontext.Decoder) (float64, error) {
	token, err := decoder.ReadToken()
	if err != nil {
		return 0, err
	}
	if token.Kind() != jsontext.KindNumber {
		return 0, errors.New("expected a JSON number")
	}
	value, err := token.Float()
	if err != nil {
		return 0, fmt.Errorf("decode JSON number: %w", err)
	}
	return value, nil
}
