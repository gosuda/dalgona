package platform

import (
	"slices"
	"unicode/utf16"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

func unicodeTextUnits(size uintptr) (uintptr, error) {
	switch {
	case size > transferLimit:
		return 0, ErrTransferTooLarge
	case size%2 != 0:
		return 0, inputtext.InvalidEncoding
	}
	return size / 2, nil
}

func decodeUnicodeText(units []uint16) (string, error) {
	end := slices.Index(units, 0)
	switch {
	case end < 0:
		return "", inputtext.InvalidEncoding
	case slices.ContainsFunc(units[end:], isTextUnit):
		return "", inputtext.DisallowedControl
	case utf8Length(units[:end]) > transferLimit:
		return "", ErrTransferTooLarge
	}
	return inputtext.DecodeUTF16(units[:end])
}

func isTextUnit(unit uint16) bool {
	return unit != 0
}

func utf8Length(units []uint16) int {
	length := 0
	for _, unit := range units {
		switch {
		case unit < 0x80:
			length++
		case unit < 0x800 || utf16.IsSurrogate(rune(unit)):
			length += 2
		default:
			length += 3
		}
	}
	return length
}
