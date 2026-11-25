package textinput

import (
	"strings"
	"unicode"
	"unicode/utf16"
	"unicode/utf8"
)

func (r Reason) Error() string { return string(r) }

type Unit uint8

const (
	Bytes Unit = iota
	Scalars
	UTF16Units
)

func DecodeUTF8(b []byte) (string, error) {
	if !utf8.Valid(b) {
		return "", InvalidEncoding
	}
	return string(b), nil
}

func DecodeUTF16(units []uint16) (string, error) {
	var text strings.Builder
	text.Grow(len(units))
	for len(units) > 0 {
		r, size := rune(units[0]), 1
		if utf16.IsSurrogate(r) {
			if len(units) < 2 {
				return "", InvalidEncoding
			}
			r, size = utf16.DecodeRune(r, rune(units[1])), 2
			if r == unicode.ReplacementChar {
				return "", InvalidEncoding
			}
		}
		text.WriteRune(r)
		units = units[size:]
	}
	return text.String(), nil
}

func Count(s string, unit Unit) int {
	switch unit {
	case Bytes:
		return len(s)
	case Scalars:
		return utf8.RuneCountInString(s)
	case UTF16Units:
	}
	units := 0
	for _, r := range s {
		units += utf16.RuneLen(r)
	}
	return units
}

func Convert(s string, offset int, from, to Unit) (int, error) {
	source, target := 0, 0
	for i := 0; source < offset && i < len(s); {
		r, size := utf8.DecodeRuneInString(s[i:])
		source += from.width(r, size)
		target += to.width(r, size)
		i += size
	}
	if source != offset {
		return 0, InvalidOffset
	}
	return target, nil
}

func (u Unit) width(r rune, size int) int {
	switch u {
	case Scalars:
		return 1
	case UTF16Units:
		return utf16.RuneLen(r)
	case Bytes:
	}
	return size
}
