package rpc

import (
	json "encoding/json/v2"
	"maps"
	"path/filepath"
	"slices"
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
		{"ThreadResumeParamsExcludeTurns", "v2/ThreadResumeParams.json", ThreadResumeParams{ThreadID: "thread", ExcludeTurns: true}},
		{"ThreadTurnsListParams", "v2/ThreadTurnsListParams.json", ThreadTurnsListParams{ThreadID: "thread", Cursor: "a7", Limit: 3, SortDirection: "asc", ItemsView: "notLoaded"}},
		{"ThreadTurnsListParamsDefaults", "v2/ThreadTurnsListParams.json", ThreadTurnsListParams{ThreadID: "thread"}},
		{"ThreadListParams", "v2/ThreadListParams.json", ThreadListParams{}},
		{"ThreadReadParams", "v2/ThreadReadParams.json", ThreadReadParams{ThreadID: "thread"}},
		{"ThreadNameSetParams", "v2/ThreadSetNameParams.json", ThreadNameSetParams{ThreadID: "thread", Name: "name"}},
		{"ThreadRevertParams", "v2/ThreadRevertParams.json", ThreadRevertParams{ThreadID: "thread", BeforeTurnID: "turn"}},
		{"ThreadForkParams", "v2/ThreadForkParams.json", ThreadForkParams{ThreadID: "thread"}},
		{"TurnInterruptParams", "v2/TurnInterruptParams.json", TurnInterruptParams{ThreadID: "thread", TurnID: "turn"}},
		{"ReviewStartParams", "v2/ReviewStartParams.json", ReviewStartParams{ThreadID: "thread", Target: ReviewTarget{Type: "custom", Instructions: "check"}}},
		{"ModelListParams", "v2/ModelListParams.json", ModelListParams{}},
		{"ConfigReadParams", "v2/ConfigReadParams.json", ConfigReadParams{}},
		{"ConfigValueWriteParams", "v2/ConfigValueWriteParams.json", ConfigValueWriteParams{KeyPath: "model", MergeStrategy: "replace", Value: "openai/gpt"}},
		{"ConfigBatchWriteParams", "v2/ConfigBatchWriteParams.json", map[string]any{"edits": []map[string]any{{"keyPath": "verbosity", "mergeStrategy": "replace", "value": "low"}}}},
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

func TestTurnsParamsUseSchemaKeys(t *testing.T) {
	cases := []struct {
		name  string
		value any
		keys  []string
	}{
		{"resume", ThreadResumeParams{ThreadID: "t", ExcludeTurns: true}, []string{"excludeTurns", "threadId"}},
		{"resume omits false", ThreadResumeParams{ThreadID: "t"}, []string{"threadId"}},
		{"list", ThreadTurnsListParams{ThreadID: "t", Cursor: "a1", Limit: 2, SortDirection: "asc", ItemsView: "full"}, []string{"cursor", "itemsView", "limit", "sortDirection", "threadId"}},
		{"list omits empty", ThreadTurnsListParams{ThreadID: "t"}, []string{"threadId"}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			body, err := json.Marshal(tc.value)
			if err != nil {
				t.Fatal(err)
			}
			var members map[string]any
			if err := json.Unmarshal(body, &members); err != nil {
				t.Fatal(err)
			}
			got := slices.Sorted(maps.Keys(members))
			if !slices.Equal(got, tc.keys) {
				t.Fatalf("%s marshals the keys %v, want %v", body, got, tc.keys)
			}
		})
	}
}

func TestTurnsListSchemaRejectsWhatTheServerRejects(t *testing.T) {
	sch, err := jsonschema.NewCompiler().Compile(filepath.Join("testdata", "codex-schema", "v2", "ThreadTurnsListParams.json"))
	if err != nil {
		t.Fatal(err)
	}
	valid := map[string]any{"threadId": "t", "cursor": "a1", "limit": 3, "sortDirection": "desc", "itemsView": "full"}
	bad := map[string]map[string]any{
		"negative limit": {"limit": -1},
		"sort direction": {"sortDirection": "up"},
		"items view":     {"itemsView": "detailed"},
		"numeric cursor": {"cursor": 7},
		"fractional":     {"limit": 2.5},
	}
	check := func(members map[string]any) error {
		body, err := json.Marshal(members)
		if err != nil {
			t.Fatal(err)
		}
		var value any
		if err := json.Unmarshal(body, &value); err != nil {
			t.Fatal(err)
		}
		return sch.Validate(value)
	}
	if err := check(valid); err != nil {
		t.Fatalf("valid params rejected: %v", err)
	}
	for name, override := range bad {
		members := maps.Clone(valid)
		maps.Copy(members, override)
		if check(members) == nil {
			t.Fatalf("%s: the schema accepted %v", name, members)
		}
	}
}
