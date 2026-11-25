//go:build gui

package decision

import (
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"slices"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const (
	methodCommand    = "item/commandExecution/requestApproval"
	methodFile       = "item/fileChange/requestApproval"
	methodQuestion   = "item/tool/requestUserInput"
	methodPermission = "dalgona/workspace/requestPermission"
)

const (
	sandboxHeader    = "Sandbox"
	sandboxAccept    = "Accept"
	sandboxDangerous = "Dangerously accept"
)

type kind uint8

const (
	kindCommand kind = iota + 1
	kindFile
	kindTrust
	kindPermission
	kindSandbox
	kindQuestion
)

type permission struct {
	ThreadID        string   `json:"threadId"`
	RunnerID        string   `json:"runnerId"`
	BindingRevision string   `json:"bindingRevision"`
	Root            string   `json:"root"`
	Rights          []string `json:"rights"`
	Reason          string   `json:"reason"`
}

type details struct {
	kind       kind
	command    rpc.CommandExecutionRequestApprovalParams
	file       rpc.FileChangeRequestApprovalParams
	permission permission
	questions  rpc.ToolRequestUserInputParams
	malformed  error
}

func known(method string) bool {
	switch method {
	case methodCommand, methodFile, methodQuestion, methodPermission:
		return true
	}
	return false
}

func parse(method string, params jsontext.Value) details {
	var d details
	switch method {
	case methodCommand:
		d.malformed = json.Unmarshal(params, &d.command)
		d.kind = kindCommand
		if d.command.ItemID == "" {
			d.kind = kindTrust
		}
	case methodFile:
		d.kind, d.malformed = kindFile, json.Unmarshal(params, &d.file)
	case methodPermission:
		d.kind, d.malformed = kindPermission, json.Unmarshal(params, &d.permission)
	case methodQuestion:
		d.kind, d.malformed = parseQuestions(params, &d.questions)
	}
	return d
}

func parseQuestions(params jsontext.Value, into *rpc.ToolRequestUserInputParams) (kind, error) {
	if err := json.Unmarshal(params, into); err != nil {
		return kindQuestion, err
	}
	if len(into.Questions) == 0 {
		return kindQuestion, errNoQuestions
	}
	if sandboxChoice(into.Questions) {
		return kindSandbox, nil
	}
	return kindQuestion, nil
}

func sandboxChoice(questions []rpc.ToolRequestUserInputQuestion) bool {
	if len(questions) != 1 {
		return false
	}
	q := questions[0]
	labels := make([]string, 0, len(q.Options))
	for _, o := range q.Options {
		labels = append(labels, o.Label)
	}
	return q.Header == sandboxHeader && !q.IsOther && slices.Equal(labels, []string{sandboxAccept, sandboxDangerous})
}

type refusal string

func (r refusal) Error() string { return string(r) }

const errNoQuestions refusal = "the question request carries no questions"

func decisionResult(decision string) jsontext.Value {
	out, _ := json.Marshal(rpc.CommandExecutionRequestApprovalResponse{Decision: decision})
	return out
}

func answersResult(answers map[string]rpc.ToolRequestUserInputAnswer) jsontext.Value {
	out, _ := json.Marshal(rpc.ToolRequestUserInputResponse{Answers: answers}, json.Deterministic(true))
	return out
}

func dismissResult() jsontext.Value {
	return answersResult(map[string]rpc.ToolRequestUserInputAnswer{})
}

func (d details) approval() bool {
	return d.kind == kindCommand || d.kind == kindFile || d.kind == kindTrust
}

func (d details) kicker() string {
	switch d.kind {
	case kindPermission:
		return "Workspace access required"
	case kindSandbox:
		return "Sandbox choice required"
	case kindQuestion:
		return "Question required"
	case kindCommand, kindFile, kindTrust:
	}
	return "Approval required"
}

func (d details) title() string {
	switch d.kind {
	case kindCommand:
		return "Run this command?"
	case kindFile:
		return "Apply these changes?"
	case kindTrust:
		return "Trust this workspace?"
	case kindPermission:
		return "Allow workspace access?"
	case kindSandbox:
		return "Run commands without a sandbox?"
	case kindQuestion:
	}
	if len(d.questions.Questions) > 1 {
		return "Answer these questions"
	}
	return "Answer this question"
}
