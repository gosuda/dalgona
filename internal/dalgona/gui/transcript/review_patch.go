//go:build gui

package transcript

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"strings"
	"unicode/utf16"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
)

var errPatchJSON = errors.New("the recorded changes are not valid patch JSON")

type patchJSON struct {
	in *bufio.Reader
}

func streamReviewPatch(ctx context.Context, source io.Reader, out io.Writer) error {
	p := patchJSON{in: bufio.NewReader(source)}
	w := bufio.NewWriter(out)
	if err := p.expect('['); err != nil {
		return err
	}
	if err := p.members(']', func() error {
		if err := ctx.Err(); err != nil {
			return err
		}
		return p.change(w)
	}); err != nil {
		return err
	}
	if _, err := p.next(); !errors.Is(err, io.EOF) {
		return patchSyntax(err)
	}
	return w.Flush()
}

func (p patchJSON) change(out *bufio.Writer) error {
	if err := p.expect('{'); err != nil {
		return err
	}
	var sawDiff, sawOmitted bool
	return p.members('}', func() error {
		name, err := p.name()
		if err != nil {
			return err
		}
		switch name {
		case "diff":
			if sawDiff {
				return errPatchJSON
			}
			sawDiff = true
			return p.readString(func(r rune) error {
				_, err := out.WriteRune(r)
				return err
			})
		case "diffOmitted":
			if sawOmitted {
				return errPatchJSON
			}
			sawOmitted = true
			return p.notOmitted()
		default:
			return p.skipValue(0)
		}
	})
}

func (p patchJSON) notOmitted() error {
	ch, err := p.next()
	if err != nil {
		return patchSyntax(err)
	}
	switch ch {
	case 't':
		if literalErr := p.literal("rue"); literalErr != nil {
			return literalErr
		}
		return errPatchOmitted
	case 'f':
		return p.literal("alse")
	default:
		return errPatchJSON
	}
}

func (p patchJSON) members(end byte, member func() error) error {
	ch, err := p.next()
	if err != nil {
		return patchSyntax(err)
	}
	if ch == end {
		return nil
	}
	if err = p.in.UnreadByte(); err != nil {
		return err
	}
	for {
		if err = member(); err != nil {
			return err
		}
		ch, err = p.next()
		if err != nil {
			return patchSyntax(err)
		}
		if ch == end {
			return nil
		}
		if ch != ',' {
			return errPatchJSON
		}
	}
}

func (p patchJSON) name() (string, error) {
	var name strings.Builder
	long := false
	if err := p.readString(func(r rune) error {
		if name.Len() >= len("diffOmitted") {
			long = true
		} else {
			name.WriteRune(r)
		}
		return nil
	}); err != nil {
		return "", err
	}
	if err := p.expect(':'); err != nil {
		return "", err
	}
	if long {
		return "", nil
	}
	return name.String(), nil
}

func (p patchJSON) skipValue(depth int) error {
	ch, err := p.next()
	if err != nil {
		return patchSyntax(err)
	}
	switch ch {
	case '{', '[':
		return p.skipContainer(ch, depth)
	case '"':
		return p.stringBody(nil)
	case 'f':
		return p.literal("alse")
	case 't':
		return p.literal("rue")
	case 'n':
		return p.literal("ull")
	default:
		return p.number(ch)
	}
}

func (p patchJSON) skipContainer(start byte, depth int) error {
	if depth == document.MaxNesting {
		return errPatchJSON
	}
	if start == '[' {
		return p.members(']', func() error { return p.skipValue(depth + 1) })
	}
	return p.members('}', func() error {
		if err := p.readString(nil); err != nil {
			return err
		}
		if err := p.expect(':'); err != nil {
			return err
		}
		return p.skipValue(depth + 1)
	})
}

func (p patchJSON) number(ch byte) error {
	if ch == '-' {
		first, err := p.in.ReadByte()
		if err != nil {
			return patchSyntax(err)
		}
		ch = first
	}
	if !patchDigit(ch) {
		return errPatchJSON
	}
	if ch != '0' {
		p.digits()
	}
	if p.takeOne(".") && !p.digits() {
		return errPatchJSON
	}
	if p.takeOne("eE") {
		p.takeOne("+-")
		if !p.digits() {
			return errPatchJSON
		}
	}
	return nil
}

func (p patchJSON) takeOne(chars string) bool {
	next, err := p.in.Peek(1)
	if err != nil || strings.IndexByte(chars, next[0]) < 0 {
		return false
	}
	_, _ = p.in.Discard(1)
	return true
}

func patchDigit(ch byte) bool { return '0' <= ch && ch <= '9' }

func (p patchJSON) digits() bool {
	any := false
	for {
		next, err := p.in.Peek(1)
		if err != nil || !patchDigit(next[0]) {
			return any
		}
		_, _ = p.in.Discard(1)
		any = true
	}
}

func (p patchJSON) next() (byte, error) {
	for {
		ch, err := p.in.ReadByte()
		if err != nil {
			return 0, err
		}
		switch ch {
		case ' ', '\t', '\n', '\r':
		default:
			return ch, nil
		}
	}
}

func (p patchJSON) expect(want byte) error {
	ch, err := p.next()
	if err != nil || ch != want {
		return patchSyntax(err)
	}
	return nil
}

func (p patchJSON) literal(word string) error {
	for i := range len(word) {
		ch, err := p.in.ReadByte()
		if err != nil || ch != word[i] {
			return patchSyntax(err)
		}
	}
	return nil
}

func (p patchJSON) readString(emit func(rune) error) error {
	if err := p.expect('"'); err != nil {
		return err
	}
	return p.stringBody(emit)
}

func (p patchJSON) stringBody(emit func(rune) error) error {
	for {
		r, end, err := p.stringRune()
		if end || err != nil {
			return err
		}
		if emit == nil {
			continue
		}
		if err = emit(r); err != nil {
			return err
		}
	}
}

func (p patchJSON) stringRune() (rune, bool, error) {
	r, width, err := p.in.ReadRune()
	if err != nil {
		return 0, false, patchSyntax(err)
	}
	switch {
	case r == '"':
		return 0, true, nil
	case r < ' ' || r == utf8.RuneError && width == 1:
		return 0, false, errPatchJSON
	case r == '\\':
		r, err = p.escape()
	}
	return r, false, err
}

func (p patchJSON) escape() (rune, error) {
	ch, err := p.in.ReadByte()
	if err != nil {
		return 0, patchSyntax(err)
	}
	const encoded, decoded = "\"\\/bfnrt", "\"\\/\b\f\n\r\t"
	if at := strings.IndexByte(encoded, ch); at >= 0 {
		return rune(decoded[at]), nil
	}
	if ch == 'u' {
		return p.unicode()
	}
	return 0, errPatchJSON
}

func (p patchJSON) unicode() (rune, error) {
	first, err := p.hexRune()
	if err != nil || !utf16.IsSurrogate(first) {
		return first, err
	}
	if err = p.literal("\\u"); err != nil {
		return 0, err
	}
	second, err := p.hexRune()
	if err != nil {
		return 0, err
	}
	r := utf16.DecodeRune(first, second)
	if r == utf8.RuneError {
		return 0, errPatchJSON
	}
	return r, nil
}

func (p patchJSON) hexRune() (rune, error) {
	var value rune
	for range 4 {
		ch, err := p.in.ReadByte()
		if err != nil {
			return 0, patchSyntax(err)
		}
		value <<= 4
		switch {
		case patchDigit(ch):
			value |= rune(ch - '0')
		case 'a' <= ch && ch <= 'f':
			value |= rune(ch-'a') + 10
		case 'A' <= ch && ch <= 'F':
			value |= rune(ch-'A') + 10
		default:
			return 0, errPatchJSON
		}
	}
	return value, nil
}

func patchSyntax(err error) error {
	if err == nil || errors.Is(err, io.EOF) {
		return errPatchJSON
	}
	return fmt.Errorf("read recorded changes: %w", err)
}
