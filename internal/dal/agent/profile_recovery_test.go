package agent

import (
	"strings"
	"testing"
)

const (
	sourceFile        = "alpha-1\nTOKEN=abc\nalpha-3\nalpha-4\nalpha-5\n"
	enhancedRefusal   = "need prior observation. Read them, then resend"
	unseenLinesRecord = `{"kind":"read","path":"a.txt","ranges":[{"first":2,"last":5}],"mapping":"known","remaining":[]}`
)

func TestUnobservedRangeOutputEndsWithTheSentence(t *testing.T) {
	t.Run("E-02 E05 OBS-03/through the approval gate", func(t *testing.T) {
		t.Parallel()
		h := newProfileHarness(t, "hashline-enhanced")
		h.write("a.txt", sourceFile)
		events := h.runRounds(
			calls(readCall("r1", `{"path":"a.txt","limit":1}`)),
			calls(patchCall("p1", "[a.txt@{ref:a.txt}]\nPUT 2-5:\n+x\n")),
		)
		got := completedOutputs(events)["p1"]
		want := "patch: changes[0]: lines 2-5 of a.txt " + enhancedRefusal
		if got.OK || got.Output != want {
			t.Fatalf("completion = %+v, want ok=false output %q", got, want)
		}
		if strings.Contains(got.Output, "alpha") || strings.Contains(got.Output, "[a.txt@") {
			t.Fatalf("output %q shows a file line or binds a header", got.Output)
		}
		requireHashlineRecovery(t, got.Meta, hashlineWant{"hashline-enhanced", "needs_context", "none", "unobserved_range", "proof"}, unseenLinesRecord)
		if recovery := decodeHashline(t, got.Meta).Recovery; !strings.HasPrefix(string(recovery), `{"kind":"read"`) {
			t.Fatalf("recovery %s, want a read instruction", recovery)
		}
	})
}

func TestRefusalCarriesNoSourceRow(t *testing.T) {
	t.Run("C15/read permission", func(t *testing.T) {
		t.Parallel()
		h := newProfileHarness(t, "hashline-enhanced")
		h.write("a.txt", sourceFile)
		events := h.runRounds(
			calls(readCall("r1", `{"path":"a.txt","limit":1}`)),
			calls(patchCall("p1", "[a.txt@{ref:a.txt}]\nPUT 2-5:\n+x\n")),
		)
		got := completedOutputs(events)["p1"]
		if got.OK || !strings.HasSuffix(got.Output, enhancedRefusal) {
			t.Fatalf("completion = %+v, want the unobserved-range refusal", got)
		}
		if strings.Contains(got.Output, "TOKEN=abc") || strings.Contains(string(got.Meta), "TOKEN=abc") {
			t.Fatalf("refusal holds a source line: output %q meta %s", got.Output, got.Meta)
		}
	})
}
