package transcript

import (
	"bytes"
	"encoding/json/jsontext"
	"errors"
	"io"
)

const argumentIndent = "  "

const structuredSourceLimit = 8 << 20

func indentedJSONSize(raw []byte, limit int) (int, bool, error) {
	dec := jsontext.NewDecoder(bytes.NewBuffer(raw), jsontext.AllowDuplicateNames(true), jsontext.AllowInvalidUTF8(true))
	size, offset := 0, 0
	oversize := false
	for {
		depth := dec.StackDepth()
		container, count := dec.StackIndex(depth)
		token, err := dec.ReadToken()
		if errors.Is(err, io.EOF) {
			return indentedJSONResult(size, count, oversize)
		}
		if err != nil || depth == 0 && count != 0 {
			return len(raw), false, nil
		}
		end := int(dec.InputOffset()) //nolint:gosec // decoder reads only the resident raw slice.
		if !oversize {
			lexeme := bytes.TrimLeft(raw[offset:end], " \t\r\n,:")
			size, oversize = addJSONExtent(size, len(lexeme)+jsonTokenSpacing(depth, container, count, token.Kind()), limit)
		}
		offset = end
	}
}

func indentedJSONResult(size int, roots int64, oversize bool) (int, bool, error) {
	if roots != 1 {
		return 0, false, nil
	}
	if oversize {
		return 0, true, errSourceBudget
	}
	return size, true, nil
}

func addJSONExtent(size, next, limit int) (int, bool) {
	if next > limit-size {
		return size, true
	}
	return size + next, false
}

func jsonTokenSpacing(depth int, container jsontext.Kind, count int64, next jsontext.Kind) int {
	if depth == 0 {
		return 0
	}
	if next == '}' || next == ']' {
		if count == 0 {
			return 0
		}
		return 1 + (depth-1)*len(argumentIndent)
	}
	if container == '{' && count%2 != 0 {
		return len(": ")
	}
	indent := 1 + depth*len(argumentIndent)
	if count != 0 {
		indent++
	}
	return indent
}

func prettyJSON(text string, outputBytes int, l *loader) (string, error) {
	if err := l.reserveJSONInput(len(text), outputBytes); err != nil {
		return "", err
	}
	if text == "" {
		return "{}", nil
	}
	raw := []byte(text)
	size, valid, err := l.reserveJSONExtent(raw)
	if err != nil {
		return "", err
	}
	if !valid {
		return text, nil
	}
	capacity := size + 1
	out := bytes.NewBuffer(make([]byte, 0, capacity+capacity/4))
	enc := jsontext.NewEncoder(out, jsontext.AllowDuplicateNames(true), jsontext.AllowInvalidUTF8(true),
		jsontext.PreserveRawStrings(true), jsontext.WithIndent(argumentIndent))
	if err := enc.WriteValue(jsontext.Value(raw)); err != nil {
		return text, nil
	}
	return string(bytes.TrimSuffix(out.Bytes(), []byte("\n"))), nil
}

func (l *loader) reserveJSONInput(textBytes, outputBytes int) error {
	if textBytes > structuredSourceLimit {
		return errSourceBudget
	}
	if textBytes > residentBudget || outputBytes > residentBudget {
		return errSourceBudget
	}
	const pageRounding = 5 * (8 << 10)
	fixed := 2*textBytes + 2*(outputBytes+1) + pageRounding
	if !l.holdProjection(fixed) {
		return errSourceBudget
	}
	return nil
}

func (l *loader) reserveJSONExtent(raw []byte) (int, bool, error) {
	const formattingBackings = 5
	limit := l.projectionRoom() / formattingBackings
	size, valid, err := indentedJSONSize(raw, limit)
	if err != nil {
		return 0, false, err
	}
	if !valid {
		size = len(raw)
	}
	if size > limit || !l.holdProjection(formattingBackings*size) {
		return 0, false, errSourceBudget
	}
	return size, valid, nil
}
