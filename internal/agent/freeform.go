package agent

import (
	"encoding/json/jsontext"
	"fmt"

	json "encoding/json/v2"

	"github.com/gosuda/dalgona/internal/model"
)

func eventArgs(call model.ToolCall) (jsontext.Value, error) {
	if !call.Freeform {
		return call.Args, nil
	}
	quoted, err := jsontext.AppendQuote(nil, call.Args)
	if err != nil {
		return nil, fmt.Errorf("agent: freeform input for %s: %w", call.Name, err)
	}
	return jsontext.Value(quoted), nil
}

func freeformInput(args jsontext.Value) (jsontext.Value, error) {
	var input string
	if err := json.Unmarshal(args, &input); err != nil {
		return nil, fmt.Errorf("agent: freeform args must be a JSON string: %w", err)
	}
	return jsontext.Value(input), nil
}
