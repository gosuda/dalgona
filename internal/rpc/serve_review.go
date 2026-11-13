package rpc

import (
	"context"
	"encoding/json/jsontext"
	"fmt"
	"regexp"
	"strings"

	"github.com/gosuda/dalgona/internal/agent"
)

const (
	reviewTargetUncommitted = "uncommittedChanges"
	reviewTargetBase        = "baseBranch"
	reviewTargetCommit      = "commit"
	reviewTargetCustom      = "custom"
	reviewDeliveryInline    = "inline"
	reviewUncommittedPrompt = "Review the uncommitted changes in this workspace: staged, unstaged, and untracked files. " +
		"Use git status and git diff HEAD to see them. Report concrete defects with file and line, most severe first. Do not edit files."
	reviewFindingsSuffix = " Report concrete defects with file and line, most severe first. Do not edit files."
)

var (
	reviewBranchPattern = regexp.MustCompile(`^[A-Za-z0-9._/][A-Za-z0-9._/-]*$`)
	reviewSHAPattern    = regexp.MustCompile(`^[0-9a-fA-F]{4,64}$`)
)

type ReviewStartResponse struct {
	ReviewThreadID string `json:"reviewThreadId"`
	Turn           Turn   `json:"turn"`
}

func (r ReviewStartResponse) write(o *out) {
	o.open()
	o.str("reviewThreadId", r.ReviewThreadID)
	o.key("turn")
	r.Turn.write(o)
	o.close()
}

type reviewTarget struct {
	Kind         string
	Branch       string
	SHA          string
	Title        string
	Instructions string
}

func (t *reviewTarget) member(name string, d *jsontext.Decoder) error {
	var field *string
	switch name {
	case "type":
		field = &t.Kind
	case "branch":
		field = &t.Branch
	case "sha":
		field = &t.SHA
	case "title":
		field = &t.Title
	case "instructions":
		field = &t.Instructions
	default:
		return d.SkipValue()
	}
	v, err := readNullableString(d)
	*field = v
	return err
}

type reviewStartParams struct {
	threadID string
	delivery *string
	target   reviewTarget
}

func decodeReviewStart(params jsontext.Value) (reviewStartParams, error) {
	var p reviewStartParams
	err := decodeParams(params, func(name string, d *jsontext.Decoder) error {
		switch name {
		case "threadId":
			v, err := readString(d)
			p.threadID = v
			return err
		case "delivery":
			if null, err := consumeNull(d); null || err != nil {
				return err
			}
			v, err := readString(d)
			p.delivery = &v
			return err
		case "target":
			return readMembers(d, p.target.member)
		}
		return d.SkipValue()
	})
	return p, err
}

func readNullableString(d *jsontext.Decoder) (string, error) {
	if d.PeekKind() == jsontext.KindNull {
		_, err := d.ReadToken()
		return "", err
	}
	return readString(d)
}

func consumeNull(d *jsontext.Decoder) (bool, error) {
	if d.PeekKind() != jsontext.KindNull {
		return false, nil
	}
	_, err := d.ReadToken()
	return true, err
}

func reviewPrompt(t reviewTarget) (string, error) {
	switch t.Kind {
	case reviewTargetUncommitted:
		return reviewUncommittedPrompt, nil
	case reviewTargetBase:
		if !reviewBranchPattern.MatchString(t.Branch) {
			return "", invalidParams("invalid branch " + t.Branch)
		}
		return fmt.Sprintf("Review the changes on the current branch against %s. Use git diff %s...HEAD to see them.%s",
			t.Branch, t.Branch, reviewFindingsSuffix), nil
	case reviewTargetCommit:
		if !reviewSHAPattern.MatchString(t.SHA) {
			return "", invalidParams("invalid sha " + t.SHA)
		}
		subject := "commit " + t.SHA
		if t.Title != "" {
			subject += ": " + t.Title
		}
		return fmt.Sprintf("Review the changes introduced by %s. Use git show %s to see them.%s", subject, t.SHA, reviewFindingsSuffix), nil
	case reviewTargetCustom:
		if strings.TrimSpace(t.Instructions) == "" {
			return "", invalidParams("instructions must not be empty")
		}
		return t.Instructions, nil
	}
	return "", invalidParams("unsupported review target " + t.Kind)
}

func (s *server) reviewStart(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	p, err := decodeReviewStart(params)
	if err != nil {
		return nil, err
	}
	if p.delivery != nil && *p.delivery != reviewDeliveryInline {
		return nil, invalidParams("unsupported delivery " + *p.delivery)
	}
	prompt, err := reviewPrompt(p.target)
	if err != nil {
		return nil, err
	}
	thread, err := s.watched(ctx, p.threadID)
	if err != nil {
		return nil, err
	}
	status, err := thread.Status()
	if err != nil {
		return nil, hostError(err, p.threadID, "")
	}
	if err := thread.Submit(ctx, agent.Input{Kind: agent.InputPrompt, Text: prompt}); err != nil {
		return nil, hostError(err, p.threadID, "")
	}
	turnID := submittedTurn(thread, status.Head)
	return encode(ReviewStartResponse{ReviewThreadID: p.threadID, Turn: Turn{ID: turnID, Status: statusInProgress}})
}
