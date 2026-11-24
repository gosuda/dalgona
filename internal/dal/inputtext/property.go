package inputtext

import "slices"

//go:generate go run ucdgen.go

type property uint16

const (
	cr property = iota + 1
	lf
	control
	extend
	zwj
	regionalIndicator
	prepend
	spacingMark
	hangulL
	hangulV
	hangulT
	hangulLV
	hangulLVT
)

const (
	breakClassMask       property = 0x0f
	extendedPictographic property = 1 << 4
	incbLinker           property = 1 << 5
	incbConsonant        property = 2 << 5
	incbExtend           property = 3 << 5
	incbMask             property = 3 << 5
	whiteSpace           property = 1 << 7
	defaultIgnorable     property = 1 << 8
)

type wordClass uint8

const (
	wordOther wordClass = iota
	wordCR
	wordLF
	wordNewline
	wordExtend
	wordZWJ
	wordRegionalIndicator
	wordFormat
	wordKatakana
	wordHebrewLetter
	wordALetter
	wordSingleQuote
	wordDoubleQuote
	wordMidNumLet
	wordMidLetter
	wordMidNum
	wordNumeric
	wordExtendNumLet
	wordWSegSpace
)

type sentenceClass uint8

const (
	sentenceCR sentenceClass = iota + 1
	sentenceLF
	sentenceExtend
	sentenceSep
	sentenceFormat
	sentenceSp
	sentenceLower
	sentenceUpper
	sentenceOLetter
	sentenceNumeric
	sentenceATerm
	sentenceSContinue
	sentenceSTerm
	sentenceClose
)

type scalarProperties struct {
	property
	word     wordClass
	sentence sentenceClass
}

const paletteMask = 1<<paletteBits - 1

func lookup(r rune) scalarProperties {
	next, _ := slices.BinarySearch(scalarRuns[:], (r+1)<<paletteBits)
	return scalarPalette[scalarRuns[next-1]&paletteMask]
}

func (p property) breakClass() property { return p & breakClassMask }

func (p property) conjunct() property { return p & incbMask }

func (p property) pictographic() bool { return p&extendedPictographic != 0 }

func (c wordClass) in(set uint32) bool { return uint32(1)<<c&set != 0 }

func (c sentenceClass) in(set uint32) bool { return uint32(1)<<c&set != 0 }
