package patch

import "strings"

type framedLine struct {
	text       string
	raw        string
	terminated bool
}

func splitFramed(input string) []framedLine {
	return splitFramedLines(input, false)
}

func splitFramedCRLF(input string) []framedLine {
	return splitFramedLines(input, true)
}

func splitFramedLines(input string, crlf bool) []framedLine {
	parts := strings.SplitAfter(input, "\n")
	out := make([]framedLine, 0, len(parts))
	for _, raw := range parts {
		if raw == "" {
			continue
		}
		terminated := strings.HasSuffix(raw, "\n")
		text, _ := strings.CutSuffix(raw, "\n")
		if crlf {
			text, _ = strings.CutSuffix(text, "\r")
		}
		out = append(out, framedLine{text: text, raw: raw, terminated: terminated})
	}
	return out
}

func stripOuterFence(input string) string {
	if !strings.HasPrefix(input, "```") {
		return input
	}
	_, rest, ok := strings.Cut(input, "\n")
	if !ok {
		return input
	}
	rest, _ = strings.CutSuffix(rest, "\n")
	body, ok := strings.CutSuffix(rest, "```")
	if !ok || !strings.HasSuffix(body, "\n") {
		return input
	}
	return body
}

func stripBeginEnd(input string) string {
	return stripEnvelope(input)
}

func stripMatchedBeginEnd(input string) string {
	begin := strings.HasPrefix(input, "*** Begin Patch\n")
	end := strings.HasSuffix(input, "*** End Patch\n") || strings.HasSuffix(input, "*** End Patch")
	if begin != end {
		return ""
	}
	input, _ = strings.CutPrefix(input, "*** Begin Patch\n")
	if body, ok := strings.CutSuffix(input, "*** End Patch\n"); ok {
		return body
	}
	input, _ = strings.CutSuffix(input, "*** End Patch")
	return input
}

func stripEnvelope(input string) string {
	input, _ = strings.CutPrefix(input, "*** Begin Patch\n")
	if body, ok := strings.CutSuffix(input, "\n*** End Patch\n"); ok {
		return body
	}
	input, _ = strings.CutSuffix(input, "\n*** End Patch")
	return input
}

func joinBody(rows []string, terminated bool) string {
	body := strings.Join(rows, "\n")
	if terminated {
		body += "\n"
	}
	return body
}

func upperHex(text string, length int) bool {
	if len(text) != length {
		return false
	}
	for _, char := range text {
		if !strings.ContainsRune("0123456789ABCDEF", char) {
			return false
		}
	}
	return true
}

func positiveNumber(text string) (int, bool) {
	if text == "" {
		return 0, false
	}
	value := 0
	for _, char := range text {
		if char < '0' || char > '9' {
			return 0, false
		}
		next := value*10 + int(char-'0')
		if next < value {
			return 0, false
		}
		value = next
	}
	return value, value > 0
}

func parseRange(text string) (int, int, bool) {
	firstText, lastText, found := strings.Cut(text, ".=")
	if !found {
		firstText, lastText, found = strings.Cut(text, "-")
	}
	if !found {
		firstText, lastText = text, text
	}
	first, ok := positiveNumber(firstText)
	if !ok {
		return 0, 0, false
	}
	last, ok := positiveNumber(lastText)
	return first, last, ok && first <= last
}
