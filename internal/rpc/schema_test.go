package rpc

import (
	json "encoding/json/v2"
	"path/filepath"
	"testing"

	"github.com/santhosh-tekuri/jsonschema/v6"
)

func TestCodexSchemasCompileAndValidate(t *testing.T) {
	cases := []struct {
		name  string
		path  string
		value any
	}{
		{"InitializeParams", "v1/InitializeParams.json", InitializeParams{ClientInfo: ClientInfo{Name: "dalgona-test", Version: "0"}}},
		{"ThreadStartParams", "v2/ThreadStartParams.json", ThreadStartParams{Cwd: "/tmp"}},
		{"ThreadResumeParams", "v2/ThreadResumeParams.json", ThreadResumeParams{ThreadID: "thread"}},
		{"ThreadListParams", "v2/ThreadListParams.json", ThreadListParams{}},
		{"ThreadReadParams", "v2/ThreadReadParams.json", ThreadReadParams{ThreadID: "thread"}},
		{"ThreadNameSetParams", "v2/ThreadSetNameParams.json", ThreadNameSetParams{ThreadID: "thread", Name: "name"}},
		{"ThreadRevertParams", "v2/ThreadRevertParams.json", ThreadRevertParams{ThreadID: "thread", BeforeTurnID: "turn"}},
		{"ThreadForkParams", "v2/ThreadForkParams.json", ThreadForkParams{ThreadID: "thread"}},
		{"TurnInterruptParams", "v2/TurnInterruptParams.json", TurnInterruptParams{ThreadID: "thread", TurnID: "turn"}},
		{"ModelListParams", "v2/ModelListParams.json", ModelListParams{}},
		{"ConfigReadParams", "v2/ConfigReadParams.json", ConfigReadParams{}},
		{"ConfigValueWriteParams", "v2/ConfigValueWriteParams.json", ConfigValueWriteParams{KeyPath: "model", MergeStrategy: "replace", Value: "openai/gpt"}},
		{"SkillsListParams", "v2/SkillsListParams.json", SkillsListParams{}},
		{"CommandExecutionRequestApprovalParams", "CommandExecutionRequestApprovalParams.json", CommandExecutionRequestApprovalParams{ThreadID: "thread", TurnID: "turn", ItemID: "item", StartedAtMs: 1, Command: "echo hi"}},
		{"FileChangeRequestApprovalParams", "FileChangeRequestApprovalParams.json", FileChangeRequestApprovalParams{ThreadID: "thread", TurnID: "turn", ItemID: "item", StartedAtMs: 1}},
	}
	root := filepath.Join("testdata", "codex-schema")
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			validateCodexSchema(t, filepath.Join(root, tc.path), tc.value)
		})
	}
}

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
