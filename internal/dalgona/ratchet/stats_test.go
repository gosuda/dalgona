package ratchet

import (
	"math"
	"testing"
)

func checkOptional(t *testing.T, name string, got, want *float64) {
	t.Helper()
	switch {
	case want == nil && got != nil:
		t.Fatalf("%s = %v, want nil", name, *got)
	case want != nil && got == nil:
		t.Fatalf("%s is nil, want %v", name, *want)
	case want != nil && math.Abs(*got-*want) > 1e-4:
		t.Fatalf("%s = %v, want %v", name, *got, *want)
	}
}

func TestMeanAndHalfAndVerdict(t *testing.T) {
	t.Parallel()
	thirtyTwo := make([]float64, 0, 32)
	for range 16 {
		thirtyTwo = append(thirtyTwo, 1)
	}
	for range 16 {
		thirtyTwo = append(thirtyTwo, 0)
	}
	cases := []struct {
		name      string
		values    []float64
		direction string
		n         int
		mean      *float64
		half      *float64
		verdict   string
	}{
		{"mixed is flat", []float64{1, 1, 1, 0}, "higher", 4, new(0.75), new(0.7955), "flat"},
		{"mostly up", []float64{1, 1, 1, 1, 0, 1}, "higher", 6, new(0.83333), new(0.42850), "up"},
		{"constant is up", []float64{0.5, 0.5, 0.5}, "higher", 3, new(0.5), new(0.0), "up"},
		{"lower down", []float64{0.2, 0.2, 0.3, 0.3}, "lower", 4, new(0.25), new(0.09186), "down"},
		{"lower up", []float64{-0.2, -0.2, -0.3, -0.3}, "lower", 4, new(-0.25), new(0.09186), "up"},
		{"single value has no half", []float64{1}, "higher", 1, new(1.0), nil, "flat"},
		{"zeros are flat", []float64{0, 0, 0}, "higher", 3, new(0.0), new(0.0), "flat"},
		{"empty has no mean", nil, "higher", 0, nil, nil, "flat"},
		{"thirty-two values use the normal value", thirtyTwo, "higher", 32, new(0.5), new(0.17601), "up"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			got := meanAndHalf(tc.values)
			if got.N != tc.n {
				t.Fatalf("N = %d, want %d", got.N, tc.n)
			}
			checkOptional(t, "mean", got.Mean, tc.mean)
			checkOptional(t, "half", got.Half, tc.half)
			if v := verdict(got, tc.direction); v != tc.verdict {
				t.Fatalf("verdict = %q, want %q", v, tc.verdict)
			}
		})
	}
}

func TestTCritical(t *testing.T) {
	t.Parallel()
	checkOptional(t, "t(1)", new(tCritical(1)), new(12.706))
	checkOptional(t, "t(30)", new(tCritical(30)), new(2.042))
	checkOptional(t, "t(31)", new(tCritical(31)), new(1.96))
}

func TestPairedSkipsUnpaired(t *testing.T) {
	t.Parallel()
	cand := caseMeans{"a": 1, "b": 1, "c": 1}
	ref := caseMeans{"a": 0, "b": 0, "d": 0}
	got := paired(cand, ref, []string{"a", "b", "c", "d"}, "higher")
	if got.N != 2 {
		t.Fatalf("N = %d, want 2", got.N)
	}
	checkOptional(t, "mean", got.Mean, new(1.0))
	checkOptional(t, "half", got.Half, new(0.0))
	if got.Verdict != "up" {
		t.Fatalf("verdict = %q, want up", got.Verdict)
	}
}
