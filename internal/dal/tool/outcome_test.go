package tool

import (
	"slices"
	"testing"
)

type outcomePair struct {
	status Status
	commit CommitState
}

var c13Pairs = []outcomePair{
	{StatusApplied, CommitComplete},
	{StatusNoChange, CommitNone},
	{StatusNeedsContext, CommitNone},
	{StatusConflict, CommitNone},
	{StatusRejected, CommitNone},
	{StatusInterrupted, CommitNone},
	{StatusWriteError, CommitNone},
	{StatusWriteError, CommitRestored},
	{StatusWriteError, CommitComplete},
	{StatusWriteError, CommitPartial},
	{StatusIndeterminate, CommitUnknown},
}

var (
	allStatuses = []Status{
		StatusApplied, StatusNoChange, StatusNeedsContext, StatusConflict,
		StatusRejected, StatusInterrupted, StatusWriteError, StatusIndeterminate,
	}
	allCommitStates = []CommitState{CommitNone, CommitRestored, CommitComplete, CommitPartial, CommitUnknown}
)

func quietStatus(status Status) bool {
	return status == StatusApplied || status == StatusNoChange
}

func reasonFor(status Status) Reason {
	if quietStatus(status) {
		return ""
	}
	return ReasonInvalidSyntax
}

func TestOutcomePairsMatchC13(t *testing.T) {
	t.Run("C13/eleven pairs", requireElevenPairs)
	t.Run("C13/reason on a quiet status", requireNoReasonOnQuietStatus)
	t.Run("C13/null reason on a status that needs one", requireReasonOnOtherStatus)
}

func requireElevenPairs(t *testing.T) {
	if len(c13Pairs) != 11 {
		t.Fatalf("fixture lists %d pairs, C13 allows eleven", len(c13Pairs))
	}
	for _, status := range allStatuses {
		for _, commit := range allCommitStates {
			want := slices.Contains(c13Pairs, outcomePair{status, commit})
			err := Outcome{Status: status, Commit: commit, Reason: reasonFor(status)}.Valid()
			if (err == nil) != want {
				t.Errorf("%s/%s valid = %v, want %v", status, commit, err == nil, want)
			}
		}
	}
}

func requireNoReasonOnQuietStatus(t *testing.T) {
	for _, item := range c13Pairs {
		if !quietStatus(item.status) {
			continue
		}
		if err := (Outcome{Status: item.status, Commit: item.commit, Reason: ReasonOverlap}).Valid(); err == nil {
			t.Errorf("%s with a reason is valid", item.status)
		}
	}
}

func requireReasonOnOtherStatus(t *testing.T) {
	for _, item := range c13Pairs {
		if quietStatus(item.status) {
			continue
		}
		if err := (Outcome{Status: item.status, Commit: item.commit}).Valid(); err == nil {
			t.Errorf("%s/%s without a reason is valid", item.status, item.commit)
		}
	}
}
