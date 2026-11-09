package patch

import "fmt"

type Style string

const (
	Anchor           Style = "anchor"
	Replace          Style = "replace"
	Hashline         Style = "hashline"
	HashlineLight    Style = "hashline-light"
	HashlineEnhanced Style = "hashline-enhanced"
	ApplyPatch       Style = "apply_patch"
)

type locatorKind uint8

const (
	locatorText locatorKind = iota + 1
	locatorSpan
	locatorLines
	locatorGap
	locatorWhole
)

type action uint8

const (
	actionReplace action = iota + 1
	actionInsertBefore
	actionInsertAfter
)

type guardKind uint8

const (
	guardQuoted guardKind = iota + 1
	guardWholeTag
	guardSeen
	guardVersion
	guardReference
	guardExists
)

type window uint8

const (
	beforePayload window = iota + 1
	afterPrevious
)

type locator struct {
	kind     locatorKind
	old      string
	lineHint int
	all      bool
	first    int
	last     int
	before   int
	quoted   []string
	context  string
	atEOF    bool
}

type guard struct {
	kind guardKind
	text string
}

type editKind uint8

const (
	editChange editKind = iota + 1
	editCreate
	editDelete
	editRename
)

type edit struct {
	kind      editKind
	index     int
	path      string
	to        string
	locator   locator
	action    action
	guard     guard
	body      string
	window    window
	reference string
}

type parseFailure struct {
	line    int
	message string
}

func (e parseFailure) Error() string { return e.message }

func unmaskEdits(edits []edit, unmask func(string) (string, error)) error {
	for i := range edits {
		var err error
		if edits[i].locator.old, err = unmask(edits[i].locator.old); err != nil {
			return err
		}
		if edits[i].locator.context, err = unmask(edits[i].locator.context); err != nil {
			return err
		}
		if edits[i].body, err = unmask(edits[i].body); err != nil {
			return err
		}
		for row := range edits[i].locator.quoted {
			if edits[i].locator.quoted[row], err = unmask(edits[i].locator.quoted[row]); err != nil {
				return err
			}
		}
	}
	return nil
}

type styleError struct {
	style Style
	err   error
}

func (e styleError) Error() string {
	return fmt.Sprintf("%s\nCurrent edit_style is %q for this model: past calls in history may use another dialect. Do not imitate them.", e.err, e.style)
}

func (e styleError) Unwrap() error { return e.err }

func parseError(style Style, err error) error {
	return styleError{style: style, err: err}
}

func expected(line int, what, text string) error {
	return parseFailure{line: line, message: fmt.Sprintf("patch: line %d: expected %s, got %q.", line, what, text)}
}

func canonicalStyle(style string) Style {
	switch style {
	case "":
		return Anchor
	case "balanced":
		return Anchor
	case "simple":
		return Replace
	case "strict":
		return Hashline
	default:
		return Style(style)
	}
}

func (s Style) profile() bool {
	return s == HashlineLight || s == HashlineEnhanced
}
