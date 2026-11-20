package patch

import (
	"context"
	"encoding/json/jsontext"
	"errors"

	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dal/tool/snapshot"
)

const contractVersion = 1

type reasonError struct {
	reason tool.Reason
	class  tool.ErrorClass
	err    error
}

func (e *reasonError) Error() string { return e.err.Error() }

func (e *reasonError) Unwrap() error { return e.err }

func reasoned(reason tool.Reason, err error) error {
	return reasonedAs(reason, reasonRows[reason].class, err)
}

func reasonedAs(reason tool.Reason, class tool.ErrorClass, err error) error {
	return &reasonError{reason: reason, class: class, err: err}
}

type reasonRow struct {
	status tool.Status
	class  tool.ErrorClass
}

var reasonRows = map[tool.Reason]reasonRow{
	tool.ReasonReferenceUnknown:       {tool.StatusNeedsContext, tool.ErrClassProof},
	tool.ReasonReferenceExpired:       {tool.StatusNeedsContext, tool.ErrClassProof},
	tool.ReasonHistoryGap:             {tool.StatusNeedsContext, tool.ErrClassStale},
	tool.ReasonUnobservedRange:        {tool.StatusNeedsContext, tool.ErrClassProof},
	tool.ReasonObservationUnavailable: {tool.StatusRejected, tool.ErrClassBlocked},
	tool.ReasonObserverBlocked:        {tool.StatusRejected, tool.ErrClassBlocked},
	tool.ReasonNoReplaceUnavailable:   {tool.StatusRejected, tool.ErrClassFile},
	tool.ReasonBindingMismatch:        {tool.StatusRejected, tool.ErrClassProof},
	tool.ReasonOutsideWorkspace:       {tool.StatusRejected, tool.ErrClassFile},
	tool.ReasonPermissionDenied:       {tool.StatusRejected, tool.ErrClassBlocked},
	tool.ReasonTargetChanged:          {tool.StatusConflict, tool.ErrClassStale},
	tool.ReasonGapChanged:             {tool.StatusConflict, tool.ErrClassStale},
	tool.ReasonPathChanged:            {tool.StatusConflict, tool.ErrClassStale},
	tool.ReasonInvalidSyntax:          {tool.StatusRejected, tool.ErrClassParse},
	tool.ReasonInvalidRange:           {tool.StatusRejected, tool.ErrClassResolve},
	tool.ReasonMixedReferences:        {tool.StatusRejected, tool.ErrClassResolve},
	tool.ReasonOverlap:                {tool.StatusRejected, tool.ErrClassResolve},
	tool.ReasonInvalidShape:           {tool.StatusRejected, tool.ErrClassFile},
	tool.ReasonResourceLimit:          {tool.StatusRejected, tool.ErrClassLimit},
	tool.ReasonInterrupted:            {tool.StatusInterrupted, tool.ErrClassNone},
	tool.ReasonWriteFailed:            {tool.StatusWriteError, tool.ErrClassIO},
	tool.ReasonRollbackFailed:         {tool.StatusWriteError, tool.ErrClassIO},
	tool.ReasonSyncFailed:             {tool.StatusWriteError, tool.ErrClassIO},
	tool.ReasonCommitUnknown:          {tool.StatusIndeterminate, tool.ErrClassIO},
}

var sentinelReasons = []struct {
	sentinel error
	reason   tool.Reason
}{
	{snapshot.ErrNoBoundary, tool.ReasonObservationUnavailable},
	{snapshot.ErrUnknownReference, tool.ReasonReferenceUnknown},
	{snapshot.ErrExpiredReference, tool.ReasonReferenceExpired},
	{snapshot.ErrBindingMismatch, tool.ReasonBindingMismatch},
	{snapshot.ErrStaleReference, tool.ReasonHistoryGap},
	{snapshot.ErrTargetChanged, tool.ReasonTargetChanged},
	{snapshot.ErrGapChanged, tool.ReasonGapChanged},
	{snapshot.ErrPathChanged, tool.ReasonPathChanged},
}

var (
	appliedOutcome  = tool.Outcome{Status: tool.StatusApplied, Commit: tool.CommitComplete}
	noChangeOutcome = tool.Outcome{Status: tool.StatusNoChange, Commit: tool.CommitNone}
)

func outcomeOf(err error) tool.Outcome {
	reason, class := classify(err)
	return tool.Outcome{
		Status: reasonRows[reason].status, Commit: tool.CommitNone, Reason: reason, Class: class,
	}
}

func classify(err error) (tool.Reason, tool.ErrorClass) {
	var tagged *reasonError
	if errors.As(err, &tagged) {
		return tagged.reason, tagged.class
	}
	if reason, found := sentinelReason(err); found {
		return reason, reasonRows[reason].class
	}
	var styled styleError
	if errors.As(err, &styled) {
		return tool.ReasonInvalidSyntax, tool.ErrClassParse
	}
	if interruption(err) {
		return tool.ReasonInterrupted, tool.ErrClassNone
	}
	return tool.ReasonWriteFailed, tool.ErrClassIO
}

func sentinelReason(err error) (tool.Reason, bool) {
	for _, item := range sentinelReasons {
		if errors.Is(err, item.sentinel) {
			return item.reason, true
		}
	}
	return "", false
}

func interruption(err error) bool {
	return errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded)
}

func outcomeFor(commit tool.CommitState, reason tool.Reason) tool.Outcome {
	row := reasonRows[reason]
	return tool.Outcome{Status: row.status, Commit: commit, Reason: reason, Class: row.class}
}

func failedCommit(cause error, paths []tool.PathState, effects, rollbackFailures int) tool.Outcome {
	outcome := writeOutcome(paths, effects, rollbackFailures)
	if effects == 0 && interruption(cause) && outcome.Commit == tool.CommitNone {
		outcome = outcomeOf(cause)
		outcome.Paths = paths
	}
	return outcome
}

func writeOutcome(paths []tool.PathState, effects, rollbackFailures int) tool.Outcome {
	var outcome tool.Outcome
	switch counts := countStates(paths); {
	case counts.unknown > 0:
		outcome = outcomeFor(tool.CommitUnknown, tool.ReasonCommitUnknown)
	case counts.after == 0 && effects > 0:
		outcome = outcomeFor(tool.CommitRestored, tool.ReasonWriteFailed)
	case counts.after == 0:
		outcome = outcomeFor(tool.CommitNone, tool.ReasonWriteFailed)
	case counts.before == 0:
		outcome = outcomeFor(tool.CommitComplete, tool.ReasonSyncFailed)
	case rollbackFailures > 0:
		outcome = outcomeFor(tool.CommitPartial, tool.ReasonRollbackFailed)
	default:
		outcome = outcomeFor(tool.CommitPartial, tool.ReasonSyncFailed)
	}
	outcome.Paths = paths
	return outcome
}

type stateCounts struct{ before, after, unknown int }

func countStates(paths []tool.PathState) stateCounts {
	var counts stateCounts
	for _, item := range paths {
		switch item.State {
		case tool.PathBefore:
			counts.before++
		case tool.PathAfter:
			counts.after++
		default:
			counts.unknown++
		}
	}
	return counts
}

func (e Editor) refuse(env tool.Env, err error) error {
	if !e.style.profile() {
		return err
	}
	outcome := outcomeOf(err)
	outcome.Recovery = recoveryFor(env, err)
	return outcomeError(e.style, outcome, err)
}

func outcomeError(style Style, outcome tool.Outcome, err error) error {
	return &tool.OutcomeError{Outcome: stamped(style, outcome), Err: err}
}

func stamped(style Style, outcome tool.Outcome) tool.Outcome {
	outcome.Style, outcome.Version = string(style), contractVersion
	return outcome
}

func outcomeMeta(outcome tool.Outcome) jsontext.Value {
	meta, err := outcome.Meta(false)
	if err != nil {
		return nil
	}
	return meta
}
