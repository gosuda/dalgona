package ratchet

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"
	"sync"
	"time"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/tool"
)

func (t Tool) approve(ctx context.Context, callID string, f *flow, a args, mu *sync.Mutex) (tool.Result, error) {
	if err := checkApproveArgs(a); err != nil {
		return tool.Result{}, err
	}
	s, sha, err := lockedDigest(mu, f, a.Stage)
	if err != nil {
		return tool.Result{}, err
	}
	if approvalCurrent(s, a.Stage, sha) {
		return toolResult(map[string]any{"approved": true, "asked": false, "sha": sha, "stage": a.Stage})
	}
	if err = t.checkApprovalRoom(s, f.name, a, sha); err != nil {
		return tool.Result{}, err
	}
	answer, err := t.asker.Ask(ctx, callID, approvalQuestion(s, f.name, a, sha))
	if outcome, declined := declinedOutcome(err); declined {
		return outcome, nil
	}
	if err != nil {
		return tool.Result{}, err
	}
	return answered(mu, f, a.Stage, sha, answer)
}

func checkApproveArgs(a args) error {
	if !slices.Contains(stages, a.Stage) {
		return fmt.Errorf("stage must be inputs, grader, or plan: %q", a.Stage)
	}
	if strings.TrimSpace(a.Question) == "" {
		return errors.New("approve needs a question")
	}
	return nil
}

func (t Tool) checkApprovalRoom(s *state, flowName string, a args, sha string) error {
	roomChars, roomLines := t.asker.PreviewRoom()
	block := hostBlock(s, flowName, a.Stage, sha)
	blockLines, blockChars := measure(block)
	if blockLines > roomLines || blockChars > roomChars {
		return fmt.Errorf("the %s facts need %d lines and %d characters, and one question holds %d lines and %d characters; list directories in place of single files, or shorten the command", a.Stage, blockLines, blockChars, roomLines, roomChars)
	}
	lines, chars := measure(approvalPreview(block, a.Preview))
	if lines > roomLines || chars > roomChars {
		previewLines, previewChars := measure(a.Preview)
		return fmt.Errorf("preview is %d lines and %d characters; the room after the host facts is %d lines and %d characters", previewLines, previewChars, max(0, roomLines-blockLines-1), max(0, roomChars-blockChars-2))
	}
	return nil
}

func declinedOutcome(err error) (tool.Result, bool) {
	switch {
	case errors.Is(err, ErrNoClient):
		return tool.Result{Output: []byte("declined: " + ErrNoClient.Error())}, true
	case errors.Is(err, ErrCancelled):
		return tool.Result{Output: []byte("cancelled: the question was closed; nothing was recorded")}, true
	}
	return tool.Result{}, false
}

func answered(mu *sync.Mutex, f *flow, stage, sha string, answer Answer) (tool.Result, error) {
	switch answer.Label {
	case "Approve":
		if err := recordApproval(mu, f, stage, sha); err != nil {
			return tool.Result{}, err
		}
		return toolResult(map[string]any{"approved": true, "asked": true, "sha": sha, "stage": stage})
	case "Abort":
		return toolResult(map[string]any{"aborted": true, "approved": false, "asked": true, "stage": stage})
	case "Revise", "":
		return toolResult(map[string]any{"approved": false, "asked": true, "feedback": answer.Other, "stage": stage})
	}
	return tool.Result{}, fmt.Errorf("unexpected answer %s", answer.Label)
}

func approvalPreview(block, preview string) string {
	return block + "\n\n" + preview
}

func approvalQuestion(s *state, flowName string, a args, sha string) Question {
	preview := approvalPreview(hostBlock(s, flowName, a.Stage, sha), a.Preview)
	return Question{
		Header: "Ratchet " + a.Stage,
		Text:   a.Question,
		Options: []Option{
			{Label: "Approve", Description: "Record approval of digest " + sha[:12], Preview: preview},
			{Label: "Revise", Description: "Record nothing and send feedback in Other"},
			{Label: "Abort", Description: "Record nothing and stop this flow"},
		},
	}
}

func lockedDigest(mu *sync.Mutex, f *flow, stage string) (*state, string, error) {
	mu.Lock()
	defer mu.Unlock()
	s, err := f.load()
	if err != nil {
		return nil, "", err
	}
	sha, err := f.digest(s, stage)
	if err != nil {
		return nil, "", err
	}
	return s, sha, nil
}

func approvalCurrent(s *state, stage, sha string) bool {
	approval, ok := s.Approvals[stage]
	return ok && approval.SHA == sha
}

func recordApproval(mu *sync.Mutex, f *flow, stage, sha string) error {
	mu.Lock()
	defer mu.Unlock()
	s, err := f.load()
	if err != nil {
		return err
	}
	now, err := f.digest(s, stage)
	if err != nil {
		return err
	}
	if now != sha {
		return fmt.Errorf("the %s files changed while the user was reading; ask again", stage)
	}
	if s.Approvals == nil {
		s.Approvals = make(map[string]approval)
	}
	s.Approvals[stage] = approval{SHA: sha, At: time.Now().UTC()}
	return f.save(s)
}

func hostBlock(s *state, flowName, stage, sha string) string {
	lines := []string{"Stage: " + stage, "Digest: " + sha, "Flow: " + flowName}
	switch stage {
	case stageInputs:
		lines = append(lines, fileLines("Files:", s.CasesPaths)...)
	case stageGrader:
		lines = append(lines, fileLines("Files:", s.HarnessPaths)...)
	default:
		lines = append(lines, planLines(s)...)
	}
	return strings.Join(lines, "\n")
}

func fileLines(header string, paths []string) []string {
	lines := []string{header}
	for _, p := range paths {
		lines = append(lines, "- "+p)
	}
	return lines
}

func planLines(s *state) []string {
	guards := "none"
	if len(s.Goal.Hold) > 0 {
		parts := make([]string, 0, len(s.Goal.Hold))
		for _, m := range s.Goal.Hold {
			parts = append(parts, fmt.Sprintf("%s (%s)", m, metricDirection(s.Goal, m)))
		}
		guards = strings.Join(parts, ", ")
	}
	off := "none"
	if len(s.OffLimits) > 0 {
		off = strings.Join(s.OffLimits, ", ")
	}
	return []string{
		fmt.Sprintf("Target: %s (%s)", s.Goal.Target, s.Goal.Direction),
		"Guardrails: " + guards,
		fmt.Sprintf("Reps: %d", s.Reps),
		fmt.Sprintf("Split: %d train, %d test, seed %d", len(s.TrainIDs), len(s.TestIDs), s.SplitSeed),
		fmt.Sprintf("Stop: plateau %d, rounds %d", s.Stop.Plateau, s.Stop.Rounds),
		"Command: " + s.Command,
		"Change paths: " + strings.Join(s.ChangePaths, ", "),
		"Off limits: " + off,
	}
}

func measure(text string) (lines, chars int) {
	return strings.Count(text, "\n") + 1, utf8.RuneCountInString(text)
}
