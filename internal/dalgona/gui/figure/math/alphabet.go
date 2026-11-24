package math

type alphabetBlock struct {
	upper, lower, digit rune
	exceptions          map[rune]rune
}

var italicBlock = alphabetBlock{upper: 0x1D434, lower: 0x1D44E, exceptions: map[rune]rune{'h': 'ℎ'}}

var alphabetBlocks = map[string]alphabetBlock{
	"mathit": italicBlock,
	"mathbf": {upper: 0x1D400, lower: 0x1D41A, digit: 0x1D7CE},
	"mathsf": {upper: 0x1D5A0, lower: 0x1D5BA, digit: 0x1D7E2},
	"mathtt": {upper: 0x1D670, lower: 0x1D68A, digit: 0x1D7F6},
	"mathbb": {upper: 0x1D538, lower: 0x1D552, digit: 0x1D7D8, exceptions: map[rune]rune{
		'C': 'ℂ', 'H': 'ℍ', 'N': 'ℕ', 'P': 'ℙ', 'Q': 'ℚ', 'R': 'ℝ', 'Z': 'ℤ',
	}},
	"mathcal": {upper: 0x1D49C, lower: 0x1D4B6, exceptions: map[rune]rune{
		'B': 'ℬ', 'E': 'ℰ', 'F': 'ℱ', 'H': 'ℋ', 'I': 'ℐ', 'L': 'ℒ', 'M': 'ℳ', 'R': 'ℛ', 'e': 'ℯ', 'g': 'ℊ', 'o': 'ℴ',
	}},
	"mathrm": {},
}

var italicGreekVariants = map[rune]rune{'ϵ': 0x1D716, 'ϑ': 0x1D717, 'ϰ': 0x1D718, 'ϕ': 0x1D719, 'ϱ': 0x1D71A, 'ϖ': 0x1D71B}

func styledRune(r rune, alphabet string) rune {
	if alphabet == "" {
		return defaultRune(r)
	}
	return alphabetBlocks[alphabet].apply(r)
}

func defaultRune(r rune) rune {
	if 'α' <= r && r <= 'ω' {
		return 0x1D6FC + r - 'α'
	}
	if variant, ok := italicGreekVariants[r]; ok {
		return variant
	}
	return italicBlock.apply(r)
}

func (b alphabetBlock) apply(r rune) rune {
	if replacement, ok := b.exceptions[r]; ok {
		return replacement
	}
	ranges := [...]struct{ first, last, start rune }{{'A', 'Z', b.upper}, {'a', 'z', b.lower}, {'0', '9', b.digit}}
	for _, letters := range ranges {
		if letters.start != 0 && letters.first <= r && r <= letters.last {
			return letters.start + r - letters.first
		}
	}
	return r
}
