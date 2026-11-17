package failure

import (
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"strings"
	"testing"
)

func convertInput(tool string, args string) ConvertInput {
	return ConvertInput{
		Tool:      tool,
		OrigArgs:  jsontext.Value(args),
		Workspace: testWorkspace(),
		Completed: true,
	}
}

func TestFailureStepContainsOnlyTargetCall(t *testing.T) {
	step := buildTargetStep(t)
	assertReadyStep(t, step)
	assertTargetCallArgs(t, step)
	assertTargetScriptShape(t, step)
}

func buildTargetStep(t *testing.T) FailureStep {
	t.Helper()
	step, err := BuildFailureStep(context.Background(), mapMasker{}, convertInput("exec", `{"command":"ls -la","cwd":"sub"}`))
	if err != nil {
		t.Fatal(err)
	}
	return step
}

func assertReadyStep(t *testing.T, step FailureStep) {
	t.Helper()
	if step.Status != StatusReady {
		t.Fatalf("status = %s", step.Status)
	}
	if err := ValidateFailureStep(step); err != nil {
		t.Fatalf("validate: %v", err)
	}
	if step.Call.Name != "exec" || step.Call.ID != "failure-call" {
		t.Fatalf("call = %+v", step.Call)
	}
	encoded, _ := json.Marshal(step)
	if strings.Contains(string(encoded), `"id":"call-1"`) {
		t.Fatalf("original call id leaked in %s", encoded)
	}
}

func assertTargetCallArgs(t *testing.T, step FailureStep) {
	t.Helper()
	var args map[string]any
	if err := json.Unmarshal([]byte(step.Call.Arguments), &args); err != nil {
		t.Fatal(err)
	}
	if len(args) != 2 || args["command"] != "ls -la" || args["cwd"] != "sub" {
		t.Fatalf("args = %v", args)
	}
}

func assertTargetScriptShape(t *testing.T, step FailureStep) {
	t.Helper()
	if len(step.Steps) != 2 || step.Steps[0].ToolCall == nil || step.Steps[1].Finish == nil {
		t.Fatalf("steps = %+v", step.Steps)
	}
}

func TestFailureStepUsesOriginalArguments(t *testing.T) {
	orig := `{"command":"ls -la","timeout_ms":30}`
	step, err := BuildFailureStep(context.Background(), mapMasker{}, convertInput("exec", orig))
	if err != nil {
		t.Fatal(err)
	}
	var emitted map[string]any
	if err := json.Unmarshal([]byte(step.Steps[0].ToolCall.Arguments), &emitted); err != nil {
		t.Fatal(err)
	}
	var original map[string]any
	if err := json.Unmarshal([]byte(orig), &original); err != nil {
		t.Fatal(err)
	}
	if emitted["command"] != original["command"] || emitted["timeout_ms"] != original["timeout_ms"] {
		t.Fatalf("emitted = %v, original = %v", emitted, original)
	}
}

func TestFailureStepUnavailableClasses(t *testing.T) {
	workspace := testWorkspace()
	big := `{"command":"ls","justification":"` + strings.Repeat("a", 9000) + `"}`
	cases := []unavailableCase{
		{"no completion", ConvertInput{Tool: "exec", OrigArgs: jsontext.Value(`{"command":"ls"}`), Workspace: workspace}, ReasonMissingCompletion},
		{"panic no call", ConvertInput{Tool: "exec", OrigArgs: jsontext.Value(`{"command":"ls"}`), Workspace: workspace, Panic: true}, ReasonPanicNoCall},
		{"freeform", ConvertInput{Tool: "exec", Freeform: true, OrigArgs: jsontext.Value(`{"command":"ls"}`), Workspace: workspace, Completed: true}, ReasonFreeform},
		{"non-object", ConvertInput{Tool: "exec", OrigArgs: jsontext.Value(`[1,2]`), Workspace: workspace, Completed: true}, ReasonNonObject},
		{"masking failed", ConvertInput{Tool: "exec", OrigArgs: jsontext.Value(`{"command":"ls"}`), Workspace: workspace, Completed: true}, ReasonMaskingFailed},
		{"unknown tool", ConvertInput{Tool: "mystery", OrigArgs: jsontext.Value(`{"x":1}`), Workspace: workspace, Completed: true}, ReasonPrivateContent},
		{"inline content", ConvertInput{Tool: "patch", OrigArgs: jsontext.Value(`{"path":"f.txt","text":"hello"}`), Workspace: workspace, Completed: true}, ReasonInlineContent},
		{"unsafe shell", ConvertInput{Tool: "exec", OrigArgs: jsontext.Value(`{"command":"sh -c 'rm -rf x'"}`), Workspace: workspace, Completed: true}, ReasonPrivateContent},
		{"env assignment", ConvertInput{Tool: "exec", OrigArgs: jsontext.Value(`{"command":"FOO=1 ls"}`), Workspace: workspace, Completed: true}, ReasonPrivateContent},
		{"absolute cwd", ConvertInput{Tool: "exec", OrigArgs: jsontext.Value(`{"command":"ls","cwd":"/etc"}`), Workspace: workspace, Completed: true}, ReasonPreimage},
		{"over bound", ConvertInput{Tool: "exec", OrigArgs: jsontext.Value(big), Workspace: workspace, Completed: true}, ReasonOverBound},
		{"placeholder args", ConvertInput{Tool: "exec", OrigArgs: jsontext.Value(`{"command":"ls s3cr3t"}`), Workspace: workspace, Completed: true}, ReasonMaskedNotExact},
		{"provider failure", ConvertInput{Tool: "exec", OrigArgs: jsontext.Value(`{"command":"ls"}`), Workspace: workspace, Unavailable: ReasonProvider}, ReasonProvider},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			assertUnavailableCase(t, tc)
		})
	}
}

type unavailableCase struct {
	name   string
	input  ConvertInput
	reason string
}

func maskerForUnavailableCase(name string) mapMasker {
	if name == "placeholder args" {
		return mapMasker{replacements: map[string]string{"s3cr3t": "{{secret:0:0}}"}}
	}
	if name == "masking failed" {
		return mapMasker{fail: true}
	}
	return mapMasker{}
}

func assertUnavailableCase(t *testing.T, tc unavailableCase) {
	t.Helper()
	step, err := BuildFailureStep(context.Background(), maskerForUnavailableCase(tc.name), tc.input)
	if err != nil {
		t.Fatal(err)
	}
	if step.Status != StatusUnavailable || step.Reason != tc.reason {
		t.Fatalf("step = %+v", step)
	}
	if step.Call != nil || len(step.Steps) != 0 || len(step.Files) != 0 {
		t.Fatalf("unavailable step carries content: %+v", step)
	}
	if verr := ValidateFailureStep(step); verr != nil {
		t.Fatalf("validate: %v", verr)
	}
}

func TestFailureFileMetadataHasNoContent(t *testing.T) {
	files := []FileRequirement{
		{Path: "src/main.go", Exists: true, Bytes: 120, SHA256: strings.Repeat("a", 64)},
		{Path: "missing.txt", Exists: false},
	}
	in := convertInput("read", `{"path":"src/main.go"}`)
	in.Files = files
	step, err := BuildFailureStep(context.Background(), mapMasker{}, in)
	if err != nil {
		t.Fatal(err)
	}
	if step.Status != StatusReady {
		t.Fatalf("step = %+v", step)
	}
	encoded, _ := json.Marshal(step)
	text := string(encoded)
	for _, forbidden := range []string{"contents", "blob", "bytes\":\"", "pre_sha", "post_sha"} {
		if strings.Contains(text, forbidden) {
			t.Fatalf("file content marker %q in %s", forbidden, text)
		}
	}
	if len(step.Files) != 2 || step.Files[0].SHA256 != strings.Repeat("a", 64) {
		t.Fatalf("files = %+v", step.Files)
	}
	tooMany := make([]FileRequirement, 33)
	in.Files = tooMany
	step, err = BuildFailureStep(context.Background(), mapMasker{}, in)
	if err != nil {
		t.Fatal(err)
	}
	if step.Reason != ReasonOverBound {
		t.Fatalf("step = %+v", step)
	}
}

func TestValidateFailureStepRejects(t *testing.T) {
	good, err := BuildFailureStep(context.Background(), mapMasker{}, convertInput("exec", `{"command":"ls"}`))
	if err != nil {
		t.Fatal(err)
	}
	if err := ValidateFailureStep(good); err != nil {
		t.Fatalf("good step rejected: %v", err)
	}
	bad := good
	bad.Status = "maybe"
	if err := ValidateFailureStep(bad); err == nil {
		t.Fatal("bad status accepted")
	}
	bad = good
	bad.Reason = "x"
	if err := ValidateFailureStep(bad); err == nil {
		t.Fatal("ready step with reason accepted")
	}
	bad = good
	bad.Call = nil
	if err := ValidateFailureStep(bad); err == nil {
		t.Fatal("ready step without call accepted")
	}
	bad = good
	bad.Steps = bad.Steps[:1]
	if err := ValidateFailureStep(bad); err == nil {
		t.Fatal("single step accepted")
	}
	envelope := UnavailableStep("not an exact reason")
	if err := ValidateFailureStep(envelope); err == nil {
		t.Fatal("inexact reason accepted")
	}
	envelope = UnavailableStep(ReasonNetwork)
	envelope.Call = good.Call
	if err := ValidateFailureStep(envelope); err == nil {
		t.Fatal("unavailable step with call accepted")
	}
}
