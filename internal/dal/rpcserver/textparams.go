package rpcserver

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"strconv"
	"strings"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/textinput"
)

const maxKeywordText = 64 << 10

type textLocation struct {
	path   string
	policy textinput.Policy
}

type textExtractor func(params jsontext.Value) ([]textinput.Item, error)

var (
	directTextPolicy  = textinput.Policy{Lines: textinput.MultiLine, Limit: textinput.Limit{Unit: textinput.Bytes, Max: frameBytes}}
	apiKeyTextPolicy  = textinput.Policy{Lines: textinput.SingleLine, Limit: textinput.Limit{Unit: textinput.Bytes, Max: frameBytes}}
	keywordTextPolicy = textinput.Policy{Lines: textinput.MultiLine, Limit: textinput.Limit{Unit: textinput.Bytes, Max: maxKeywordText}}
)

var fsTexts = directTexts("path", "sourcePath", "destinationPath", "dataBase64")

var methodTexts = map[string]textExtractor{
	"thread/start":               directTexts("cwd", "model", "modelProvider"),
	"thread/name/set":            directTexts("name"),
	"turn/start":                 turnTexts,
	"turn/steer":                 turnTexts,
	"review/start":               directTexts("target.title", "target.instructions"),
	"config/value/write":         directTexts("value"),
	"config/batchWrite":          directTexts("edits[].value"),
	"account/login/start":        locatedTexts(textLocation{path: "apiKey", policy: apiKeyTextPolicy}),
	"dalgona/keywords/match":     locatedTexts(textLocation{path: "text", policy: keywordTextPolicy}),
	"dalgona/script/run":         directTexts("entry", "source", "cwd", "args"),
	"dalgona/daltoki/draft/edit": directTexts("title", "body"),
	methodWorkspaceExec:          locatedTexts(textLocation{path: "text", policy: workspaceExecPolicy}),
	"fs/readFile":                fsTexts,
	"fs/writeFile":               fsTexts,
	"fs/createDirectory":         fsTexts,
	"fs/getMetadata":             fsTexts,
	"fs/readDirectory":           fsTexts,
	"fs/remove":                  fsTexts,
	"fs/copy":                    fsTexts,
	"fs/watch":                   fsTexts,
}

func directTexts(paths ...string) textExtractor {
	locations := make([]textLocation, len(paths))
	for i, path := range paths {
		locations[i] = textLocation{path: path, policy: directTextPolicy}
	}
	return locatedTexts(locations...)
}

func locatedTexts(locations ...textLocation) textExtractor {
	return func(params jsontext.Value) ([]textinput.Item, error) {
		walk := textWalk{locations: locations}
		if err := walk.value(jsontext.NewDecoder(bytes.NewReader(params)), "", ""); err != nil {
			return nil, invalidParams(err.Error())
		}
		return walk.items, nil
	}
}

func (s *server) invoke(ctx context.Context, name string, call method, params jsontext.Value) (jsontext.Value, error) {
	if err := checkText(methodTexts[name], params); err != nil {
		return nil, err
	}
	return call(s, ctx, params)
}

func checkText(extract textExtractor, params jsontext.Value) error {
	if extract == nil || len(params) == 0 {
		return nil
	}
	items, err := extract(params)
	if err != nil {
		return err
	}
	if err := textinput.CheckRequest(items); err != nil {
		return invalidParams(err.Error())
	}
	return nil
}

type textWalk struct {
	locations []textLocation
	items     []textinput.Item
}

func (w *textWalk) value(d *jsontext.Decoder, pattern, field string) error {
	if policy, ok := w.declared(pattern); ok {
		return w.collect(d, policy, field)
	}
	switch d.PeekKind() {
	case jsontext.KindBeginObject:
		return rpc.ReadMembers(d, func(name string, d *jsontext.Decoder) error {
			next := memberPath(pattern, name)
			if !w.leadsTo(next) {
				return d.SkipValue()
			}
			return w.value(d, next, memberPath(field, name))
		})
	case jsontext.KindBeginArray:
		return w.elements(d, pattern+"[]", field)
	case jsontext.KindInvalid, jsontext.KindNull, jsontext.KindFalse, jsontext.KindTrue, jsontext.KindString, jsontext.KindNumber, jsontext.KindEndObject, jsontext.KindEndArray:
		return d.SkipValue()
	}
	return d.SkipValue()
}

func (w *textWalk) elements(d *jsontext.Decoder, pattern, field string) error {
	if !w.leadsTo(pattern) {
		return d.SkipValue()
	}
	index := 0
	return rpc.ReadArray(d, func() error {
		err := w.value(d, pattern, field+"["+strconv.Itoa(index)+"]")
		index++
		return err
	})
}

func (w *textWalk) collect(d *jsontext.Decoder, policy textinput.Policy, field string) error {
	for depth := 0; ; {
		token, err := d.ReadToken()
		if err != nil {
			return err
		}
		switch token.Kind() {
		case jsontext.KindString:
			w.items = append(w.items, textinput.Item{Field: field, Policy: policy, Text: token.String()})
		case jsontext.KindBeginObject, jsontext.KindBeginArray:
			depth++
		case jsontext.KindEndObject, jsontext.KindEndArray:
			depth--
		case jsontext.KindInvalid, jsontext.KindNull, jsontext.KindFalse, jsontext.KindTrue, jsontext.KindNumber:
		}
		if depth == 0 {
			return nil
		}
	}
}

func (w *textWalk) declared(pattern string) (textinput.Policy, bool) {
	for _, location := range w.locations {
		if location.path == pattern {
			return location.policy, true
		}
	}
	return textinput.Policy{}, false
}

func (w *textWalk) leadsTo(pattern string) bool {
	for _, location := range w.locations {
		rest, ok := strings.CutPrefix(location.path, pattern)
		if ok && (rest == "" || rest[0] == '.' || rest[0] == '[') {
			return true
		}
	}
	return false
}

func memberPath(parent, name string) string {
	if parent == "" {
		return name
	}
	return parent + "." + name
}
