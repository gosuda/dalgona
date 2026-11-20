package patch

import (
	"context"
	"errors"
	"reflect"
	"slices"
	"strconv"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dal/tool/snapshot"
)

func numberedLines(count int) string {
	var out strings.Builder
	for i := 1; i <= count; i++ {
		out.WriteString("l" + strconv.Itoa(i) + "\n")
	}
	return out.String()
}

func lineRanges(bounds ...int) []tool.LineRange {
	var out []tool.LineRange
	for i := 0; i+1 < len(bounds); i += 2 {
		out = append(out, tool.LineRange{First: bounds[i], Last: bounds[i+1]})
	}
	return out
}

func readUnknown(path string) *tool.Recovery {
	return &tool.Recovery{Kind: "read", Path: path, Mapping: "unknown"}
}

func knownRead(path string, ranges, remaining []tool.LineRange) *tool.Recovery {
	return &tool.Recovery{Kind: "read", Path: path, Ranges: ranges, Mapping: "known", Remaining: remaining}
}

func withRecovery(outcome tool.Outcome, recovery *tool.Recovery) tool.Outcome {
	outcome.Recovery = recovery
	return outcome
}

func refusalOf(t *testing.T, env tool.Env, style Style, input string) *tool.OutcomeError {
	t.Helper()
	_, err := runPatch(t, env, style, input)
	var failure *tool.OutcomeError
	if !errors.As(err, &failure) {
		t.Fatalf("error %v carries no outcome", err)
	}
	return failure
}

func requireRecovery(t *testing.T, failure *tool.OutcomeError, want *tool.Recovery) {
	t.Helper()
	if !reflect.DeepEqual(failure.Outcome.Recovery, want) {
		t.Fatalf("recovery %+v, want %+v (error %q)", failure.Outcome.Recovery, want, failure.Err)
	}
}

func referenceSequence(t *testing.T, ref string) uint64 {
	t.Helper()
	_, seq, ok := strings.Cut(ref, ".")
	if !ok {
		t.Fatalf("reference %q has no sequence", ref)
	}
	value, err := strconv.ParseUint(seq, 36, 64)
	if err != nil {
		t.Fatal(err)
	}
	return value
}

func TestRecoveryRangesAreBoundedTo40Lines(t *testing.T) {
	t.Run("OBS-10/one hundred unseen lines", func(t *testing.T) {
		env, cwd := editTestEnv(t)
		source := numberedLines(100)
		writeFixture(t, cwd, "f.txt", source)
		ref := env.Snapshots.Put("f.txt", []byte(source), nil)
		failure := refusalOf(t, env, HashlineEnhanced, profileInput("f.txt", ref, "PUT 1-100:\n+x\n"))
		requireRecovery(t, failure, knownRead("f.txt", lineRanges(1, 40), lineRanges(41, 100)))
	})
	t.Run("C15/ranges follow a cooperating prefix insertion", func(t *testing.T) {
		env, cwd := editTestEnv(t)
		source := numberedLines(100)
		writeFixture(t, cwd, "f.txt", source)
		ref := env.Snapshots.Put("f.txt", []byte(source), nil)
		if _, err := runPatch(t, env, HashlineLight, profileInput("f.txt", ref, "PUT <1:\n+a\n+b\n+c\n")); err != nil {
			t.Fatal(err)
		}
		failure := refusalOf(t, env, HashlineEnhanced, profileInput("f.txt", ref, "PUT 1-100:\n+x\n"))
		requireRecovery(t, failure, knownRead("f.txt", lineRanges(4, 43), lineRanges(44, 103)))
	})
	t.Run("INV-COVERAGE/a range that crosses the 40th line is cut there", func(t *testing.T) {
		env, cwd := editTestEnv(t)
		source := numberedLines(100)
		writeFixture(t, cwd, "f.txt", source)
		shown := []snapshot.Range{{Start: 1, End: 10}, {Start: 31, End: 40}}
		ref := env.Snapshots.Put("f.txt", []byte(source), shown)
		failure := refusalOf(t, env, HashlineEnhanced, profileInput("f.txt", ref, "PUT 1-100:\n+x\n"))
		requireRecovery(t, failure, knownRead("f.txt", lineRanges(11, 30, 41, 60), lineRanges(61, 100)))
	})
	t.Run("OBS-10/one row of 20 KiB is listed and nothing is captured", func(t *testing.T) {
		env, cwd := editTestEnv(t)
		source := "a\n" + strings.Repeat("x", 20<<10) + "\nc\n"
		writeFixture(t, cwd, "f.txt", source)
		shown := []snapshot.Range{{Start: 1, End: 1}, {Start: 3, End: 3}}
		ref := env.Snapshots.Put("f.txt", []byte(source), shown)
		first := env.Snapshots.Put("probe.txt", []byte("p\n"), nil)
		failure := refusalOf(t, env, HashlineEnhanced, profileInput("f.txt", ref, "PUT 2:\n+y\n"))
		second := env.Snapshots.Put("probe.txt", []byte("p\n"), nil)
		requireRecovery(t, failure, knownRead("f.txt", lineRanges(2, 2), nil))
		if got, err := env.Snapshots.Shown(ref); err != nil || !slices.Equal(got, shown) {
			t.Fatalf("Shown = %v, %v; want the unchanged %v", got, err, shown)
		}
		if referenceSequence(t, second) != referenceSequence(t, first)+1 {
			t.Fatalf("references %s and %s are not consecutive: the refusal captured an entry", first, second)
		}
	})
}

func TestRecoveryRemainingIsCappedAt64Ranges(t *testing.T) {
	t.Run("C15/200 alternating rows", func(t *testing.T) {
		env, cwd := editTestEnv(t)
		source := numberedLines(400)
		writeFixture(t, cwd, "f.txt", source)
		var shown []snapshot.Range
		for line := 2; line <= 400; line += 2 {
			shown = append(shown, snapshot.Range{Start: line, End: line})
		}
		ref := env.Snapshots.Put("f.txt", []byte(source), shown)
		failure := refusalOf(t, env, HashlineEnhanced, profileInput("f.txt", ref, "PUT 1-400:\n+x\n"))
		var wantRanges, wantRemaining []tool.LineRange
		for k := range 40 {
			wantRanges = append(wantRanges, tool.LineRange{First: 2*k + 1, Last: 2*k + 1})
		}
		for k := 40; k < 103; k++ {
			wantRemaining = append(wantRemaining, tool.LineRange{First: 2*k + 1, Last: 2*k + 1})
		}
		wantRemaining = append(wantRemaining, tool.LineRange{First: 207, Last: 399})
		requireRecovery(t, failure, knownRead("f.txt", wantRanges, wantRemaining))
		if got := len(failure.Outcome.Recovery.Remaining); got != 64 {
			t.Fatalf("remaining holds %d ranges, want 64", got)
		}
	})
}

func requestedCoverage(ref string, first, last int, before []byte) error {
	return &coverageError{
		err:  reasoned(tool.ReasonUnobservedRange, errors.New("patch: coverage refused")),
		path: "f.txt", ref: ref, first: first, last: last, before: before,
	}
}

func TestRecoveryMappingUnknownPresentsNoCoordinates(t *testing.T) {
	const emptyMembers = `"recovery":{"kind":"read","path":"f.txt","ranges":[],"mapping":"unknown","remaining":[]}`
	t.Run("C15/history gap", func(t *testing.T) {
		env, cwd := editTestEnv(t)
		writeFixture(t, cwd, "f.txt", outcomeSource)
		ref := seedReference(env, "f.txt", outcomeSource, true)
		writeFixture(t, cwd, "f.txt", "one\nTWO\nthree\n")
		failure := refusalOf(t, env, HashlineEnhanced, profileInput("f.txt", ref, "PUT 1:\n+x\n"))
		if failure.Outcome.Reason != tool.ReasonHistoryGap {
			t.Fatalf("reason %q, want history_gap", failure.Outcome.Reason)
		}
		requireRecovery(t, failure, readUnknown("f.txt"))
		meta, err := failure.Outcome.Meta(false)
		if err != nil || !strings.Contains(string(meta), emptyMembers) {
			t.Fatalf("meta %s, %v; want %s", meta, err, emptyMembers)
		}
	})
	t.Run("C15/coverage refusal whose lineage no longer maps", func(t *testing.T) {
		env, _ := editTestEnv(t)
		ref := env.Snapshots.Put("f.txt", []byte(outcomeSource), []snapshot.Range{{Start: 1, End: 1}})
		got := recoveryFor(env, requestedCoverage(ref, 2, 3, []byte("changed\n")))
		if !reflect.DeepEqual(got, readUnknown("f.txt")) {
			t.Fatalf("recovery %+v, want %+v", got, readUnknown("f.txt"))
		}
	})
	t.Run("C15/one of two uncovered ranges no longer maps", func(t *testing.T) {
		env, _ := editTestEnv(t)
		before, after := []byte("a\nb\nc\nd\n"), []byte("A\nb\nc\nd\n")
		ref := env.Snapshots.Put("f.txt", before, []snapshot.Range{{Start: 2, End: 2}})
		env.Snapshots.Record("f.txt", before, after, []snapshot.Splice{{Start: 0, End: 1, Inserted: 1}})
		got := recoveryFor(env, requestedCoverage(ref, 1, 4, after))
		if !reflect.DeepEqual(got, readUnknown("f.txt")) {
			t.Fatalf("recovery %+v, want %+v", got, readUnknown("f.txt"))
		}
	})
	t.Run("C15/nothing is left uncovered", func(t *testing.T) {
		env, _ := editTestEnv(t)
		ref := env.Snapshots.Put("f.txt", []byte(outcomeSource), []snapshot.Range{{Start: 1, End: 3}})
		got := recoveryFor(env, requestedCoverage(ref, 1, 3, []byte(outcomeSource)))
		if !reflect.DeepEqual(got, readUnknown("f.txt")) {
			t.Fatalf("recovery %+v, want %+v", got, readUnknown("f.txt"))
		}
	})
	t.Run("C15/no store", func(t *testing.T) {
		got := recoveryFor(tool.Env{}, requestedCoverage("r", 2, 3, []byte(outcomeSource)))
		if !reflect.DeepEqual(got, readUnknown("f.txt")) {
			t.Fatalf("recovery %+v, want %+v", got, readUnknown("f.txt"))
		}
	})
}

func TestGapCoverageRefusalNamesTheSnapshotRow(t *testing.T) {
	tests := []struct {
		name        string
		cooperating string
		tail        string
		want        []tool.LineRange
	}{
		{"control PUT >3", "", "PUT >3:\n+four\n", lineRanges(3, 3)},
		{"control PUT >$", "", "PUT >$:\n+four\n", lineRanges(3, 3)},
		{"CUT 1 then PUT >3", "CUT 1\n", "PUT >3:\n+four\n", lineRanges(2, 2)},
		{"CUT 1 then PUT >$", "CUT 1\n", "PUT >$:\n+four\n", lineRanges(2, 2)},
	}
	for _, test := range tests {
		t.Run("E-13/"+test.name, func(t *testing.T) {
			env, cwd := editTestEnv(t)
			source := "one\ntwo\nthree"
			writeFixture(t, cwd, "f.txt", source)
			ref := env.Snapshots.Put("f.txt", []byte(source), []snapshot.Range{{Start: 1, End: 2}})
			if test.cooperating != "" {
				if _, err := runPatch(t, env, HashlineEnhanced, profileInput("f.txt", ref, test.cooperating)); err != nil {
					t.Fatal(err)
				}
			}
			failure := refusalOf(t, env, HashlineEnhanced, profileInput("f.txt", ref, test.tail))
			requireRecovery(t, failure, knownRead("f.txt", test.want, nil))
		})
	}
}

type countingReadRunner struct {
	runner.Runner
	reads int
}

func (r *countingReadRunner) ReadFile(ctx context.Context, path string) ([]byte, error) {
	r.reads++
	return r.Runner.ReadFile(ctx, path)
}

func TestRecoveryDoesNotReadTheFile(t *testing.T) {
	t.Run("OBS-04/refusal adds no read", func(t *testing.T) {
		env, cwd := editTestEnv(t)
		source := numberedLines(10)
		writeFixture(t, cwd, "f.txt", source)
		ref := env.Snapshots.Put("f.txt", []byte(source), []snapshot.Range{{Start: 1, End: 5}})
		counter := &countingReadRunner{Runner: env.Runner}
		env.Runner = counter
		failure := refusalOf(t, env, HashlineEnhanced, profileInput("f.txt", ref, "PUT 6-10:\n+x\n"))
		if failure.Outcome.Reason != tool.ReasonUnobservedRange {
			t.Fatalf("reason %q, want unobserved_range", failure.Outcome.Reason)
		}
		if counter.reads != 1 {
			t.Fatalf("the call read the file %d times, want the planner's one read", counter.reads)
		}
	})
}

func TestRefusalTextStillEndsWithTheEnhancedPhrase(t *testing.T) {
	const phrase = "need prior observation. Read them, then resend"
	t.Run("E-02 E05/text of the refusal", func(t *testing.T) {
		env, cwd := editTestEnv(t)
		source := numberedLines(10)
		writeFixture(t, cwd, "f.txt", source)
		ref := env.Snapshots.Put("f.txt", []byte(source), []snapshot.Range{{Start: 1, End: 5}})
		failure := refusalOf(t, env, HashlineEnhanced, profileInput("f.txt", ref, "PUT 6-10:\n+x\n"))
		want := "patch: changes[0]: lines 6-10 of f.txt " + phrase
		if text := failure.Error(); !strings.HasSuffix(text, phrase) || text != want {
			t.Fatalf("refusal text %q, want %q", text, want)
		}
	})
}

func TestUncoveredReturnsTheGapsOfARange(t *testing.T) {
	tests := []struct {
		name        string
		shown       []snapshot.Range
		first, last int
		want        []snapshot.Range
	}{
		{"nothing shown", nil, 3, 9, []snapshot.Range{{Start: 3, End: 9}}},
		{"fully covered", []snapshot.Range{{Start: 1, End: 10}}, 3, 9, nil},
		{"covered prefix", []snapshot.Range{{Start: 1, End: 5}}, 3, 9, []snapshot.Range{{Start: 6, End: 9}}},
		{"covered suffix", []snapshot.Range{{Start: 7, End: 12}}, 3, 9, []snapshot.Range{{Start: 3, End: 6}}},
		{"covered middle", []snapshot.Range{{Start: 5, End: 6}}, 3, 9, []snapshot.Range{{Start: 3, End: 4}, {Start: 7, End: 9}}},
		{"shown outside", []snapshot.Range{{Start: 20, End: 30}}, 3, 9, []snapshot.Range{{Start: 3, End: 9}}},
		{"shown before", []snapshot.Range{{Start: 1, End: 2}}, 3, 9, []snapshot.Range{{Start: 3, End: 9}}},
		{
			"several",
			[]snapshot.Range{{Start: 4, End: 4}, {Start: 6, End: 7}},
			3, 9,
			[]snapshot.Range{{Start: 3, End: 3}, {Start: 5, End: 5}, {Start: 8, End: 9}},
		},
		{
			"unsorted",
			[]snapshot.Range{{Start: 6, End: 7}, {Start: 4, End: 4}},
			3, 9,
			[]snapshot.Range{{Start: 3, End: 3}, {Start: 5, End: 5}, {Start: 8, End: 9}},
		},
		{"single row", []snapshot.Range{{Start: 1, End: 1}, {Start: 3, End: 3}}, 2, 2, []snapshot.Range{{Start: 2, End: 2}}},
	}
	for _, test := range tests {
		t.Run("INV-COVERAGE/"+test.name, func(t *testing.T) {
			got := uncovered(test.shown, test.first, test.last)
			if !slices.Equal(got, test.want) {
				t.Fatalf("uncovered(%v, %d, %d) = %v, want %v", test.shown, test.first, test.last, got, test.want)
			}
		})
	}
}
