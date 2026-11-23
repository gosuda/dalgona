package tool

import (
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"strings"
	"testing"
)

const (
	appliedMeta = `{"hashline":{"contract_version":1,"style":"hashline-light","status":"applied","commit_state":"complete","reason":null,"error_class":null,"replayed":false,"recovery":null,"path_states":[]}}`
	needsMeta   = `{"hashline":{"contract_version":1,"style":"hashline-enhanced","status":"needs_context","commit_state":"none","reason":"unobserved_range","error_class":"proof","replayed":false,"recovery":null,"path_states":[]}}`
	partialMeta = `{"hashline":{"contract_version":1,"style":"hashline-light","status":"write_error","commit_state":"partial","reason":"rollback_failed","error_class":"io","replayed":false,"recovery":null,"path_states":[{"path":"a.txt","state":"after"},{"path":"b.txt","state":"before"}]}}`
	recoverMeta = `{"hashline":{"contract_version":1,"style":"hashline-enhanced","status":"needs_context","commit_state":"none","reason":"unobserved_range","error_class":"proof","replayed":false,"recovery":{"kind":"read","path":"src/lib.rs","ranges":[{"first":40,"last":45}],"mapping":"known","remaining":[{"first":60,"last":61}]},"path_states":[]}}`
)

type decodedMeta struct {
	Hashline struct {
		ContractVersion int            `json:"contract_version"`
		Style           string         `json:"style"`
		Status          string         `json:"status"`
		CommitState     string         `json:"commit_state"`
		Reason          *string        `json:"reason"`
		ErrorClass      *string        `json:"error_class"`
		Replayed        bool           `json:"replayed"`
		Recovery        jsontext.Value `json:"recovery"`
		PathStates      []struct {
			Path  string `json:"path"`
			State string `json:"state"`
		} `json:"path_states"`
	} `json:"hashline"`
}

type metaCase struct {
	name    string
	want    string
	outcome Outcome
}

func metaCases() []metaCase {
	return []metaCase{
		{"applied", appliedMeta, Outcome{Style: "hashline-light", Version: 1, Status: StatusApplied, Commit: CommitComplete}},
		{"needs_context", needsMeta, Outcome{
			Style: "hashline-enhanced", Version: 1, Status: StatusNeedsContext, Commit: CommitNone,
			Reason: ReasonUnobservedRange, Class: ErrClassProof,
		}},
		{"partial", partialMeta, Outcome{
			Style: "hashline-light", Version: 1, Status: StatusWriteError, Commit: CommitPartial,
			Reason: ReasonRollbackFailed, Class: ErrClassIO,
			Paths: []PathState{{Path: "a.txt", State: PathAfter}, {Path: "b.txt", State: PathBefore}},
		}},
		{"recovery", recoverMeta, Outcome{
			Style: "hashline-enhanced", Version: 1, Status: StatusNeedsContext, Commit: CommitNone,
			Reason: ReasonUnobservedRange, Class: ErrClassProof,
			Recovery: &Recovery{
				Kind: "read", Path: "src/lib.rs", Ranges: []LineRange{{First: 40, Last: 45}},
				Mapping: "known", Remaining: []LineRange{{First: 60, Last: 61}},
			},
		}},
	}
}

func TestOutcomeMetaRoundTrip(t *testing.T) {
	for _, test := range metaCases() {
		t.Run("C15/"+test.name, func(t *testing.T) { requireMetaCase(t, test) })
	}
	t.Run("C15/replayed", requireReplayedFlag)
}

func requireMetaCase(t *testing.T, test metaCase) {
	t.Helper()
	meta, err := test.outcome.Meta(false)
	if err != nil {
		t.Fatal(err)
	}
	if string(meta) != test.want {
		t.Fatalf("meta\n%s\nwant\n%s", meta, test.want)
	}
	requireOnlyHashlineMember(t, meta)
	requireDecodes(t, meta, test.outcome)
}

func requireReplayedFlag(t *testing.T) {
	meta, err := metaCases()[0].outcome.Meta(true)
	if err != nil {
		t.Fatal(err)
	}
	if want := strings.Replace(appliedMeta, `"replayed":false`, `"replayed":true`, 1); string(meta) != want {
		t.Fatalf("replayed meta\n%s\nwant\n%s", meta, want)
	}
}

func requireOnlyHashlineMember(t *testing.T, meta jsontext.Value) {
	t.Helper()
	var top map[string]jsontext.Value
	if err := json.Unmarshal(meta, &top); err != nil {
		t.Fatal(err)
	}
	if _, ok := top["hashline"]; !ok || len(top) != 1 {
		t.Fatalf("top level holds %d members, want only hashline", len(top))
	}
}

func requireDecodes(t *testing.T, meta jsontext.Value, outcome Outcome) {
	t.Helper()
	var got decodedMeta
	if err := json.Unmarshal(meta, &got); err != nil {
		t.Fatal(err)
	}
	object := got.Hashline
	if object.ContractVersion != outcome.Version || object.Style != outcome.Style || object.Status != string(outcome.Status) {
		t.Fatalf("object %+v disagrees with %+v", object, outcome)
	}
	if object.CommitState != string(outcome.Commit) || object.Replayed || len(object.PathStates) != len(outcome.Paths) {
		t.Fatalf("object %+v disagrees with %+v", object, outcome)
	}
	requireNullMembers(t, object.Reason == nil, object.ErrorClass == nil, string(object.Recovery) == "null", outcome)
}

func requireNullMembers(t *testing.T, reason, class, recovery bool, outcome Outcome) {
	t.Helper()
	if reason != (outcome.Reason == "") || class != (outcome.Class == "") || recovery != (outcome.Recovery == nil) {
		t.Fatalf("null members of the object disagree with %+v", outcome)
	}
}

func TestOutcomeMetaRefusesAnInvalidPair(t *testing.T) {
	cases := []struct {
		name    string
		outcome Outcome
	}{
		{"reason on applied", Outcome{Style: "hashline-light", Version: 1, Status: StatusApplied, Commit: CommitComplete, Reason: ReasonOverlap}},
		{"null reason on rejected", Outcome{Style: "hashline-light", Version: 1, Status: StatusRejected, Commit: CommitNone}},
		{"applied without complete", Outcome{Style: "hashline-light", Version: 1, Status: StatusApplied, Commit: CommitNone}},
	}
	for _, test := range cases {
		t.Run("C13/"+test.name, func(t *testing.T) {
			if meta, err := test.outcome.Meta(false); err == nil {
				t.Fatalf("Meta accepted an invalid pair: %s", meta)
			}
		})
	}
}

func outcomeWithPath(path string) Outcome {
	return Outcome{
		Style: "hashline-light", Version: 1, Status: StatusWriteError, Commit: CommitPartial,
		Reason: ReasonRollbackFailed, Class: ErrClassIO, Paths: []PathState{{Path: path, State: PathAfter}},
	}
}

func TestOutcomeMetaStaysUnder48KiB(t *testing.T) {
	const limit = 48 << 10
	base, err := outcomeWithPath("x").Meta(false)
	if err != nil {
		t.Fatal(err)
	}
	fill := limit - (len(base) - 1)
	t.Run("C15/one byte under 48 KiB", func(t *testing.T) {
		meta, err := outcomeWithPath(strings.Repeat("x", fill-1)).Meta(false)
		if err != nil || len(meta) != limit-1 {
			t.Fatalf("meta of %d bytes, %v; want %d bytes and no error", len(meta), err, limit-1)
		}
	})
	for name, size := range map[string]int{"exactly 48 KiB": fill, "one byte over": fill + 1, "over the journal limit of 64 KiB": 70 << 10} {
		t.Run("C15/"+name, func(t *testing.T) {
			if meta, err := outcomeWithPath(strings.Repeat("x", size)).Meta(false); err == nil {
				t.Fatalf("meta of %d bytes accepted", len(meta))
			}
		})
	}
}
