package patch

import (
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/tool"
)

type metaPath struct {
	Path  string `json:"path"`
	State string `json:"state"`
}

type metaObject struct {
	Hashline struct {
		ContractVersion int            `json:"contract_version"`
		Style           string         `json:"style"`
		Status          string         `json:"status"`
		CommitState     string         `json:"commit_state"`
		Reason          *string        `json:"reason"`
		ErrorClass      *string        `json:"error_class"`
		Replayed        bool           `json:"replayed"`
		Recovery        jsontext.Value `json:"recovery"`
		PathStates      []metaPath     `json:"path_states"`
	} `json:"hashline"`
}

func decodeMeta(t *testing.T, meta jsontext.Value) metaObject {
	t.Helper()
	var object metaObject
	if err := json.Unmarshal(meta, &object); err != nil {
		t.Fatalf("meta %q: %v", meta, err)
	}
	return object
}

func requireMetaStatus(t *testing.T, meta jsontext.Value, style Style, status, commit string) metaObject {
	t.Helper()
	object := decodeMeta(t, meta)
	got := object.Hashline
	if got.ContractVersion != 1 || got.Style != string(style) || got.Status != status || got.CommitState != commit || got.Replayed {
		t.Fatalf("meta %s, want %s/%s/%s at version 1 and replayed false", meta, style, status, commit)
	}
	return object
}

func requireReason(t *testing.T, meta jsontext.Value, object metaObject, reason, class string) {
	t.Helper()
	got := object.Hashline
	if got.Reason == nil || *got.Reason != reason {
		t.Fatalf("meta %s, want reason %q", meta, reason)
	}
	if (class == "") != (got.ErrorClass == nil) || (class != "" && *got.ErrorClass != class) {
		t.Fatalf("meta %s, want class %q", meta, class)
	}
}

func applyWith(t *testing.T, style Style, input string) tool.Result {
	t.Helper()
	f := newOutcomeFixture(t)
	plan, err := f.plan(t, style, input)
	if err != nil {
		t.Fatal(err)
	}
	result, err := plan.Apply(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	return result
}

func TestProfileApplyCarriesHashlineMeta(t *testing.T) {
	for _, style := range profileStyles {
		t.Run("COMPAT-03/applied/"+string(style), func(t *testing.T) { requireAppliedMeta(t, style) })
		t.Run("COMPAT-03/no_change/"+string(style), func(t *testing.T) { requireNoChangeMeta(t, style) })
	}
}

func requireAppliedMeta(t *testing.T, style Style) {
	result := applyWith(t, style, "[f.txt@{ref}]\nPUT 1:\n+uno\n")
	object := requireMetaStatus(t, result.Meta, style, "applied", "complete")
	if object.Hashline.Reason != nil || object.Hashline.ErrorClass != nil || len(object.Hashline.PathStates) != 0 {
		t.Fatalf("applied meta %s names a reason, a class or a path", result.Meta)
	}
}

func requireNoChangeMeta(t *testing.T, style Style) {
	result := applyWith(t, style, "[f.txt@{ref}]\nPUT 1:\n+one\n")
	if string(result.Output) != "no_change\n" {
		t.Fatalf("output %q, want no_change", result.Output)
	}
	requireMetaStatus(t, result.Meta, style, "no_change", "none")
}

func TestProfileApplyFailureCarriesHashlineMeta(t *testing.T) {
	t.Run("IO-03/partial", requirePartialMeta)
	t.Run("AUTH-02/target changed while approving", requireConflictMeta)
	t.Run("AUTH-02/target vanished while approving", requireVanishedMeta)
}

func requireVanishedMeta(t *testing.T) {
	f := newOutcomeFixture(t)
	plan, err := f.plan(t, HashlineLight, "[f.txt@{ref}]\nPUT 1:\n+uno\n")
	if err != nil {
		t.Fatal(err)
	}
	if rmErr := os.Remove(filepath.Join(f.cwd, "f.txt")); rmErr != nil {
		t.Fatal(rmErr)
	}
	result, err := plan.Apply(t.Context())
	if err == nil {
		t.Fatal("apply succeeded")
	}
	if !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("vanished target error reports %v, want an fs.ErrNotExist chain", err)
	}
	object := requireMetaStatus(t, result.Meta, HashlineLight, "conflict", "none")
	requireReason(t, result.Meta, object, "path_changed", "stale")
	if _, statErr := os.Stat(filepath.Join(f.cwd, "f.txt")); !os.IsNotExist(statErr) {
		t.Fatalf("f.txt exists after the refused apply: %v", statErr)
	}
}

func requirePartialMeta(t *testing.T) {
	f := newOutcomeFixture(t)
	twoFiles(t, f)
	failRestore(t, f)
	plan, err := f.plan(t, HashlineLight, twoFileEdit)
	if err != nil {
		t.Fatal(err)
	}
	result, err := plan.Apply(t.Context())
	if err == nil {
		t.Fatal("apply succeeded")
	}
	object := requireMetaStatus(t, result.Meta, HashlineLight, "write_error", "partial")
	requireReason(t, result.Meta, object, "rollback_failed", "io")
	want := []metaPath{{Path: "a.txt", State: "after"}, {Path: "b.txt", State: "before"}}
	if got := object.Hashline.PathStates; len(got) != 2 || got[0] != want[0] || got[1] != want[1] {
		t.Fatalf("path_states %+v, want %+v", got, want)
	}
}

func requireConflictMeta(t *testing.T) {
	f := newOutcomeFixture(t)
	plan, err := f.plan(t, HashlineLight, "[f.txt@{ref}]\nPUT 1:\n+uno\n")
	if err != nil {
		t.Fatal(err)
	}
	writeFixture(t, f.cwd, "f.txt", "drift\n")
	result, err := plan.Apply(t.Context())
	if err == nil {
		t.Fatal("apply succeeded")
	}
	object := requireMetaStatus(t, result.Meta, HashlineLight, "conflict", "none")
	requireReason(t, result.Meta, object, "target_changed", "stale")
}

func TestEditorProfileNamesOnlyTheNewStyles(t *testing.T) {
	cases := []struct {
		row     string
		style   Style
		profile bool
	}{
		{"COMPAT-01", Anchor, false},
		{"COMPAT-01", Replace, false},
		{"COMPAT-01", Hashline, false},
		{"COMPAT-01", ApplyPatch, false},
		{"COMPAT-03", HashlineLight, true},
		{"COMPAT-03", HashlineEnhanced, true},
	}
	for _, test := range cases {
		t.Run(test.row+"/"+string(test.style), func(t *testing.T) {
			profiled, ok := New(string(test.style)).(tool.Profiled)
			if !ok {
				t.Fatal("the editor does not implement tool.Profiled")
			}
			profile, named := profiled.Profile()
			if named != test.profile {
				t.Fatalf("Profile ok = %v, want %v", named, test.profile)
			}
			if test.profile && (profile.Style != string(test.style) || profile.Version != 1) {
				t.Fatalf("Profile = %+v, want %s at version 1", profile, test.style)
			}
		})
	}
}

func maxCallPaths(t *testing.T) []string {
	t.Helper()
	paths := spreadPaths(64, pathTextBytes)
	total := 0
	for _, path := range paths {
		total += len(path)
	}
	if total != 16<<10 || checkPathText(paths) != nil {
		t.Fatalf("fixture paths total %d bytes or exceed the call cap", total)
	}
	return paths
}

func pathStatesOf(paths []string, state func(i int) string) []tool.PathState {
	states := make([]tool.PathState, len(paths))
	for i, path := range paths {
		states[i] = tool.PathState{Path: path, State: state(i)}
	}
	return states
}

func TestHashlineMetaStaysUnder64KiB(t *testing.T) {
	paths := maxCallPaths(t)
	t.Run("IO-06/partial outcome naming 64 paths", func(t *testing.T) { requirePartialFits(t, paths) })
	t.Run("IO-06/unknown outcome naming 64 paths", func(t *testing.T) { requireUnknownFits(t, paths) })
}

func requirePartialFits(t *testing.T, paths []string) {
	states := pathStatesOf(paths, func(i int) string {
		if i%2 == 1 {
			return tool.PathBefore
		}
		return tool.PathAfter
	})
	meta, err := stamped(HashlineLight, writeOutcome(states, len(states), 1)).Meta(false)
	if err != nil {
		t.Fatal(err)
	}
	if len(meta) >= 48<<10 {
		t.Fatalf("meta of 64 paths is %d bytes, want under 48 KiB", len(meta))
	}
	got := requireMetaStatus(t, meta, HashlineLight, "write_error", "partial").Hashline.PathStates
	if len(got) != len(states) {
		t.Fatalf("meta names %d paths, want %d", len(got), len(states))
	}
	for i, item := range got {
		if item.Path != states[i].Path || item.State != states[i].State {
			t.Fatalf("path_states[%d] = %+v, want %+v", i, item, states[i])
		}
	}
}

func requireUnknownFits(t *testing.T, paths []string) {
	states := pathStatesOf(paths, func(int) string { return tool.PathUnknown })
	meta, err := stamped(HashlineLight, writeOutcome(states, len(states), 0)).Meta(false)
	if err != nil || len(meta) >= 48<<10 {
		t.Fatalf("meta of %d bytes, %v; want an object under 48 KiB", len(meta), err)
	}
}
