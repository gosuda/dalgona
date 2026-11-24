package main

import (
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"strconv"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/rpcserver"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/remote"
)

const (
	methodRemoteStatus   = "dalgona/remote/status"
	methodRemoteProgress = "dalgona/remote/progress"
	remoteOutputLimit    = 64 << 10
	remotePathLimit      = 1000
	remoteStatusDefault  = 100
	remoteStatusMax      = 500
	remoteProgressGap    = 250 * time.Millisecond
)

type remotePullResult struct {
	ExitCode        int      `json:"exitCode"`
	Output          string   `json:"output"`
	ChangedPaths    []string `json:"changedPaths"`
	ConflictPaths   []string `json:"conflictPaths"`
	BindingRevision string   `json:"bindingRevision,omitzero"`
	Truncated       bool     `json:"truncated"`
}

type remoteCandidate struct {
	BoxID     string   `json:"boxId"`
	Service   string   `json:"service"`
	ThreadIDs []string `json:"threadIds"`
	Reason    string   `json:"reason"`
}

type remoteFailure struct {
	BoxID string `json:"boxId"`
	Kind  string `json:"kind"`
}

type remoteGCResult struct {
	ExitCode        int               `json:"exitCode"`
	Output          string            `json:"output"`
	Candidates      []remoteCandidate `json:"candidates"`
	Destroyed       []string          `json:"destroyed"`
	Failures        []remoteFailure   `json:"failures"`
	PreviewRevision string            `json:"previewRevision"`
	Truncated       bool              `json:"truncated"`
}

type remoteRunner struct {
	Name         string   `json:"name"`
	Service      string   `json:"service"`
	Available    bool     `json:"available"`
	Capabilities []string `json:"capabilities"`
}

type remoteBinding struct {
	ThreadID        string   `json:"threadId"`
	RunnerID        string   `json:"runnerId"`
	BoxID           string   `json:"boxId,omitzero"`
	SharedThreadIDs []string `json:"sharedThreadIds"`
	State           string   `json:"state"`
	BindingRevision string   `json:"bindingRevision"`
}

type remoteStatusResult struct {
	Runners        []remoteRunner  `json:"runners"`
	Bindings       []remoteBinding `json:"bindings"`
	LedgerRevision string          `json:"ledgerRevision"`
	NextCursor     string          `json:"nextCursor,omitzero"`
}

type remoteProgress struct {
	OperationID string `json:"operationId"`
	Phase       string `json:"phase"`
	Completed   string `json:"completed"`
	Total       string `json:"total"`
}

func boundedOutput(output string) string {
	if len(output) <= remoteOutputLimit {
		return output
	}
	return strings.ToValidUTF8(output[:remoteOutputLimit], "")
}

func boundedPaths(paths []string) ([]string, bool) {
	if paths == nil {
		return []string{}, false
	}
	return paths[:min(len(paths), remotePathLimit)], len(paths) > remotePathLimit
}

func progressSink(ctx context.Context, operationID string) remote.Progress {
	if operationID == "" {
		return nil
	}
	var last time.Time
	return func(phase client.ProgressPhase, completed, total uint64) {
		if wait := remoteProgressGap - time.Since(last); !last.IsZero() && wait > 0 {
			select {
			case <-time.After(wait):
			case <-ctx.Done():
				return
			}
		}
		last = time.Now()
		params, err := json.Marshal(remoteProgress{OperationID: operationID, Phase: string(phase), Completed: rpc.FormatDecimal(completed), Total: rpc.FormatDecimal(total)})
		if err == nil {
			_ = rpcserver.Notify(ctx, methodRemoteProgress, params)
		}
	}
}

func nativePull(ctx context.Context, opts remote.Options, call remoteCall) (jsontext.Value, error) {
	pulled, outcome := remote.NativePull(ctx, opts, call.ThreadID, progressSink(ctx, call.OperationID))
	changed, changedCut := boundedPaths(pulled.ChangedPaths)
	conflicts, conflictCut := boundedPaths(pulled.ConflictPaths)
	return json.Marshal(remotePullResult{
		ExitCode: outcome.ExitCode, Output: boundedOutput(outcome.Output), ChangedPaths: changed, ConflictPaths: conflicts,
		BindingRevision: pulled.BindingRevision, Truncated: changedCut || conflictCut,
	})
}

func nativeGC(ctx context.Context, opts remote.Options, call remoteCall) (jsontext.Value, error) {
	collected, outcome, err := remote.NativeGC(ctx, opts, call.Yes, call.PreviewRevision)
	if err != nil {
		return nil, rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorStaleRevision, CurrentRevision: collected.PreviewRevision})
	}
	result := remoteGCResult{
		ExitCode: outcome.ExitCode, Output: boundedOutput(outcome.Output), PreviewRevision: collected.PreviewRevision,
		Candidates: []remoteCandidate{}, Destroyed: []string{}, Failures: []remoteFailure{},
	}
	for _, box := range collected.Candidates[:min(len(collected.Candidates), remotePathLimit)] {
		result.Candidates = append(result.Candidates, remoteCandidate{BoxID: box.ID, Service: box.Service, ThreadIDs: box.Threads, Reason: "every bound thread is archived"})
	}
	for _, box := range collected.Destroyed[:min(len(collected.Destroyed), remotePathLimit)] {
		result.Destroyed = append(result.Destroyed, box.ID)
	}
	for _, failure := range collected.Failures[:min(len(collected.Failures), remotePathLimit)] {
		result.Failures = append(result.Failures, remoteFailure{BoxID: failure.BoxID, Kind: failure.Kind})
	}
	result.Truncated = len(collected.Candidates) > remotePathLimit || len(collected.Destroyed) > remotePathLimit || len(collected.Failures) > remotePathLimit
	return json.Marshal(result)
}

func nativeStatus(ctx context.Context, opts remote.Options, call remoteCall) (jsontext.Value, error) {
	limit := call.Limit
	if limit == 0 {
		limit = remoteStatusDefault
	}
	if limit < 0 || limit > remoteStatusMax {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	status, err := remote.NewBinder(opts).Status(ctx, call.ThreadID)
	if err != nil {
		return nil, statusError(err)
	}
	offset, err := statusOffset(call.Cursor, status.LedgerRevision)
	if err != nil {
		return nil, err
	}
	return json.Marshal(statusPage(status, offset, limit))
}

func statusError(err error) error {
	if errors.Is(err, remote.ErrUnknownThread) {
		return rpc.NativeKind(rpc.ErrorNotFound)
	}
	return rpc.NativeKind(rpc.ErrorUnavailable)
}

func statusOffset(cursor, revision string) (int, error) {
	if cursor == "" {
		return 0, nil
	}
	position, cursorRevision, ok := strings.Cut(cursor, ".")
	offset, err := strconv.Atoi(position)
	if !ok || err != nil || offset < 0 {
		return 0, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	if cursorRevision != revision {
		return 0, rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorStaleRevision, CurrentRevision: revision})
	}
	return offset, nil
}

func statusPage(status remote.Status, offset, limit int) remoteStatusResult {
	page := remoteStatusResult{Runners: []remoteRunner{}, Bindings: []remoteBinding{}, LedgerRevision: status.LedgerRevision}
	end := min(offset+limit, len(status.Runners)+len(status.Bindings))
	for index := offset; index < end; index++ {
		if index < len(status.Runners) {
			r := status.Runners[index]
			page.Runners = append(page.Runners, remoteRunner{Name: r.Name, Service: r.Service, Available: r.Available, Capabilities: r.Capabilities})
			continue
		}
		b := status.Bindings[index-len(status.Runners)]
		page.Bindings = append(page.Bindings, remoteBinding{
			ThreadID: b.ThreadID, RunnerID: b.RunnerID, BoxID: b.BoxID, SharedThreadIDs: append([]string{}, b.SharedThreadIDs...),
			State: b.State, BindingRevision: b.BindingRevision,
		})
	}
	if end < len(status.Runners)+len(status.Bindings) {
		page.NextCursor = strconv.Itoa(end) + "." + status.LedgerRevision
	}
	return page
}
