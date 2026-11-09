package task

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"strings"

	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/tool"
)

const Name = "task"

type Request struct {
	Prompt  string
	Runner  string
	Model   string
	Advisor bool
}

type Spawner interface {
	Spawn(ctx context.Context, callID string, req Request) (tool.Result, error)
}

type Tool struct{ spawner Spawner }

func New(spawner Spawner) Tool { return Tool{spawner: spawner} }

func (Tool) Spec() tool.Spec {
	return tool.Spec{
		Name: Name,
		Description: "Start a subagent in a child thread to work on `prompt`. " +
			"Optional `runner` names a configured runner and `model` names a role or provider/model. " +
			"Set `advisor` to true to attach the advisor to the subagent. " +
			"Calls in one message run together. The result is JSON text: {\"data\": <value>} when the subagent succeeded, or {\"error\": \"<text>\"} when it failed.",
		Schema: jsontext.Value(`{"type":"object","properties":{"prompt":{"type":"string"},"runner":{"type":"string"},"model":{"type":"string"},"advisor":{"type":"boolean"}},"required":["prompt"],"additionalProperties":false}`),
	}
}

func (Tool) Class() tool.Class { return tool.ClassAgent }

func (t Tool) Run(ctx context.Context, call tool.Call, _ tool.Env) (tool.Result, error) {
	req, err := Parse(call.Args)
	if err != nil {
		return tool.Result{}, err
	}
	return t.spawner.Spawn(ctx, call.ID, req)
}

func Parse(raw jsontext.Value) (Request, error) {
	var req Request
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	err := model.ReadObjectFields(decoder, func(name string) error {
		var err error
		switch name {
		case "prompt":
			req.Prompt, err = model.ReadString(decoder)
		case "runner":
			req.Runner, err = model.ReadString(decoder)
		case "model":
			req.Model, err = model.ReadString(decoder)
		case "advisor":
			req.Advisor, err = model.ReadBool(decoder)
		default:
			err = decoder.SkipValue()
		}
		return err
	})
	if err != nil {
		return Request{}, err
	}
	if req.Prompt == "" {
		return Request{}, errors.New("task needs a prompt")
	}
	return req, nil
}

func DataText(data jsontext.Value) string { return `{"data":` + string(data) + `}` }

func ErrorText(message string) string {
	quoted, _ := jsontext.AppendQuote(nil, strings.ToValidUTF8(message, "\uFFFD"))
	return `{"error":` + string(quoted) + `}`
}
