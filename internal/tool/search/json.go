package search

import (
	"bytes"
	"encoding/base64"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"slices"
	"strconv"

	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

type args struct {
	Mode       string
	Pattern    string
	Path       string
	Glob       string
	Literal    bool
	IgnoreCase bool
	Kind       []string
	Limit      int
}

type capture struct {
	Path  string
	Data  []byte
	Shown []snapshot.Range
}

type outcome struct {
	Text      string
	Captures  []capture
	Unindexed int
}

func eachField(raw jsontext.Value, visit func(key string, value jsontext.Value) error) error {
	decoder := jsontext.NewDecoder(bytes.NewBuffer(raw))
	start, err := decoder.ReadToken()
	if err != nil || start.Kind() != '{' {
		return errors.New("must be an object")
	}
	for decoder.PeekKind() != '}' {
		name, nameErr := decoder.ReadToken()
		if nameErr != nil {
			return nameErr
		}
		key := name.String()
		value, valueErr := decoder.ReadValue()
		if valueErr != nil {
			return valueErr
		}
		if visitErr := visit(key, value); visitErr != nil {
			return visitErr
		}
	}
	return closeValue(decoder)
}

func eachItem(raw jsontext.Value, visit func(value jsontext.Value) error) error {
	decoder := jsontext.NewDecoder(bytes.NewBuffer(raw))
	start, err := decoder.ReadToken()
	if err != nil || start.Kind() != '[' {
		return errors.New("must be an array")
	}
	for decoder.PeekKind() != ']' {
		value, valueErr := decoder.ReadValue()
		if valueErr != nil {
			return valueErr
		}
		if visitErr := visit(value); visitErr != nil {
			return visitErr
		}
	}
	return closeValue(decoder)
}

func closeValue(decoder *jsontext.Decoder) error {
	if _, err := decoder.ReadToken(); err != nil {
		return err
	}
	if _, err := decoder.ReadToken(); !errors.Is(err, io.EOF) {
		return errors.New("trailing data")
	}
	return nil
}

func parseArgs(raw jsontext.Value) (args, error) {
	out := args{Path: ".", Limit: 200}
	if err := eachField(raw, func(key string, value jsontext.Value) error {
		return setArg(&out, key, value)
	}); err != nil {
		return args{}, fmt.Errorf("search: invalid arguments: %w", err)
	}
	return out, validateArgs(out)
}

func validateArgs(out args) error {
	if err := validateMode(out); err != nil {
		return err
	}
	if out.Limit < 1 || out.Limit > 1000 {
		return errors.New("search: limit must be between 1 and 1000")
	}
	return validateKinds(out)
}

func validateMode(out args) error {
	if out.Mode != "find" && out.Mode != "grep" && out.Mode != "symbols" {
		return errors.New("search: mode must be find, grep, or symbols")
	}
	if out.Pattern == "" && out.Mode != "symbols" {
		return errors.New("search: pattern must not be empty")
	}
	return nil
}

func validateKinds(out args) error {
	for _, kind := range out.Kind {
		if !slices.Contains(symbolKinds, kind) {
			return fmt.Errorf("search: unknown kind %s", kind)
		}
	}
	if len(out.Kind) > 0 && out.Mode != "symbols" {
		return errors.New("search: kind applies only to symbols")
	}
	return nil
}

func setArg(out *args, name string, value jsontext.Value) error {
	var err error
	switch name {
	case "mode", "pattern", "path", "glob":
		err = setStringArg(out, name, value)
	case "literal":
		out.Literal, err = boolArg(value)
	case "ignore_case":
		out.IgnoreCase, err = boolArg(value)
	case "kind":
		out.Kind, err = stringsArg(value)
	case "limit":
		out.Limit, err = intArg(value)
	default:
		return fmt.Errorf("unknown argument %q", name)
	}
	if err != nil {
		return fmt.Errorf("%s: %w", name, err)
	}
	return nil
}

func setStringArg(out *args, name string, value jsontext.Value) error {
	text, err := stringArg(value)
	if err != nil {
		return err
	}
	switch name {
	case "mode":
		out.Mode = text
	case "pattern":
		out.Pattern = text
	case "path":
		out.Path = text
	default:
		out.Glob = text
	}
	return nil
}

func soleToken(value jsontext.Value) (string, jsontext.Kind, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(value))
	token, err := decoder.ReadToken()
	if err != nil {
		return "", 0, err
	}
	return token.String(), token.Kind(), nil
}

func stringsArg(value jsontext.Value) ([]string, error) {
	var out []string
	err := eachItem(value, func(item jsontext.Value) error {
		text, err := stringArg(item)
		if err != nil {
			return err
		}
		out = append(out, text)
		return nil
	})
	return out, err
}

func stringArg(value jsontext.Value) (string, error) {
	text, kind, err := soleToken(value)
	if err != nil {
		return "", err
	}
	if kind != '"' {
		return "", errors.New("must be a string")
	}
	return text, nil
}

func boolArg(value jsontext.Value) (bool, error) {
	text, kind, err := soleToken(value)
	if err != nil {
		return false, err
	}
	if kind != 't' && kind != 'f' {
		return false, errors.New("must be a boolean")
	}
	return text == "true", nil
}

func intArg(value jsontext.Value) (int, error) {
	text, kind, err := soleToken(value)
	if err != nil {
		return 0, err
	}
	if kind != '0' {
		return 0, errors.New("must be an integer")
	}
	number, err := strconv.Atoi(text)
	if err != nil {
		return 0, fmt.Errorf("must be an integer: %w", err)
	}
	return number, nil
}

func encodeOutcome(result outcome) []byte {
	out := []byte(`{"text":`)
	out = snapshot.AppendJSONString(out, result.Text)
	out = append(out, `,"captures":[`...)
	for i, item := range result.Captures {
		if i > 0 {
			out = append(out, ',')
		}
		out = append(out, `{"path":`...)
		out = snapshot.AppendJSONString(out, item.Path)
		out = append(out, `,"data":"`...)
		out = base64.StdEncoding.AppendEncode(out, item.Data)
		out = append(out, `","shown":[`...)
		for j, r := range item.Shown {
			if j > 0 {
				out = append(out, ',')
			}
			out = append(out, '[')
			out = strconv.AppendInt(out, int64(r.Start), 10)
			out = append(out, ',')
			out = strconv.AppendInt(out, int64(r.End), 10)
			out = append(out, ']')
		}
		out = append(out, `]}`...)
	}
	out = append(out, ']')
	out = append(out, `,"unindexed":`...)
	out = strconv.AppendInt(out, int64(result.Unindexed), 10)
	out = append(out, '}')
	return out
}

func decodeOutcome(data []byte) (outcome, error) {
	var result outcome
	err := eachField(data, func(key string, value jsontext.Value) error {
		var fieldErr error
		switch key {
		case "text":
			result.Text, fieldErr = stringArg(value)
		case "captures":
			result.Captures, fieldErr = decodeCaptures(value)
		case "unindexed":
			result.Unindexed, fieldErr = intArg(value)
		default:
			fieldErr = fmt.Errorf("unknown field %q", key)
		}
		return fieldErr
	})
	if err != nil {
		return outcome{}, fmt.Errorf("search: invalid subprocess response: %w", err)
	}
	return result, nil
}

func decodeCaptures(value jsontext.Value) ([]capture, error) {
	var out []capture
	err := eachItem(value, func(item jsontext.Value) error {
		decoded, captureErr := decodeCapture(item)
		out = append(out, decoded)
		return captureErr
	})
	return out, err
}

func decodeCapture(value jsontext.Value) (capture, error) {
	var out capture
	err := eachField(value, func(key string, field jsontext.Value) error {
		var fieldErr error
		switch key {
		case "path":
			out.Path, fieldErr = stringArg(field)
		case "data":
			out.Data, fieldErr = base64Arg(field)
		case "shown":
			out.Shown, fieldErr = decodeRanges(field)
		default:
			fieldErr = fmt.Errorf("unknown capture field %q", key)
		}
		return fieldErr
	})
	return out, err
}

func base64Arg(value jsontext.Value) ([]byte, error) {
	text, err := stringArg(value)
	if err != nil {
		return nil, err
	}
	return base64.StdEncoding.DecodeString(text)
}

func decodeRanges(value jsontext.Value) ([]snapshot.Range, error) {
	var out []snapshot.Range
	err := eachItem(value, func(item jsontext.Value) error {
		var pair []int
		pairErr := eachItem(item, func(number jsontext.Value) error {
			parsed, numberErr := intArg(number)
			pair = append(pair, parsed)
			return numberErr
		})
		if pairErr != nil || len(pair) != 2 {
			return errors.New("shown range must have two integers")
		}
		out = append(out, snapshot.Range{Start: pair[0], End: pair[1]})
		return nil
	})
	return out, err
}
