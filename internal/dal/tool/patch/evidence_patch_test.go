package patch

import (
	"fmt"
	"math/rand/v2"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dal/tool/snapshot"
)

func TestProfileEvidenceCutoff(t *testing.T) {
	for _, style := range []Style{HashlineLight, HashlineEnhanced} {
		for _, operation := range []string{"PUT 1:\n+changed", "PUT >1:\n+added", "REM", "MV moved.txt"} {
			t.Run(string(style)+"/"+operation, func(t *testing.T) {
				testProfileEvidenceCutoff(t, style, operation)
			})
		}
	}
}

func testProfileEvidenceCutoff(t *testing.T, style Style, operation string) {
	t.Helper()
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "file.txt", "original\n")
	ref := seedReference(env, "file.txt", "original\n", true)
	env.Snapshots.Bind(ref, snapshot.ConsumerModel, 8)
	env.Evidence = snapshot.Evidence{Consumer: snapshot.ConsumerModel, Cutoff: 7}
	input := fmt.Sprintf("*** Begin Patch\n[file.txt@%s]\n%s\n*** End Patch", ref, operation)
	_, err := runPatch(t, env, style, input)
	if err == nil || !strings.Contains(err.Error(), "not eligible") {
		t.Fatalf("future delivery accepted: %v", err)
	}
	if got := readFixture(t, cwd, "file.txt"); got != "original\n" {
		t.Fatalf("refusal changed file: %q", got)
	}
	env.Evidence.Cutoff = 8
	if _, err := runPatch(t, env, style, input); err != nil {
		t.Fatalf("later request refused eligible header: %v", err)
	}
}

func TestProfileEvidenceConsumerAndBoundary(t *testing.T) {
	for _, consumer := range []string{"", "another-consumer", snapshot.ConsumerModel} {
		t.Run(consumer, func(t *testing.T) {
			env, cwd := editTestEnv(t)
			writeFixture(t, cwd, "file.txt", "original\n")
			ref := seedReference(env, "file.txt", "original\n", true)
			env.Snapshots.Bind(ref, snapshot.ConsumerModel, 8)
			env.Evidence = snapshot.Evidence{Consumer: consumer, Cutoff: 8}
			input := fmt.Sprintf("*** Begin Patch\n[file.txt@%s]\nPUT 1:\n+changed\n*** End Patch", ref)
			args := []byte(`{"input":` + snapshot.JSONString(input) + `}`)
			_, err := New(string(HashlineEnhanced)).Run(t.Context(), tool.Call{ID: "call", Args: args}, env)
			if consumer == snapshot.ConsumerModel {
				if err != nil {
					t.Fatal(err)
				}
				return
			}
			if err == nil || readFixture(t, cwd, "file.txt") != "original\n" {
				t.Fatalf("invalid consumer boundary accepted: %v", err)
			}
		})
	}
}

func TestProfileEvidenceUnboundAndUnobserved(t *testing.T) {
	for _, item := range []struct {
		name  string
		bound bool
		want  string
	}{
		{name: "unbound", want: "not eligible"},
		{name: "bound", bound: true, want: "need prior observation"},
	} {
		t.Run(item.name, func(t *testing.T) {
			env, cwd := editTestEnv(t)
			writeFixture(t, cwd, "file.txt", "original\n")
			ref := seedReference(env, "file.txt", "original\n", false)
			if item.bound {
				env.Snapshots.Bind(ref, snapshot.ConsumerModel, 8)
			}
			env.Evidence = snapshot.Evidence{Consumer: snapshot.ConsumerModel, Cutoff: 8}
			input := fmt.Sprintf("*** Begin Patch\n[file.txt@%s]\nPUT 1:\n+changed\n*** End Patch", ref)
			_, err := runPatch(t, env, HashlineEnhanced, input)
			if err == nil || readFixture(t, cwd, "file.txt") != "original\n" || !strings.Contains(err.Error(), item.want) {
				t.Fatalf("evidence refusal = %v, want %q", err, item.want)
			}
		})
	}
}

func TestProfileEvidenceDeliveryInterleavings(t *testing.T) {
	rng := rand.New(rand.NewPCG(41, 97))
	for i := range 64 {
		t.Run(fmt.Sprint(i), func(t *testing.T) {
			env, cwd := editTestEnv(t)
			writeFixture(t, cwd, "file.txt", "original\n")
			ref := seedReference(env, "file.txt", "original\n", true)
			delivery, cutoff := 1+rng.Uint64N(12), 1+rng.Uint64N(12)
			env.Snapshots.Bind(ref, snapshot.ConsumerModel, delivery)
			env.Evidence = snapshot.Evidence{Consumer: snapshot.ConsumerModel, Cutoff: cutoff}
			input := profileInput("file.txt", ref, "PUT 1:\n+changed\n")
			_, err := runPatch(t, env, HashlineEnhanced, input)
			want := "original\n"
			if delivery <= cutoff {
				want = "changed\n"
				if err != nil {
					t.Fatalf("delivery %d before cutoff %d refused: %v", delivery, cutoff, err)
				}
			} else if err == nil {
				t.Fatalf("delivery %d after cutoff %d accepted", delivery, cutoff)
			}
			if got := readFixture(t, cwd, "file.txt"); got != want {
				t.Fatalf("file = %q, want %q", got, want)
			}
		})
	}
}
