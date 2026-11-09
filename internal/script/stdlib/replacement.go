package stdlib

import (
	"errors"
	"fmt"
	"regexp"
	"strconv"
	"strings"
)

type replacementPart struct {
	literal string
	group   int
}

func (p replacementPart) isGroup() bool {
	return p.literal == "" && p.group >= 0
}

type replacementTemplate struct {
	parts []replacementPart
}

func parseReplacement(compiled *regexp.Regexp, replacement string) (replacementTemplate, error) {
	var template replacementTemplate
	for offset := 0; offset < len(replacement); {
		if replacement[offset] != '\\' {
			next := strings.IndexByte(replacement[offset:], '\\')
			if next < 0 {
				next = len(replacement) - offset
			}
			template.parts = appendLiteral(template.parts, replacement[offset:offset+next])
			offset += next
			continue
		}
		part, consumed, err := parseReplacementEscape(compiled, replacement[offset:])
		if err != nil {
			return replacementTemplate{}, err
		}
		if part.isGroup() || part.literal != "" {
			template.parts = append(template.parts, part)
		}
		offset += consumed
	}
	return template, nil
}

func appendLiteral(parts []replacementPart, literal string) []replacementPart {
	if literal == "" {
		return parts
	}
	return append(parts, replacementPart{literal: literal, group: -1})
}

func parseReplacementEscape(compiled *regexp.Regexp, replacement string) (replacementPart, int, error) {
	if len(replacement) < 2 {
		return replacementPart{}, 0, errors.New("bad escape (end of pattern) in replacement")
	}
	escape := replacement[1]
	switch {
	case isThreeDigitOctal(replacement):
		return octalReplacement(replacement)
	case isDecimalDigit(escape):
		return numericReplacementGroup(compiled, replacement)
	case escape == '0':
		return octalReplacement(replacement)
	case escape == 'g':
		return namedReplacementGroup(compiled, replacement)
	default:
		return simpleReplacementEscape(replacement)
	}
}

func simpleReplacementEscape(replacement string) (replacementPart, int, error) {
	escape := replacement[1]
	if index := strings.IndexByte("nrtfvab\\", escape); index >= 0 {
		values := []byte{'\n', '\r', '\t', '\f', '\v', '\a', '\b', '\\'}
		return replacementPart{literal: string(values[index : index+1]), group: -1}, 2, nil
	}
	if isASCIILetter(escape) {
		return replacementPart{}, 0, fmt.Errorf("bad escape \\%c in replacement", escape)
	}
	return replacementPart{literal: replacement[:2], group: -1}, 2, nil
}

func numericReplacementGroup(compiled *regexp.Regexp, replacement string) (replacementPart, int, error) {
	end := 2
	if end < len(replacement) && replacement[end] >= '0' && replacement[end] <= '9' {
		end++
	}
	index, err := strconv.Atoi(replacement[1:end])
	if err != nil {
		return replacementPart{}, 0, fmt.Errorf("parse replacement group: %w", err)
	}
	if index >= compiled.NumSubexp()+1 {
		return replacementPart{}, 0, fmt.Errorf("invalid group reference %d", index)
	}
	return replacementPart{group: index}, end, nil
}

func octalReplacement(replacement string) (replacementPart, int, error) {
	end := 2
	for end < len(replacement) && end < 4 && replacement[end] >= '0' && replacement[end] <= '7' {
		end++
	}
	value, err := strconv.ParseUint(replacement[1:end], 8, 8)
	if err != nil {
		return replacementPart{}, 0, fmt.Errorf("parse octal replacement: %w", err)
	}
	return replacementPart{literal: fmt.Sprintf("%c", value), group: -1}, end, nil
}

func namedReplacementGroup(compiled *regexp.Regexp, replacement string) (replacementPart, int, error) {
	if len(replacement) < 4 || replacement[2] != '<' {
		return replacementPart{}, 0, errors.New("missing < in \\g replacement")
	}
	end := strings.IndexByte(replacement[3:], '>')
	if end < 0 {
		return replacementPart{}, 0, errors.New("missing > in \\g replacement")
	}
	end += 3
	name := replacement[3:end]
	index, err := replacementGroupIndex(compiled, name)
	if err != nil {
		return replacementPart{}, 0, err
	}
	return replacementPart{group: index}, end + 1, nil
}

func replacementGroupIndex(compiled *regexp.Regexp, name string) (int, error) {
	if index, err := strconv.Atoi(name); err == nil {
		if index < 0 || index > compiled.NumSubexp() {
			return 0, fmt.Errorf("invalid group reference %d", index)
		}
		return index, nil
	}
	for index, candidate := range compiled.SubexpNames() {
		if index > 0 && candidate == name {
			return index, nil
		}
	}
	return 0, fmt.Errorf("unknown group name %q", name)
}

func isASCIILetter(value byte) bool {
	return value >= 'a' && value <= 'z' || value >= 'A' && value <= 'Z'
}

func isDecimalDigit(value byte) bool {
	return value >= '1' && value <= '9'
}

func isThreeDigitOctal(replacement string) bool {
	if len(replacement) < 4 {
		return false
	}
	for i := 1; i < 4; i++ {
		if replacement[i] < '0' || replacement[i] > '7' {
			return false
		}
	}
	return true
}

func (t replacementTemplate) expand(out *strings.Builder, text string, indices []int) {
	for _, part := range t.parts {
		if !part.isGroup() {
			out.WriteString(part.literal)
			continue
		}
		start, end := indices[part.group*2], indices[part.group*2+1]
		if start >= 0 {
			out.WriteString(text[start:end])
		}
	}
}
