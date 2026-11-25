package agent

import (
	"bytes"
	"cmp"
	"context"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"slices"
	"strconv"
	"strings"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dal/tool/snapshot"
)

const (
	invocationExpired       = "reference_expired: this edit comes from a model request made before the host opened this thread again. Read the files again and send a new edit."
	invocationMismatch      = "invocation_mismatch: this call repeats an earlier invocation with a different payload. Send the edit as a new call."
	invocationIndeterminate = "indeterminate: the host restarted while this edit ran, and its result was not recorded. Read the files before you edit them again."
	replayedPointer         = "/hashline/replayed"
)

type invocationClaim struct {
	identity    string
	fingerprint string
	profile     tool.Profile
}

type invocationOwner struct {
	fingerprint string
	done        chan struct{}
	result      event.ToolCompleted
	err         error
}

func (w *worker) claimFor(p *prepared, profile tool.Profile, request uint64) *invocationClaim {
	consumer := snapshot.ConsumerModel
	identity := strings.Join([]string{
		w.t.id, strconv.FormatUint(w.t.openGeneration, 10), w.turnID,
		strconv.FormatUint(request, 10), consumer, p.call.ID,
	}, "/")
	return &invocationClaim{
		identity: identity, profile: profile,
		fingerprint: invocationFingerprint(profile, w.t.created.Cwd, consumer, p.args),
	}
}

func invocationFingerprint(profile tool.Profile, workspace, consumer string, payload []byte) string {
	hash := sha256.New()
	var size [8]byte
	for _, part := range []string{profile.Style, strconv.Itoa(profile.Version), workspace, consumer} {
		binary.BigEndian.PutUint64(size[:], uint64(len(part)))
		_, _ = hash.Write(size[:])
		_, _ = io.WriteString(hash, part)
	}
	binary.BigEndian.PutUint64(size[:], uint64(len(payload)))
	_, _ = hash.Write(size[:])
	_, _ = hash.Write(payload)
	return hex.EncodeToString(hash.Sum(nil))
}

func (w *worker) claimInvocation(ctx context.Context, p *prepared) (bool, error) {
	profile, ok := profileOf(p.tool)
	if !ok {
		return false, nil
	}
	evidence, err := w.evidenceFor(ctx, p.call.ID)
	if err != nil {
		return false, err
	}
	p.evidence = evidence
	p.claim = w.claimFor(p, profile, evidence.Cutoff)
	return evidence.Cutoff < w.t.openGeneration, nil
}

func expiredRefusal(profile tool.Profile) verdict {
	refusal := decline("rule:invocation", invocationExpired)
	refusal.meta = outcomeMeta(tool.Outcome{
		Style: profile.Style, Version: profile.Version, Status: tool.StatusNeedsContext, Commit: tool.CommitNone,
		Reason: tool.ReasonReferenceExpired, Class: tool.ErrClassProof,
	})
	return refusal
}

func (w *worker) invocationStarted(p *prepared) (event.HashlineInvocationStarted, error) {
	if p.claim == nil || p.plan == nil {
		return event.HashlineInvocationStarted{}, nil
	}
	paths := make([]string, len(p.plan.RequestedPaths))
	for i, path := range p.plan.RequestedPaths {
		masked, err := w.t.host.mask(path)
		if err != nil {
			return event.HashlineInvocationStarted{}, fmt.Errorf("agent: mask invocation path: %w", err)
		}
		paths[i] = masked
	}
	return event.HashlineInvocationStarted{
		CallID: p.call.ID, TurnID: w.turnID, Identity: p.claim.identity, Fingerprint: p.claim.fingerprint,
		Style: p.claim.profile.Style, ContractVersion: p.claim.profile.Version, Paths: paths,
	}, nil
}

func (o *invocationOwner) finish(result event.ToolCompleted, err error) {
	o.result, o.err = result, err
	close(o.done)
}

func (o *invocationOwner) join(ctx context.Context, claim invocationClaim, callID string) (event.ToolCompleted, error) {
	if o.fingerprint != claim.fingerprint {
		outcome := tool.Outcome{
			Style: claim.profile.Style, Version: claim.profile.Version, Status: tool.StatusRejected, Commit: tool.CommitNone,
			Reason: tool.ReasonInvocationMismatch, Class: tool.ErrClassResolve,
		}
		return event.ToolCompleted{CallID: callID, Output: invocationMismatch, Recovered: "none", Meta: outcomeMeta(outcome)}, nil
	}
	select {
	case <-o.done:
	case <-ctx.Done():
		return event.ToolCompleted{}, context.Cause(ctx)
	}
	if o.err != nil {
		return event.ToolCompleted{}, fmt.Errorf("agent: invocation %s failed in its first dispatch: %w", claim.identity, o.err)
	}
	return replayOf(o.result)
}

func replayOf(completed event.ToolCompleted) (event.ToolCompleted, error) {
	meta, err := replayedMeta(completed.Meta)
	if err != nil {
		return event.ToolCompleted{}, err
	}
	completed.Meta = meta
	return completed, nil
}

func replayedMeta(meta jsontext.Value) (jsontext.Value, error) {
	if len(meta) == 0 {
		return meta, nil
	}
	decoder := jsontext.NewDecoder(bytes.NewReader(meta))
	var out bytes.Buffer
	encoder := jsontext.NewEncoder(&out)
	for {
		token, err := decoder.ReadToken()
		if errors.Is(err, io.EOF) {
			return jsontext.Value(bytes.TrimSuffix(out.Bytes(), []byte("\n"))), nil
		}
		if err != nil {
			return nil, fmt.Errorf("agent: read replayed meta: %w", err)
		}
		if token.Kind() == 'f' && decoder.StackPointer() == replayedPointer {
			token = jsontext.True
		}
		if err := encoder.WriteToken(token); err != nil {
			return nil, fmt.Errorf("agent: write replayed meta: %w", err)
		}
	}
}

func (w *worker) recoverInvocation(ctx context.Context, v view, callID, identity string) error {
	invocation := v.state.Invocations[identity]
	if !invocation.Settled() {
		return w.completeIndeterminate(ctx, v, callID, invocation)
	}
	settled, ok := recordAt(v, invocation.SettledSeq).(event.HashlineInvocationSettled)
	if !ok {
		return fmt.Errorf("agent: invocation %s has no settled record at %d", identity, invocation.SettledSeq)
	}
	completed, err := replayOf(event.ToolCompleted{
		CallID: callID, OK: settled.OK, Output: settled.Output, Spill: settled.Spill, Meta: settled.Meta, Recovered: recoveredReplay,
	})
	if err != nil {
		return err
	}
	_, err = w.t.appendEvents(ctx, completed)
	return err
}

func (w *worker) completeIndeterminate(ctx context.Context, v view, callID string, invocation event.InvocationState) error {
	started, ok := recordAt(v, invocation.StartedSeq).(event.HashlineInvocationStarted)
	if !ok {
		return fmt.Errorf("agent: invocation of %s has no start record at %d", callID, invocation.StartedSeq)
	}
	files, ok := recordAt(v, invocation.StartedSeq-1).(event.ToolStarted)
	if !ok || files.CallID != callID || files.TurnID != started.TurnID {
		return fmt.Errorf("agent: invocation of %s has no matching tool start before %d", callID, invocation.StartedSeq)
	}
	states, err := w.pathStates(ctx, files.Files)
	if err != nil {
		return err
	}
	outcome := tool.Outcome{
		Style: started.Style, Version: started.ContractVersion, Status: tool.StatusIndeterminate, Commit: tool.CommitUnknown,
		Reason: tool.ReasonCommitUnknown, Class: tool.ErrClassIO, Paths: states,
	}
	meta, err := outcome.Meta(false)
	if err != nil {
		return fmt.Errorf("agent: indeterminate outcome of %s: %w", callID, err)
	}
	_, err = w.t.appendEvents(ctx, event.ToolCompleted{CallID: callID, Output: invocationIndeterminate, Recovered: recoveredStopped, Meta: meta})
	return err
}

func (w *worker) pathStates(ctx context.Context, files []event.FileChange) ([]tool.PathState, error) {
	states := make([]tool.PathState, 0, len(files))
	for _, file := range files {
		sha, readErr := w.fileSHA(ctx, file.Path)
		path, err := w.t.host.mask(file.Path)
		if err != nil {
			return nil, fmt.Errorf("agent: mask path state: %w", err)
		}
		states = append(states, tool.PathState{Path: path, State: tool.PathStateOf(sha, file.PreSHA, file.PostSHA, readErr == nil)})
	}
	return states, nil
}

func recordAt(v view, seq uint64) event.Event {
	i, found := slices.BinarySearchFunc(v.records, seq, func(item event.Seqd, target uint64) int { return cmp.Compare(item.Seq, target) })
	if !found {
		return nil
	}
	return v.records[i].Event
}
