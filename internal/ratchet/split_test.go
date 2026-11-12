package ratchet

import (
	"fmt"
	"slices"
	"strconv"
	"strings"
	"testing"
)

func makeStrata(specs ...string) map[string]string {
	strata := make(map[string]string)
	for _, spec := range specs {
		tag, countText, _ := strings.Cut(spec, ":")
		count, _ := strconv.Atoi(countText)
		for i := 1; i <= count; i++ {
			strata[fmt.Sprintf("%s%d", tag, i)] = tag
		}
	}
	return strata
}

type splitCase struct {
	name     string
	strata   map[string]string
	fraction float64
	train    int
	test     int
	testTags map[string]int
	absent   []string
}

func checkSplitCase(t *testing.T, tc splitCase, s state) {
	t.Helper()
	if len(s.TrainIDs) != tc.train || len(s.TestIDs) != tc.test {
		t.Fatalf("split sizes = %d train %d test, want %d and %d", len(s.TrainIDs), len(s.TestIDs), tc.train, tc.test)
	}
	tags := make(map[string]int)
	for _, id := range s.TestIDs {
		tags[tc.strata[id]]++
	}
	for tag, want := range tc.testTags {
		if tags[tag] != want {
			t.Fatalf("test tag %s = %d, want %d", tag, tags[tag], want)
		}
	}
	for _, tag := range tc.absent {
		if tags[tag] != 0 {
			t.Fatalf("tag %s reached the test side", tag)
		}
	}
	if len(tc.strata) != len(s.TrainIDs)+len(s.TestIDs) {
		t.Fatalf("split lost or duplicated cases")
	}
}

func TestSplitCounts(t *testing.T) {
	t.Parallel()
	cases := []splitCase{
		{name: "stratified pos and neg", strata: makeStrata("pos:5", "neg:3"), fraction: 0.4, train: 5, test: 3, testTags: map[string]int{"pos": 2, "neg": 1}},
		{name: "tiny stratum stays in train", strata: makeStrata("y:4", "x:1"), fraction: 0.4, train: 3, test: 2, testTags: map[string]int{"y": 2}, absent: []string{"x"}},
		{name: "tiny test side is filled", strata: makeStrata("t:4"), fraction: 0.1, train: 3, test: 1, testTags: map[string]int{"t": 1}},
		{name: "tiny train side is filled", strata: makeStrata("t:4"), fraction: 0.9, train: 1, test: 3, testTags: map[string]int{"t": 3}},
		{name: "half rounds up", strata: makeStrata("t:5"), fraction: 0.5, train: 2, test: 3, testTags: map[string]int{"t": 3}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			var s state
			if err := s.split(tc.strata, tc.fraction, 7); err != nil {
				t.Fatal(err)
			}
			checkSplitCase(t, tc, s)
		})
	}
}

func TestSplitRefusals(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name     string
		state    state
		strata   map[string]string
		fraction float64
		want     string
	}{
		{"too few cases", state{}, makeStrata("t:3"), 0.4, "split needs at least 4 cases"},
		{"fraction too small", state{}, makeStrata("t:4"), 0.05, "test_fraction must be from 0.1 to 0.9"},
		{"bad case id", state{}, map[string]string{"a/b": "t", "c": "t", "d": "t", "e": "t"}, 0.4, `case id "a/b" must match [A-Za-z0-9][A-Za-z0-9._-]{0,127}`},
		{"frozen after baseline", state{Rounds: []round{{Variant: "baseline"}}}, makeStrata("t:4"), 0.4, "the split is frozen once the baseline is gated"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			s := tc.state
			err := s.split(tc.strata, tc.fraction, 7)
			if err == nil || err.Error() != tc.want {
				t.Fatalf("split error = %v, want %q", err, tc.want)
			}
		})
	}
}

func TestSplitSameSeedSameIDs(t *testing.T) {
	t.Parallel()
	strata := makeStrata("pos:6", "neg:6")
	var first, second state
	if err := first.split(strata, 0.4, 7); err != nil {
		t.Fatal(err)
	}
	if err := second.split(strata, 0.4, 7); err != nil {
		t.Fatal(err)
	}
	if !slices.Equal(first.TrainIDs, second.TrainIDs) || !slices.Equal(first.TestIDs, second.TestIDs) {
		t.Fatalf("same seed gave different splits: %v %v and %v %v", first.TrainIDs, first.TestIDs, second.TrainIDs, second.TestIDs)
	}
	if first.SplitSeed != 7 || first.TestFraction != 0.4 {
		t.Fatalf("recorded seed %d fraction %v, want 7 and 0.4", first.SplitSeed, first.TestFraction)
	}
}
