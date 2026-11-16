package acp

import (
	"cmp"
	"context"
	"encoding/json/jsontext"
	"slices"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const (
	askRefusal = "ask is not supported by this client"

	optionAllowOnce   = "allow_once"
	optionAllowAlways = "allow_always"
	optionRejectOnce  = "reject_once"

	decisionAccept  = "accept"
	decisionSession = "acceptForSession"
	decisionDecline = "decline"
	decisionCancel  = "cancel"

	methodCommandApproval = "item/commandExecution/requestApproval"
	methodFileApproval    = "item/fileChange/requestApproval"
	methodUserInput       = "item/tool/requestUserInput"
)

type (
	approvalParams struct {
		ThreadID              string   `json:"threadId"`
		ItemID                string   `json:"itemId"`
		Command               string   `json:"command"`
		Reason                string   `json:"reason"`
		PlanPath              string   `json:"planPath"`
		RequiredHumanApproval bool     `json:"requiredHumanApproval"`
		AvailableDecisions    []string `json:"availableDecisions"`
	}
	permissionOption struct {
		OptionID string `json:"optionId"`
		Name     string `json:"name"`
		Kind     string `json:"kind"`
		decision string
	}
	permissionRequest struct {
		SessionID string             `json:"sessionId"`
		ToolCall  toolCallNote       `json:"toolCall"`
		Options   []permissionOption `json:"options"`
	}
	permissionOutcome struct {
		Outcome  string `json:"outcome"`
		OptionID string `json:"optionId"`
	}
	permissionResponse struct {
		Outcome permissionOutcome `json:"outcome"`
	}
)

func (a *adapter) Request(ctx context.Context, method string, params jsontext.Value) (any, error) {
	switch method {
	case methodCommandApproval, methodFileApproval:
		return a.approve(ctx, method, params)
	case methodUserInput:
		return nil, rpcError(codeMethodNotFound, askRefusal)
	}
	return nil, rpcError(codeMethodNotFound, "Method not found")
}

func (a *adapter) approve(ctx context.Context, method string, params jsontext.Value) (any, error) {
	p, err := decode[approvalParams](params)
	if err != nil {
		return nil, err
	}
	s := a.lookup(p.ThreadID)
	if s == nil {
		return nil, rpcError(codeInvalidParams, "unknown thread "+p.ThreadID)
	}
	options := permissionOptions(p)
	request := permissionRequest{SessionID: s.id, ToolCall: permissionToolCall(method, p), Options: options}
	var response permissionResponse
	if err = a.peer.call(ctx, "session/request_permission", request, &response); err != nil {
		return nil, err
	}
	return rpc.ApprovalResponse{Decision: decide(response.Outcome, options)}, nil
}

func humanOnly(p approvalParams) bool {
	return p.RequiredHumanApproval || p.PlanPath != ""
}

func permissionOptions(p approvalParams) []permissionOption {
	options := []permissionOption{{OptionID: optionAllowOnce, Name: "Allow once", Kind: optionAllowOnce, decision: decisionAccept}}
	if !humanOnly(p) && slices.Contains(p.AvailableDecisions, decisionSession) {
		options = append(options, permissionOption{OptionID: optionAllowAlways, Name: "Allow for this thread", Kind: optionAllowAlways, decision: decisionSession})
	}
	return append(options, permissionOption{OptionID: optionRejectOnce, Name: "Reject", Kind: optionRejectOnce, decision: decisionDecline})
}

func permissionToolCall(method string, p approvalParams) toolCallNote {
	call := toolCallNote{ToolCallID: p.ItemID, Status: "pending"}
	switch {
	case p.PlanPath != "":
		call.Kind = "switch_mode"
		call.Title = cmp.Or(p.Command, "Exit plan mode")
		call.Content = appendText(nil, p.Reason)
		call.Locations = []location{{Path: p.PlanPath}}
	case method == methodFileApproval:
		call.Kind = "edit"
		call.Title = cmp.Or(p.Reason, "Edit files")
	default:
		call.Kind = "execute"
		call.Title = cmp.Or(p.Command, "Run command")
		call.Content = appendText(nil, p.Reason)
	}
	return call
}

func decide(outcome permissionOutcome, sent []permissionOption) string {
	switch outcome.Outcome {
	case "cancelled":
		return decisionCancel
	case "selected":
		for _, option := range sent {
			if option.OptionID == outcome.OptionID {
				return option.decision
			}
		}
	}
	return decisionDecline
}
