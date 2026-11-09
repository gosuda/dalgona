package snapshot

import (
	"strings"
	"unicode/utf8"
)

const hexDigits = "0123456789abcdef"

var shortEscapes = [0x20]byte{'\b': 'b', '\f': 'f', '\n': 'n', '\r': 'r', '\t': 't'}

func AppendJSONString(out []byte, text string) []byte {
	out = append(out, '"')
	for i := 0; i < len(text); {
		char, size := utf8.DecodeRuneInString(text[i:])
		switch {
		case char == '"' || char == '\\':
			out = append(out, '\\', text[i])
		case char < 0x20:
			out = appendControl(out, text[i])
		default:
			out = utf8.AppendRune(out, char)
		}
		i += size
	}
	return append(out, '"')
}

func appendControl(out []byte, char byte) []byte {
	if short := shortEscapes[char]; short != 0 {
		return append(out, '\\', short)
	}
	return append(out, '\\', 'u', '0', '0', hexDigits[char>>4], hexDigits[char&0x0f])
}

func JSONString(text string) string {
	return string(AppendJSONString(nil, text))
}

func HeaderPath(path string) string {
	quoted := path == "" || strings.TrimSpace(path) != path || strings.HasPrefix(path, "\"") || strings.ContainsAny(path, "@#]\r\n")
	for _, char := range path {
		quoted = quoted || char < 0x20 || char == 0x7f
	}
	if quoted {
		return JSONString(path)
	}
	return path
}
