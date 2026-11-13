package rpc

import (
	"cmp"
	"context"
	"encoding/json/jsontext"
	"strings"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
)

type turnParams struct {
	threadID       string
	expectedTurnID string
	turnID         string
	effort         string
	model          string
	approvalPolicy string
	sandboxPolicy  string
	text           []string
	images         []agent.Image
	imageErr       error
	imageBytes     int64
}

func (p *turnParams) member(name string, d *jsontext.Decoder) error {
	if target := p.stringField(name); target != nil {
		v, err := readString(d)
		*target = v
		return err
	}
	switch name {
	case "input":
		return readArray(d, func() error { return p.readInput(d) })
	case "sandboxPolicy":
		return readMembers(d, p.sandboxMember)
	case "approvalPolicy":
		v, err := readApprovalPolicy(d)
		p.approvalPolicy = v
		return err
	}
	return d.SkipValue()
}

func (p *turnParams) stringField(name string) *string {
	switch name {
	case "threadId":
		return &p.threadID
	case "expectedTurnId":
		return &p.expectedTurnID
	case "turnId":
		return &p.turnID
	case "effort":
		return &p.effort
	case "model":
		return &p.model
	}
	return nil
}

func (p *turnParams) sandboxMember(name string, d *jsontext.Decoder) error {
	if name != "type" {
		return d.SkipValue()
	}
	v, err := readString(d)
	p.sandboxPolicy = v
	return err
}

func sandboxPolicyMode(kind string) (config.SandboxMode, error) {
	switch kind {
	case "":
		return "", nil
	case "dangerFullAccess":
		return config.SandboxOff, nil
	case "workspaceWrite", "readOnly":
		return config.SandboxWorkspaceWrite, nil
	}
	return "", invalidParams("unsupported sandboxPolicy " + kind)
}

func (p *turnParams) prompt(current string) (agent.Input, error) {
	approval, err := approvalMode(p.approvalPolicy)
	if err != nil {
		return agent.Input{}, err
	}
	sandbox, err := sandboxPolicyMode(p.sandboxPolicy)
	if err != nil {
		return agent.Input{}, err
	}
	effort, err := effortMode(p.effort)
	if err != nil {
		return agent.Input{}, err
	}
	images, err := p.checkedImages()
	if err != nil {
		return agent.Input{}, err
	}
	in := agent.Input{Kind: agent.InputPrompt, Text: strings.Join(p.text, "\n"), Effort: string(effort), Approval: approval, Sandbox: sandbox, Images: images}
	if p.model != "" {
		in.Model = qualifiedModel(p.model, "", current)
	}
	return in, nil
}

var approvalRank = map[config.Approval]int{config.ApprovalYolo: 1, config.ApprovalAuto: 2, config.ApprovalAsk: 3}

func stricterApproval(thread, turn config.Approval) config.Approval {
	if approvalRank[thread] > approvalRank[turn] {
		return thread
	}
	return turn
}

func (p *turnParams) readInput(d *jsontext.Decoder) error {
	var kind, text, path, url string
	err := readMembers(d, stringMembers(map[string]*string{"type": &kind, "text": &text, "path": &path, "url": &url}))
	if err != nil {
		return err
	}
	switch kind {
	case "text":
		p.text = append(p.text, text)
	case "mention", "skill":
		p.text = append(p.text, "@"+path)
	case "image":
		p.takeImage(func(admit func(int64) error) (agent.Image, error) { return imageFromURL(url, admit) })
	case "localImage":
		p.takeImage(func(admit func(int64) error) (agent.Image, error) { return imageFromPath(path, admit) })
	default:
		return invalidParams("unsupported input type " + kind)
	}
	return nil
}

func decodeTurn(params jsontext.Value) (turnParams, error) {
	var p turnParams
	err := decodeParams(params, p.member)
	return p, err
}

func (s *server) turnStart(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	p, err := decodeTurn(params)
	if err != nil {
		return nil, err
	}
	thread, err := s.watched(ctx, p.threadID)
	if err != nil {
		return nil, err
	}
	info, _ := s.info(p.threadID)
	in, err := p.prompt(cmp.Or(info.Model, s.host.Config().Model))
	if err != nil {
		return nil, err
	}
	if in.Approval != "" {
		settings, settingsErr := thread.Settings()
		if settingsErr != nil {
			return nil, hostError(settingsErr, p.threadID, "")
		}
		in.Approval = stricterApproval(settings.Approval, in.Approval)
	}
	status, err := thread.Status()
	if err != nil {
		return nil, hostError(err, p.threadID, "")
	}
	if err := thread.Submit(ctx, in); err != nil {
		return nil, hostError(err, p.threadID, "")
	}
	turnID := submittedTurn(thread, status.Head)
	return encode(TurnStartResponse{Turn: Turn{ID: turnID, Status: statusInProgress}})
}

func submittedTurn(thread *agent.Thread, head uint64) string {
	read, err := thread.ReadAfter(head)
	if err == nil {
		for _, e := range read.Events {
			if message, ok := e.Event.(event.UserMessage); ok && e.Seq > head {
				return message.TurnID
			}
		}
	}
	status, err := thread.Status()
	if err != nil {
		return ""
	}
	return status.Turn
}

func (s *server) turnSteer(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	p, err := decodeTurn(params)
	if err != nil {
		return nil, err
	}
	thread, err := s.watched(ctx, p.threadID)
	if err != nil {
		return nil, err
	}
	expected := cmp.Or(p.expectedTurnID, p.turnID)
	images, err := p.checkedImages()
	if err != nil {
		return nil, err
	}
	in := agent.Input{Kind: agent.InputSteer, Text: strings.Join(p.text, "\n"), ExpectedTurnID: expected, Images: images}
	if err = thread.Submit(ctx, in); err != nil {
		return nil, hostError(err, p.threadID, expected)
	}
	if expected != "" {
		return encode(TurnSteerResponse{TurnID: expected})
	}
	status, err := thread.Status()
	if err != nil {
		return nil, hostError(err, p.threadID, "")
	}
	return encode(TurnSteerResponse{TurnID: status.Turn})
}

func (s *server) turnInterrupt(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	p, err := decodeTurn(params)
	if err != nil {
		return nil, err
	}
	thread, err := s.watched(ctx, p.threadID)
	if err != nil {
		return nil, err
	}
	id := cmp.Or(p.turnID, p.expectedTurnID)
	if id == "" {
		return nil, invalidParams("missing turnId")
	}
	if err := thread.InterruptTurn(ctx, id); err != nil {
		return nil, hostError(err, p.threadID, id)
	}
	return emptyObject(), nil
}
