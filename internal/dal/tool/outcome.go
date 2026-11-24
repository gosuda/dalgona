package tool

import (
	"bytes"
	"encoding/json/jsontext"
	"errors"
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
	ReasonSyncFailed             Reason = "sync_failed"
	ReasonCommitUnknown          Reason = "commit_unknown"
	ReasonInvocationMismatch     Reason = "invocation_mismatch"
)

const (
	PathBefore  = "before"
	PathAfter   = "after"
	PathUnknown = "unknown"
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

func PathStateOf(current, before, after string, readable bool) string {
	switch {
	case !readable:
		return PathUnknown
	case current == before:
		return PathBefore
	case current == after:
		return PathAfter
	}
	return PathUnknown
}

type OutcomeError struct {
	Outcome Outcome
	Err     error
}

func (e *OutcomeError) Error() string { return e.Err.Error() }

func (e *OutcomeError) Unwrap() error { return e.Err }

const MaxOutcomeMetaBytes = 48 << 10

type Profile struct {
	Style   string
	Version int
}

type Profiled interface {
	Profile() (Profile, bool)
}

func (o Outcome) Meta(replayed bool) (jsontext.Value, error) {
	if err := o.Valid(); err != nil {
		return nil, err
	}
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	err := errors.Join(
		enc.WriteToken(jsontext.BeginObject),
		enc.WriteToken(jsontext.String("hashline")),
		o.writeObject(enc, replayed),
		enc.WriteToken(jsontext.EndObject),
	)
	if err != nil {
		return nil, fmt.Errorf("tool: encode outcome meta: %w", err)
	}
	meta := bytes.TrimSuffix(buf.Bytes(), []byte("\n"))
	if len(meta) >= MaxOutcomeMetaBytes {
		return nil, fmt.Errorf("tool: outcome meta is %d bytes, not under %d", len(meta), MaxOutcomeMetaBytes)
	}
	return jsontext.Value(meta), nil
}

func (o Outcome) writeObject(enc *jsontext.Encoder, replayed bool) error {
	return errors.Join(
		enc.WriteToken(jsontext.BeginObject),
		writeKey(enc, "contract_version"), enc.WriteToken(jsontext.Int(int64(o.Version))),
		writeKey(enc, "style"), enc.WriteToken(jsontext.String(o.Style)),
		writeKey(enc, "status"), enc.WriteToken(jsontext.String(string(o.Status))),
		writeKey(enc, "commit_state"), enc.WriteToken(jsontext.String(string(o.Commit))),
		writeKey(enc, "reason"), writeOptional(enc, string(o.Reason)),
		writeKey(enc, "error_class"), writeOptional(enc, string(o.Class)),
		writeKey(enc, "replayed"), enc.WriteToken(jsontext.Bool(replayed)),
		writeKey(enc, "recovery"), writeRecovery(enc, o.Recovery),
		writeKey(enc, "path_states"), writePathStates(enc, o.Paths),
		enc.WriteToken(jsontext.EndObject),
	)
}

func writeKey(enc *jsontext.Encoder, name string) error {
	return enc.WriteToken(jsontext.String(name))
}

func writeOptional(enc *jsontext.Encoder, value string) error {
	if value == "" {
		return enc.WriteToken(jsontext.Null)
	}
	return enc.WriteToken(jsontext.String(value))
}

func writeRecovery(enc *jsontext.Encoder, recovery *Recovery) error {
	if recovery == nil {
		return enc.WriteToken(jsontext.Null)
	}
	return errors.Join(
		enc.WriteToken(jsontext.BeginObject),
		writeKey(enc, "kind"), enc.WriteToken(jsontext.String(recovery.Kind)),
		writeKey(enc, "path"), enc.WriteToken(jsontext.String(recovery.Path)),
		writeKey(enc, "ranges"), writeRanges(enc, recovery.Ranges),
		writeKey(enc, "mapping"), enc.WriteToken(jsontext.String(recovery.Mapping)),
		writeKey(enc, "remaining"), writeRanges(enc, recovery.Remaining),
		enc.WriteToken(jsontext.EndObject),
	)
}

func writeRanges(enc *jsontext.Encoder, ranges []LineRange) error {
	err := enc.WriteToken(jsontext.BeginArray)
	for _, item := range ranges {
		err = errors.Join(err,
			enc.WriteToken(jsontext.BeginObject),
			writeKey(enc, "first"), enc.WriteToken(jsontext.Int(int64(item.First))),
			writeKey(enc, "last"), enc.WriteToken(jsontext.Int(int64(item.Last))),
			enc.WriteToken(jsontext.EndObject),
		)
	}
	return errors.Join(err, enc.WriteToken(jsontext.EndArray))
}

func writePathStates(enc *jsontext.Encoder, paths []PathState) error {
	err := enc.WriteToken(jsontext.BeginArray)
	for _, item := range paths {
		err = errors.Join(err,
			enc.WriteToken(jsontext.BeginObject),
			writeKey(enc, "path"), enc.WriteToken(jsontext.String(item.Path)),
			writeKey(enc, "state"), enc.WriteToken(jsontext.String(item.State)),
			enc.WriteToken(jsontext.EndObject),
		)
	}
	return errors.Join(err, enc.WriteToken(jsontext.EndArray))
}
