package yield

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"

	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/tool"
)

const Name = "yield"

var ErrShape = errors.New("yield needs exactly one of data or error")

type Result struct {
	Data    jsontext.Value
	Error   string
	IsError bool
}

type Tool struct{}

func New() Tool { return Tool{} }

func (Tool) Spec() tool.Spec {
	return tool.Spec{
		Name: Name,
		Description: "Return your final result to the parent agent and end your turn. " +
			"Pass exactly one field: `data` (any JSON value) on success, or `error` (a string) on failure. " +
			"The parent receives {\"data\": <value>} or {\"error\": \"<text>\"}.",
		Schema: jsontext.Value(`{"type":"object","properties":{"data":{},"error":{"type":"string"}},"additionalProperties":false}`),
	}
}

func (Tool) Class() tool.Class { return tool.ClassRead }

func (Tool) Run(_ context.Context, call tool.Call, _ tool.Env) (tool.Result, error) {
	if _, err := Parse(call.Args); err != nil {
		return tool.Result{}, err
	}
	return tool.Result{Output: []byte("yielded"), OK: true}, nil
}

func Parse(raw jsontext.Value) (Result, error) {
	var out Result
	var hasData, hasError bool
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	err := model.ReadObjectFields(decoder, func(name string) error {
		var err error
		switch name {
		case "data":
			hasData = true
			out.Data, err = model.ReadValue(decoder)
		case "error":
			hasError = true
			out.Error, err = model.ReadString(decoder)
		default:
			err = decoder.SkipValue()
		}
		return err
	})
	if err != nil || hasData == hasError {
		return Result{}, ErrShape
	}
	out.IsError = hasError
	return out, nil
}
