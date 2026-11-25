package math

import "testing"

func TestAlphabetsMapToMathematicalAlphanumericSymbols(t *testing.T) {
	cases := []struct {
		r        rune
		alphabet string
		want     rune
	}{
		{'x', "", '𝑥'},
		{'A', "", '𝐴'},
		{'h', "", 'ℎ'},
		{'α', "", '𝛼'},
		{'ω', "", '𝜔'},
		{'ϕ', "", '𝜙'},
		{'Γ', "", 'Γ'},
		{'7', "", '7'},
		{'x', "mathrm", 'x'},
		{'a', "mathbf", '𝐚'},
		{'Z', "mathbf", '𝐙'},
		{'3', "mathbf", '𝟑'},
		{'h', "mathit", 'ℎ'},
		{'q', "mathsf", '𝗊'},
		{'0', "mathtt", '𝟶'},
		{'R', "mathbb", 'ℝ'},
		{'A', "mathbb", '𝔸'},
		{'1', "mathbb", '𝟙'},
		{'L', "mathcal", 'ℒ'},
		{'A', "mathcal", '𝒜'},
		{'e', "mathcal", 'ℯ'},
		{'+', "mathbf", '+'},
	}
	for _, tc := range cases {
		if got := styledRune(tc.r, tc.alphabet); got != tc.want {
			t.Errorf("styledRune(%q, %q) = %U, want %U", tc.r, tc.alphabet, got, tc.want)
		}
	}
}

func TestCoverageLookupsResolveBothFormats(t *testing.T) {
	data := []byte{
		0, 0,
		0, 1, 0, 3, 0, 3, 0, 7, 0, 9,
		0, 2, 0, 2, 0, 10, 0, 14, 0, 0, 0, 20, 0, 20, 0, 5,
	}
	table := &mathTable{reader: tableReader{data: data}}
	formatOne := map[uint32][2]int{3: {0, 1}, 7: {1, 1}, 9: {2, 1}, 8: {0, 0}, 2: {0, 0}, 10: {0, 0}}
	for glyph, want := range formatOne {
		index, ok := table.coverageIndex(2, glyph)
		if ok != (want[1] == 1) || ok && index != want[0] {
			t.Errorf("format 1 glyph %d = %d %v, want %v", glyph, index, ok, want)
		}
	}
	formatTwo := map[uint32][2]int{10: {0, 1}, 12: {2, 1}, 14: {4, 1}, 20: {5, 1}, 15: {0, 0}, 9: {0, 0}, 21: {0, 0}}
	for glyph, want := range formatTwo {
		index, ok := table.coverageIndex(12, glyph)
		if ok != (want[1] == 1) || ok && index != want[0] {
			t.Errorf("format 2 glyph %d = %d %v, want %v", glyph, index, ok, want)
		}
	}
}
