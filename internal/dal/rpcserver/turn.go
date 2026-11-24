package rpcserver

import (
	"cmp"
	"context"
	"encoding/json/jsontext"
	"strconv"
	"strings"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/textinput"
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
	items          int
	textErr        error
	loads          []imageLoad
	images         []agent.Image
	imageErr       error
	imageBytes     int64
}

func (p *turnParams) member(name string, d *jsontext.Decoder) error {
	if target := p.stringField(name); target != nil {
		v, err := rpc.ReadString(d)
		*target = v
		return err
	}
	switch name {
	case "input":
		return rpc.ReadArray(d, func() error { return p.readInput(d) })
	case "sandboxPolicy":
		return rpc.ReadMembers(d, p.sandboxMember)
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
	v, err := rpc.ReadString(d)
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
	err := rpc.ReadMembers(d, stringMembers(map[string]*string{"type": &kind, "text": &text, "path": &path, "url": &url}))
	if err != nil {
		return err
	}
	index := p.items
	p.items++
	switch kind {
	case "text":
		p.checkText(index, "text", text)
		p.text = append(p.text, text)
	case "mention", "skill":
		p.checkText(index, "path", path)
		p.text = append(p.text, "@"+path)
	case "image":
		p.queueImage(func(admit func(int64) error) (agent.Image, error) { return imageFromURL(url, admit) })
	case "localImage":
		p.checkText(index, "path", path)
		p.queueImage(func(admit func(int64) error) (agent.Image, error) { return imageFromPath(path, admit) })
	default:
		return invalidParams("unsupported input type " + kind)
	}
	return nil
}

func (p *turnParams) checkText(index int, member, value string) {
	if p.textErr != nil {
		return
	}
	field := textinput.Field{Name: "input[" + strconv.Itoa(index) + "]." + member}
	if err := field.Check(value); err != nil {
		p.textErr = invalidParams(err.Error())
	}
}

func decodeTurn(params jsontext.Value) (turnParams, error) {
	var p turnParams
	if err := decodeParams(params, p.member); err != nil {
		return p, err
	}
	if p.textErr != nil {
		return p, p.textErr
	}
	p.loadImages()
	return p, nil
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
	if err = s.refuseUnsupportedEffort(ctx, cmp.Or(in.Model, info.Model, s.host.Config().Model), in.Effort); err != nil {
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
	return rpc.Marshal(rpc.TurnStartResponse{Turn: rpc.Turn{ID: turnID, Status: rpc.StatusInProgress}})
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
		return rpc.Marshal(rpc.TurnSteerResponse{TurnID: expected})
	}
	status, err := thread.Status()
	if err != nil {
		return nil, hostError(err, p.threadID, "")
	}
	return rpc.Marshal(rpc.TurnSteerResponse{TurnID: status.Turn})
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
