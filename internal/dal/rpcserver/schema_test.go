package rpcserver

import (
	json "encoding/json/v2"
	"path/filepath"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/santhosh-tekuri/jsonschema/v6"
)

func validateCodexSchema(t *testing.T, path string, input any) {
	t.Helper()
	compiler := jsonschema.NewCompiler()
	sch, err := compiler.Compile(path)
	if err != nil {
		t.Fatal(err)
	}
	body, err := json.Marshal(input)
	if err != nil {
		t.Fatal(err)
	}
	var value any
	if err := json.Unmarshal(body, &value); err != nil {
		t.Fatal(err)
	}
	if err := sch.Validate(value); err != nil {
		t.Fatal(err)
	}
}

func TestCodexSchemasValidateUserInput(t *testing.T) {
	params := rpc.ToolRequestUserInputParams{
		ThreadID: "thread", TurnID: "turn", ItemID: "call", IsBlocking: true,
		Questions: []rpc.ToolRequestUserInputQuestion{
			{
				ID: "q1", Header: "Database", Question: "Which?", IsOther: true, MultiSelect: true,
				Options: []rpc.ToolRequestUserInputOption{{Label: "Postgres", Description: "A server.", Preview: "image: postgres"}},
			},
			{ID: "q2", Header: "Name", Question: "What?"},
		},
	}
	encoded, err := rpc.Marshal(params)
	if err != nil {
		t.Fatal(err)
	}
	root := filepath.Join("..", "rpc", "testdata", "codex-schema")
	validateCodexSchema(t, filepath.Join(root, "ToolRequestUserInputParams.json"), encoded)
	response := rpc.ToolRequestUserInputResponse{Answers: map[string]rpc.ToolRequestUserInputAnswer{"q1": {Answers: []string{"Postgres"}}}}
	validateCodexSchema(t, filepath.Join(root, "ToolRequestUserInputResponse.json"), response)
}
