package ratchet

import (
	"slices"
	"testing"
)

func TestInitRefusals(t *testing.T) {
	t.Parallel()
	files := map[string]string{"cases.txt": "a\n", "harness.py": "print(1)\n", "p.md": "x\n", "eval/cases.jsonl": "{}\n"}
	cases := []struct {
		name string
		in   initArgs
		want string
	}{
		{"no cases", initArgs{Harness: []string{"harness.py"}, Change: []string{"p.md"}}, "init needs at least one cases path"},
		{"no harness", initArgs{Cases: []string{"cases.txt"}, Change: []string{"p.md"}}, "init needs at least one harness path"},
		{"no change", initArgs{Cases: []string{"cases.txt"}, Harness: []string{"harness.py"}}, "init needs at least one change path"},
		{"path outside workspace", initArgs{Cases: []string{"../x"}, Harness: []string{"harness.py"}, Change: []string{"p.md"}}, "path must be inside the workspace: ../x"},
		{"missing cases", initArgs{Cases: []string{"nope.txt"}, Harness: []string{"harness.py"}, Change: []string{"p.md"}}, "path does not exist: nope.txt"},
		{"change overlaps cases", initArgs{Cases: []string{"eval/cases.jsonl"}, Harness: []string{"harness.py"}, Change: []string{"eval"}}, "change path eval overlaps eval/cases.jsonl; cases, harness and off-limits paths must stay out of the tuned surface"},
		{"off-limits in flow directory", initArgs{Cases: []string{"cases.txt"}, Harness: []string{"harness.py"}, Change: []string{"p.md"}, OffLimits: []string{".dalgona/ratchet/x"}}, "path must stay outside .dalgona/ratchet: .dalgona/ratchet/x"},
		{"change is the workspace root", initArgs{Cases: []string{"cases.txt"}, Harness: []string{"harness.py"}, Change: []string{"."}}, "path must name a file or directory below the workspace root: ."},
		{"cases is the workspace root", initArgs{Cases: []string{"./"}, Harness: []string{"harness.py"}, Change: []string{"p.md"}}, "path must name a file or directory below the workspace root: ./"},
		{"off-limits is the workspace root", initArgs{Cases: []string{"cases.txt"}, Harness: []string{"harness.py"}, Change: []string{"p.md"}, OffLimits: []string{"a/.."}}, "path must name a file or directory below the workspace root: a/.."},
		{"cases contain the flow root", initArgs{Cases: []string{".dalgona"}, Harness: []string{"harness.py"}, Change: []string{"p.md"}}, "path must stay outside .dalgona/ratchet: .dalgona"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			f := newWorkspace(t, files)
			var s state
			err := f.init(&s, tc.in)
			if err == nil || err.Error() != tc.want {
				t.Fatalf("init error = %v, want %q", err, tc.want)
			}
		})
	}
}

func TestInitRecordsPaths(t *testing.T) {
	t.Parallel()
	f := newWorkspace(t, map[string]string{"cases.txt": "a\n", "harness.py": "x\n", "p.md": "y\n"})
	var s state
	in := initArgs{Cases: []string{"cases.txt"}, Harness: []string{"harness.py"}, Change: []string{"p.md"}, OffLimits: []string{"notes.md"}}
	if err := f.init(&s, in); err != nil {
		t.Fatal(err)
	}
	if s.Flow != "live" || !slices.Equal(s.CasesPaths, []string{"cases.txt"}) || !slices.Equal(s.HarnessPaths, []string{"harness.py"}) ||
		!slices.Equal(s.ChangePaths, []string{"p.md"}) || !slices.Equal(s.OffLimits, []string{"notes.md"}) {
		t.Fatalf("state after init = %+v, want the given paths", s)
	}
}

func TestPlanRefusals(t *testing.T) {
	t.Parallel()
	nine := []string{"m1", "m2", "m3", "m4", "m5", "m6", "m7", "m8", "m9"}
	cases := []struct {
		name  string
		state state
		plan  planArgs
		want  string
	}{
		{"hold includes target", state{}, planArgs{Target: "score", Hold: []string{"score"}}, "hold must not include the target metric"},
		{"hold too long", state{}, planArgs{Target: "score", Hold: nine}, "hold takes at most 8 metrics"},
		{"reps zero", state{}, planArgs{Target: "score", Reps: new(0)}, "reps must be from 1 to 20"},
		{"reps too large", state{}, planArgs{Target: "score", Reps: new(21)}, "reps must be from 1 to 20"},
		{"reps shrink after baseline", state{Reps: 2, Rounds: []round{{Variant: "baseline"}}}, planArgs{Target: "score", Reps: new(1)}, "reps cannot shrink after the baseline"},
		{"plateau zero", state{}, planArgs{Target: "score", Plateau: new(0)}, "plateau must be from 2 to 10"},
		{"plateau too small", state{}, planArgs{Target: "score", Plateau: new(1)}, "plateau must be from 2 to 10"},
		{"plateau too large", state{}, planArgs{Target: "score", Plateau: new(11)}, "plateau must be from 2 to 10"},
		{"rounds zero", state{}, planArgs{Target: "score", Rounds: new(0)}, "rounds must be from 1 to 50"},
		{"rounds too large", state{}, planArgs{Target: "score", Rounds: new(51)}, "rounds must be from 1 to 50"},
		{"command without variant", state{}, planArgs{Target: "score", Command: "run.sh"}, "command must contain {variant}"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			s := tc.state
			err := s.applyPlan(tc.plan)
			if err == nil || err.Error() != tc.want {
				t.Fatalf("applyPlan error = %v, want %q", err, tc.want)
			}
		})
	}
}

func TestRefusedPlanLeavesStateUnchanged(t *testing.T) {
	t.Parallel()
	s := state{Reps: 3}
	err := s.applyPlan(planArgs{Target: "score", Plateau: new(0)})
	if err == nil || err.Error() != "plateau must be from 2 to 10" {
		t.Fatalf("applyPlan error = %v, want the plateau refusal", err)
	}
	if s.Goal.Target != "" || s.Reps != 3 {
		t.Fatalf("refused plan changed the state: goal %q reps %d", s.Goal.Target, s.Reps)
	}
}

func TestPlanDefaults(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name      string
		plan      planArgs
		direction string
	}{
		{"latency defaults to lower", planArgs{Target: "latency_s"}, "lower"},
		{"score defaults to higher", planArgs{Target: "score"}, "higher"},
		{"given direction wins", planArgs{Target: "latency_s", Direction: "higher"}, "higher"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			var s state
			if err := s.applyPlan(tc.plan); err != nil {
				t.Fatal(err)
			}
			if s.Goal.Direction != tc.direction {
				t.Fatalf("direction = %s, want %s", s.Goal.Direction, tc.direction)
			}
			if s.Reps != 2 || s.Stop.Plateau != 3 || s.Stop.Rounds != 10 {
				t.Fatalf("defaults reps %d plateau %d rounds %d, want 2, 3 and 10", s.Reps, s.Stop.Plateau, s.Stop.Rounds)
			}
		})
	}
}
