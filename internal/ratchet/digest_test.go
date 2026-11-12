package ratchet

import (
	"testing"
	"time"
)

func TestHashPaths(t *testing.T) {
	t.Parallel()
	f := newWorkspace(t, map[string]string{"cases.txt": "a\n", "d/x": "1", "d/y": "2"})
	cases := []struct {
		name  string
		paths []string
		want  string
	}{
		{"single file", []string{"cases.txt"}, sumOf("cases.txt\x00a\n\x00")},
		{"missing path", []string{"gone.txt"}, sumOf("missing:gone.txt\x00")},
		{"directory", []string{"d"}, sumOf("d/x\x001\x00d/y\x002\x00")},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got, _, err := f.hashPaths(tc.paths)
			if err != nil {
				t.Fatal(err)
			}
			if got != tc.want {
				t.Fatalf("hashPaths(%v) = %s, want %s", tc.paths, got, tc.want)
			}
		})
	}
}

func TestPlanDigest(t *testing.T) {
	t.Parallel()
	f := newWorkspace(t, nil)
	s := &state{
		Goal:        goal{Target: "score", Direction: "higher", Hold: []string{}, Directions: map[string]string{}},
		Reps:        2,
		Stop:        stopRule{Plateau: 3, Rounds: 10},
		Command:     "run {variant}",
		ChangePaths: []string{"p.md"},
		OffLimits:   []string{},
		TrainIDs:    []string{"a", "b", "c"},
		TestIDs:     []string{"d"},
		SplitSeed:   7,
	}
	got, err := f.digest(s, "plan")
	if err != nil {
		t.Fatal(err)
	}
	want := sumOf(`{"goal":{"target":"score","direction":"higher","hold":[],"directions":{}},"reps":2,"stop":{"plateau":3,"rounds":10},"command":"run {variant}","change_paths":["p.md"],"off_limits":[],"train_ids":["a","b","c"],"test_ids":["d"],"split_seed":7}`)
	if got != want {
		t.Fatalf("plan digest = %s, want %s", got, want)
	}
}

func TestPlanDigestNeedsPlan(t *testing.T) {
	t.Parallel()
	f := newWorkspace(t, nil)
	if _, err := f.digest(&state{Goal: goal{Target: "score"}}, "plan"); err == nil || err.Error() != "plan needs a target, a command and a split first" {
		t.Fatalf("plan digest without a split = %v, want the plan refusal", err)
	}
}

func TestApprovalGoesStale(t *testing.T) {
	t.Parallel()
	f := newWorkspace(t, map[string]string{"cases.txt": "a\n"})
	s := &state{CasesPaths: []string{"cases.txt"}}
	digest, err := f.digest(s, "inputs")
	if err != nil {
		t.Fatal(err)
	}
	s.Approvals = map[string]approval{"inputs": {SHA: digest, At: time.Now().UTC()}}
	status, err := f.approvals(s)
	if err != nil {
		t.Fatal(err)
	}
	if status["inputs"] != "current" || status["grader"] != "missing" || status["plan"] != "missing" {
		t.Fatalf("approvals before edit = %v, want inputs current and the rest missing", status)
	}
	err = f.root.WriteFile("cases.txt", []byte("b\n"), 0o600)
	if err != nil {
		t.Fatal(err)
	}
	status, err = f.approvals(s)
	if err != nil {
		t.Fatal(err)
	}
	if status["inputs"] != "stale" {
		t.Fatalf("approvals after edit = %v, want inputs stale", status)
	}
}
