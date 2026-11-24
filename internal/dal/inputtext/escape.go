package inputtext

import "unicode/utf8"

const (
	escape        = 0x1b
	bell          = 0x07
	cancel        = 0x18
	substitute    = 0x1a
	terminatorEnd = '\\'
)

func EscapeLength(s string) int {
	if len(s) < 2 || s[0] != escape {
		return 0
	}
	switch s[1] {
	case '[':
		return headerEnd(s, 2)
	case 'P':
		return controlStringEnd(s, headerEnd(s, 2))
	case ']':
		return operatingCommandEnd(s)
	case 'X', '^', '_':
		return controlStringEnd(s, 2)
	}
	return shortEscapeEnd(s)
}

func headerEnd(s string, i int) int {
	for i < len(s) && s[i] >= 0x30 && s[i] <= 0x3f {
		i++
	}
	i = intermediatesEnd(s, i)
	if i < len(s) && s[i] >= 0x40 && s[i] <= 0x7e {
		return i + 1
	}
	return 0
}

func shortEscapeEnd(s string) int {
	i := intermediatesEnd(s, 1)
	if i < len(s) && s[i] >= 0x30 && s[i] <= 0x7e {
		return i + 1
	}
	return 0
}

func intermediatesEnd(s string, i int) int {
	for i < len(s) && s[i] >= 0x20 && s[i] <= 0x2f {
		i++
	}
	return i
}

func operatingCommandEnd(s string) int {
	i := 2
	for i < len(s) && s[i] >= '0' && s[i] <= '9' {
		i++
	}
	switch {
	case i == 2 || i == len(s):
		return 0
	case s[i] == ';':
		return payloadEnd(s, i+1, true)
	case s[i] == bell:
		return i + 1
	}
	return stringTerminatorEnd(s, i)
}

func controlStringEnd(s string, i int) int {
	if i == 0 {
		return 0
	}
	return payloadEnd(s, i, false)
}

func payloadEnd(s string, i int, bellTerminates bool) int {
	for i < len(s) {
		switch s[i] {
		case escape:
			return stringTerminatorEnd(s, i)
		case cancel, substitute:
			return 0
		case bell:
			if bellTerminates {
				return i + 1
			}
		}
		r, size := utf8.DecodeRuneInString(s[i:])
		if r == utf8.RuneError && size == 1 {
			return 0
		}
		i += size
	}
	return 0
}

func stringTerminatorEnd(s string, i int) int {
	if i+1 < len(s) && s[i] == escape && s[i+1] == terminatorEnd {
		return i + 2
	}
	return 0
}
