package agent

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"strings"

	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/tool"
)

const taskName = "task"

type TaskRequest struct {
	Prompt  string
	Runner  string
	Model   string
	Advisor bool
}

type taskTool struct{ t *Thread }

func (taskTool) Spec() tool.Spec {
	return tool.Spec{
		Name: taskName,
		Description: "Start a subagent in a child thread to work on `prompt`. " +
			"Optional `runner` names a configured runner and `model` names a role or provider/model. " +
			"Set `advisor` to true to attach the advisor to the subagent. " +
			"Calls in one message run together. The result is JSON text: {\"data\": <value>} when the subagent succeeded, or {\"error\": \"<text>\"} when it failed.",
		Schema: jsontext.Value(`{"type":"object","properties":{"prompt":{"type":"string"},"runner":{"type":"string"},"model":{"type":"string"},"advisor":{"type":"boolean"}},"required":["prompt"],"additionalProperties":false}`),
	}
}

func (taskTool) Class() tool.Class { return tool.ClassAgent }

func (tt taskTool) Run(ctx context.Context, call tool.Call, _ tool.Env) (tool.Result, error) {
	req, err := parseTask(call.Args)
	if err != nil {
		return tool.Result{}, err
	}
	return tt.t.spawnChild(ctx, call.ID, req)
}

func parseTask(raw jsontext.Value) (TaskRequest, error) {
	var req TaskRequest
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
		return TaskRequest{}, err
	}
	if req.Prompt == "" {
		return TaskRequest{}, errors.New("task needs a prompt")
	}
	return req, nil
}

func taskDataText(data jsontext.Value) string { return `{"data":` + string(data) + `}` }

func taskErrorText(message string) string {
	quoted, _ := jsontext.AppendQuote(nil, strings.ToValidUTF8(message, "\uFFFD"))
	return `{"error":` + string(quoted) + `}`
}

var errYieldShape = errors.New("yield needs exactly one of data or error")

type yieldArgs struct {
	Data    jsontext.Value
	Error   string
	IsError bool
}

type yieldTool struct{}

func (yieldTool) Spec() tool.Spec {
	return tool.Spec{
		Name: tool.YieldName,
		Description: "Return your final result to the parent agent and end your turn. " +
			"Pass exactly one field: `data` (any JSON value) on success, or `error` (a string) on failure. " +
			"The parent receives {\"data\": <value>} or {\"error\": \"<text>\"}.",
		Schema: jsontext.Value(`{"type":"object","properties":{"data":{},"error":{"type":"string"}},"additionalProperties":false}`),
	}
}

func (yieldTool) Class() tool.Class { return tool.ClassRead }

func (yieldTool) Run(_ context.Context, call tool.Call, _ tool.Env) (tool.Result, error) {
	if _, err := parseYield(call.Args); err != nil {
		return tool.Result{}, err
	}
	return tool.Result{Output: []byte("yielded"), OK: true}, nil
}

func parseYield(raw jsontext.Value) (yieldArgs, error) {
	var out yieldArgs
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
		return yieldArgs{}, errYieldShape
	}
	out.IsError = hasError
	return out, nil
}
