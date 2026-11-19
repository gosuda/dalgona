package patch

import (
	"context"
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

func commitOutcome(effects, rollbackFailures int) tool.Outcome {
	switch {
	case effects == 0:
		return outcomeFor(tool.CommitNone, tool.ReasonWriteFailed)
	case rollbackFailures == 0:
		return outcomeFor(tool.CommitRestored, tool.ReasonWriteFailed)
	}
	return outcomeFor(tool.CommitPartial, tool.ReasonRollbackFailed)
}

func outcomeFor(commit tool.CommitState, reason tool.Reason) tool.Outcome {
	row := reasonRows[reason]
	return tool.Outcome{Status: row.status, Commit: commit, Reason: reason, Class: row.class}
}

func failedCommit(cause error, effects, rollbackFailures int) tool.Outcome {
	if effects == 0 && interruption(cause) {
		return outcomeOf(cause)
	}
	return commitOutcome(effects, rollbackFailures)
}

func (e Editor) refuse(err error) error {
	if !e.style.profile() {
		return err
	}
	return outcomeError(e.style, outcomeOf(err), err)
}

func outcomeError(style Style, outcome tool.Outcome, err error) error {
	outcome.Style, outcome.Version = string(style), contractVersion
	return &tool.OutcomeError{Outcome: outcome, Err: err}
}
