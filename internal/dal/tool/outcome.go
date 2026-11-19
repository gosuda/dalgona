package tool

import (
	"fmt"
	"slices"
)

type Status string

const (
	StatusApplied       Status = "applied"
	StatusNoChange      Status = "no_change"
	StatusNeedsContext  Status = "needs_context"
	StatusConflict      Status = "conflict"
	StatusRejected      Status = "rejected"
	StatusInterrupted   Status = "interrupted"
	StatusWriteError    Status = "write_error"
	StatusIndeterminate Status = "indeterminate"
)

type CommitState string

const (
	CommitNone     CommitState = "none"
	CommitRestored CommitState = "restored"
	CommitComplete CommitState = "complete"
	CommitPartial  CommitState = "partial"
	CommitUnknown  CommitState = "unknown"
)

type ErrorClass string

const (
	ErrClassNone    ErrorClass = ""
	ErrClassProof   ErrorClass = "proof"
	ErrClassStale   ErrorClass = "stale"
	ErrClassParse   ErrorClass = "parse"
	ErrClassResolve ErrorClass = "resolve"
	ErrClassFile    ErrorClass = "file"
	ErrClassBlocked ErrorClass = "blocked"
	ErrClassLimit   ErrorClass = "limit"
	ErrClassIO      ErrorClass = "io"
)

type Reason string

const (
	ReasonReferenceUnknown       Reason = "reference_unknown"
	ReasonReferenceExpired       Reason = "reference_expired"
	ReasonHistoryGap             Reason = "history_gap"
	ReasonUnobservedRange        Reason = "unobserved_range"
	ReasonObservationUnavailable Reason = "observation_unavailable"
	ReasonObserverBlocked        Reason = "observer_blocked"
	ReasonNoReplaceUnavailable   Reason = "no_replace_unavailable"
	ReasonBindingMismatch        Reason = "binding_mismatch"
	ReasonOutsideWorkspace       Reason = "outside_workspace"
	ReasonPermissionDenied       Reason = "permission_denied"
	ReasonTargetChanged          Reason = "target_changed"
	ReasonGapChanged             Reason = "gap_changed"
	ReasonPathChanged            Reason = "path_changed"
	ReasonInvalidSyntax          Reason = "invalid_syntax"
	ReasonInvalidRange           Reason = "invalid_range"
	ReasonMixedReferences        Reason = "mixed_references"
	ReasonOverlap                Reason = "overlap"
	ReasonInvalidShape           Reason = "invalid_shape"
	ReasonResourceLimit          Reason = "resource_limit"
	ReasonInterrupted            Reason = "interrupted"
	ReasonWriteFailed            Reason = "write_failed"
	ReasonRollbackFailed         Reason = "rollback_failed"
)

type PathState struct {
	Path  string
	State string
}

type LineRange struct{ First, Last int }

type Recovery struct {
	Kind      string
	Path      string
	Ranges    []LineRange
	Mapping   string
	Remaining []LineRange
}

type Outcome struct {
	Style    string
	Version  int
	Status   Status
	Commit   CommitState
	Reason   Reason
	Class    ErrorClass
	Paths    []PathState
	Recovery *Recovery
}

var commitStatesOf = map[Status][]CommitState{
	StatusApplied:       {CommitComplete},
	StatusNoChange:      {CommitNone},
	StatusNeedsContext:  {CommitNone},
	StatusConflict:      {CommitNone},
	StatusRejected:      {CommitNone},
	StatusInterrupted:   {CommitNone},
	StatusWriteError:    {CommitNone, CommitRestored, CommitComplete, CommitPartial},
	StatusIndeterminate: {CommitUnknown},
}

func (o Outcome) Valid() error {
	if !slices.Contains(commitStatesOf[o.Status], o.Commit) {
		return fmt.Errorf("tool: status %q cannot have commit state %q", o.Status, o.Commit)
	}
	quiet := o.Status == StatusApplied || o.Status == StatusNoChange
	if quiet && o.Reason != "" {
		return fmt.Errorf("tool: status %q cannot have reason %q", o.Status, o.Reason)
	}
	if !quiet && o.Reason == "" {
		return fmt.Errorf("tool: status %q needs a reason", o.Status)
	}
	return nil
}

type OutcomeError struct {
	Outcome Outcome
	Err     error
}

func (e *OutcomeError) Error() string { return e.Err.Error() }

func (e *OutcomeError) Unwrap() error { return e.Err }
