package tui

import (
	"math"
	"slices"
	"strconv"
	"strings"
	"unicode"
)

type Keyword struct {
	Word string `json:"word"`
	Hue  [2]int `json:"hue"`
}

type keywordSpan struct {
	start   int
	end     int
	keyword Keyword
}

type fence struct {
	marker rune
	length int
}

type fenceMark struct {
	fence fence
	rest  []rune
}

type tag struct {
	name        []rune
	end         int
	closing     bool
	selfClosing bool
}

func matchKeywords(prompt string, table []Keyword) []Keyword {
	masked := maskNonProse([]rune(prompt))
	result := make([]Keyword, 0, len(table))
	for _, item := range table {
		if item.Word != "" && item.Word == strings.ToLower(item.Word) && containsKeyword(masked, []rune(item.Word)) {
			result = append(result, item)
		}
	}
	return result
}

func keywordSpans(prompt string, table []Keyword) []keywordSpan {
	masked := maskNonProse([]rune(prompt))
	var spans []keywordSpan
	for _, item := range table {
		if item.Word == "" || item.Word != strings.ToLower(item.Word) {
			continue
		}
		spans = appendKeywordSpans(spans, masked, item)
	}
	slices.SortStableFunc(spans, func(a, b keywordSpan) int { return a.start - b.start })
	return spans
}

func appendKeywordSpans(spans []keywordSpan, masked []rune, item Keyword) []keywordSpan {
	word := []rune(item.Word)
	for start := 0; start+len(word) <= len(masked); start++ {
		end := start + len(word)
		if slices.Equal(masked[start:end], word) && keywordBoundary(masked, start, end) {
			spans = append(spans, keywordSpan{start: start, end: end, keyword: item})
			start = end - 1
		}
	}
	return spans
}

func containsKeyword(masked, word []rune) bool {
	for start := 0; start+len(word) <= len(masked); start++ {
		end := start + len(word)
		if slices.Equal(masked[start:end], word) && keywordBoundary(masked, start, end) {
			return true
		}
	}
	return false
}

func glowKeywords(text string, table []Keyword, phase float64) string {
	spans := keywordSpans(text, table)
	if len(spans) == 0 {
		return text
	}
	runes := []rune(text)
	var out strings.Builder
	out.Grow(len(text) + len(spans)*64)
	last := 0
	phase = wrappedPhase(phase)
	for _, span := range spans {
		if span.start < last {
			continue
		}
		out.WriteString(string(runes[last:span.start]))
		out.WriteString(paintKeyword(runes[span.start:span.end], span.keyword, phase))
		last = span.end
	}
	out.WriteString(string(runes[last:]))
	return out.String()
}

func wrappedPhase(phase float64) float64 {
	return phase - math.Floor(phase)
}

func paintKeyword(word []rune, keyword Keyword, phase float64) string {
	palette := keywordPalette(keyword.Hue)
	var out strings.Builder
	out.Grow(len(word) * 16)
	previous := -1
	for i, r := range word {
		position := wrappedPhase(float64(i)/float64(len(word)) + phase)
		stop := int(position * float64(len(palette)))
		if stop != previous {
			out.WriteString(palette[stop])
			previous = stop
		}
		out.WriteRune(r)
	}
	out.WriteString("\x1b[39m")
	return out.String()
}

func keywordPalette(hue [2]int) [14]string {
	var palette [14]string
	for i := range palette {
		h := float64(hue[0]) + float64(hue[1]-hue[0])*float64(i)/float64(len(palette))
		r, g, b := hslRGB(h, 0.9, 0.62)
		sequence := make([]byte, 0, len("\x1b[38;2;255;255;255m"))
		sequence = append(sequence, "\x1b[38;2;"...)
		sequence = strconv.AppendUint(sequence, uint64(r), 10)
		sequence = append(sequence, ';')
		sequence = strconv.AppendUint(sequence, uint64(g), 10)
		sequence = append(sequence, ';')
		sequence = strconv.AppendUint(sequence, uint64(b), 10)
		sequence = append(sequence, 'm')
		palette[i] = string(sequence)
	}
	return palette
}

func hslRGB(h, s, l float64) (uint8, uint8, uint8) {
	h = math.Mod(h, 360)
	if h < 0 {
		h += 360
	}
	chroma := (1 - math.Abs(2*l-1)) * s
	segment := h / 60
	x := chroma * (1 - math.Abs(math.Mod(segment, 2)-1))
	r, g, b := hueRGB(segment, chroma, x)
	m := l - chroma/2
	return colorByte(r + m), colorByte(g + m), colorByte(b + m)
}

func hueRGB(segment, chroma, x float64) (float64, float64, float64) {
	switch {
	case segment < 1:
		return chroma, x, 0
	case segment < 2:
		return x, chroma, 0
	case segment < 3:
		return 0, chroma, x
	case segment < 4:
		return 0, x, chroma
	case segment < 5:
		return x, 0, chroma
	default:
		return chroma, 0, x
	}
}

func colorByte(value float64) uint8 {
	return uint8(math.Round(value * 255))
}

func maskNonProse(input []rune) []rune {
	hidden := make([]bool, len(input))
	maskFences(input, hidden)
	maskInlineAndTags(input, hidden)
	out := slices.Clone(input)
	for i, hide := range hidden {
		if hide && out[i] != '\n' {
			out[i] = ' '
		}
	}
	return out
}

func maskFences(input []rune, hidden []bool) {
	var active fence
	for start := 0; start < len(input); {
		end := lineEnd(input, start)
		mark, ok := parseFence(input[start:end])
		if active.marker != 0 {
			blank(hidden, start, end)
			if ok && closesFence(mark, active) {
				active = fence{}
			}
		} else if ok && opensFence(mark) {
			active = mark.fence
			blank(hidden, start, end)
		}
		start = end + 1
	}
}

func lineEnd(input []rune, start int) int {
	offset := slices.Index(input[start:], '\n')
	if offset < 0 {
		return len(input)
	}
	return start + offset
}

func parseFence(line []rune) (fenceMark, bool) {
	start, ok := fenceStart(line)
	if !ok {
		return fenceMark{}, false
	}
	end := fenceRunEnd(line, start)
	if end-start < 3 {
		return fenceMark{}, false
	}
	return fenceMark{fence: fence{marker: line[start], length: end - start}, rest: line[end:]}, true
}

func fenceStart(line []rune) (int, bool) {
	start := 0
	for start < len(line) && line[start] == ' ' {
		start++
	}
	if start > 3 || start == len(line) || !fenceMarker(line[start]) {
		return 0, false
	}
	return start, true
}

func fenceMarker(r rune) bool {
	return r == '`' || r == '~'
}

func fenceRunEnd(line []rune, start int) int {
	end := start
	for end < len(line) && line[end] == line[start] {
		end++
	}
	return end
}

func opensFence(mark fenceMark) bool {
	return mark.fence.marker != '`' || !slices.Contains(mark.rest, '`')
}

func closesFence(mark fenceMark, active fence) bool {
	return mark.fence.marker == active.marker && mark.fence.length >= active.length && onlySpace(mark.rest)
}

func onlySpace(text []rune) bool {
	for _, r := range text {
		if !unicode.IsSpace(r) {
			return false
		}
	}
	return true
}

func maskInlineAndTags(input []rune, hidden []bool) {
	for i := 0; i < len(input); {
		if hidden[i] {
			i++
			continue
		}
		switch input[i] {
		case '`':
			i = maskInlineAt(input, hidden, i)
		case '<':
			i = maskTagAt(input, hidden, i)
		default:
			i++
		}
	}
}

func maskInlineAt(input []rune, hidden []bool, start int) int {
	content := backtickRunEnd(input, start)
	close := findBacktickClose(input, hidden, content, content-start)
	if close < 0 {
		return content
	}
	blank(hidden, start, close)
	return close
}

func backtickRunEnd(input []rune, start int) int {
	end := start
	for end < len(input) && input[end] == '`' {
		end++
	}
	return end
}

func findBacktickClose(input []rune, hidden []bool, start, size int) int {
	for start < len(input) {
		if hidden[start] || input[start] != '`' {
			start++
			continue
		}
		end := backtickRunEnd(input, start)
		if end-start == size {
			return end
		}
		start = end
	}
	return -1
}

func maskTagAt(input []rune, hidden []bool, start int) int {
	if end := commentEnd(input, start); end >= 0 {
		blank(hidden, start, end)
		return end
	}
	parsed, ok := parseTag(input, start)
	if !ok {
		return start + 1
	}
	blank(hidden, start, parsed.end)
	if parsed.closing || parsed.selfClosing {
		return parsed.end
	}
	close := findMatchingClose(input, hidden, parsed)
	if close < 0 {
		return parsed.end
	}
	blank(hidden, parsed.end, close)
	return close
}

func commentEnd(input []rune, start int) int {
	if !commentStart(input, start) {
		return -1
	}
	for i := start + 4; i+2 < len(input); i++ {
		if commentClose(input, i) {
			return i + 3
		}
	}
	return len(input)
}

func commentStart(input []rune, start int) bool {
	return start+3 < len(input) && input[start] == '<' && input[start+1] == '!' && input[start+2] == '-' && input[start+3] == '-'
}

func commentClose(input []rune, start int) bool {
	return input[start] == '-' && input[start+1] == '-' && input[start+2] == '>'
}

func parseTag(input []rune, start int) (tag, bool) {
	i := start + 1
	closing := i < len(input) && input[i] == '/'
	if closing {
		i++
	}
	nameStart := i
	if i >= len(input) || !asciiLetter(input[i]) {
		return tag{}, false
	}
	i++
	for i < len(input) && tagNameRune(input[i]) {
		i++
	}
	end, ok := findTagEnd(input, i)
	if !ok {
		return tag{}, false
	}
	return tag{name: input[nameStart:i], end: end + 1, closing: closing, selfClosing: input[end-1] == '/'}, true
}

func asciiLetter(r rune) bool {
	return r >= 'A' && r <= 'Z' || r >= 'a' && r <= 'z'
}

func tagNameRune(r rune) bool {
	return asciiLetter(r) || unicode.IsDigit(r) || r == '-'
}

func findTagEnd(input []rune, start int) (int, bool) {
	quote := rune(0)
	for i := start; i < len(input); i++ {
		r := input[i]
		if quote != 0 {
			if r == quote {
				quote = 0
			}
			continue
		}
		if r == '\'' || r == '"' {
			quote = r
			continue
		}
		if r == '<' {
			return 0, false
		}
		if r == '>' {
			return i, true
		}
	}
	return 0, false
}

func findMatchingClose(input []rune, hidden []bool, opening tag) int {
	depth := 1
	for i := opening.end; i < len(input); {
		if hidden[i] || input[i] != '<' {
			i++
			continue
		}
		if end := commentEnd(input, i); end >= 0 {
			i = end
			continue
		}
		candidate, ok := parseTag(input, i)
		if !ok {
			i++
			continue
		}
		depth = changedDepth(depth, opening, candidate)
		if depth == 0 {
			return candidate.end
		}
		i = candidate.end
	}
	return -1
}

func changedDepth(depth int, opening, candidate tag) int {
	if !equalASCIIFold(opening.name, candidate.name) {
		return depth
	}
	if candidate.closing {
		return depth - 1
	}
	if !candidate.selfClosing {
		return depth + 1
	}
	return depth
}

func equalASCIIFold(a, b []rune) bool {
	if len(a) != len(b) {
		return false
	}
	for i, r := range a {
		if asciiLower(r) != asciiLower(b[i]) {
			return false
		}
	}
	return true
}

func asciiLower(r rune) rune {
	if r >= 'A' && r <= 'Z' {
		return r + 'a' - 'A'
	}
	return r
}

func blank(hidden []bool, start, end int) {
	for i := start; i < end; i++ {
		hidden[i] = true
	}
}

func keywordBoundary(text []rune, start, end int) bool {
	previous := runeAt(text, start-1)
	if forbiddenPrevious(previous) || previous == ':' && runeAt(text, start-2) == ':' {
		return false
	}
	next := runeAt(text, end)
	return !forbiddenNext(next) && !forbiddenPeriod(text, end)
}

func runeAt(text []rune, index int) rune {
	if index < 0 || index >= len(text) {
		return 0
	}
	return text[index]
}

func forbiddenPeriod(text []rune, end int) bool {
	if runeAt(text, end) != '.' {
		return false
	}
	next := runeAt(text, end+1)
	return unicode.IsLetter(next) || unicode.IsDigit(next) || next == '_' || next == '-'
}

func forbiddenPrevious(r rune) bool {
	return unicode.IsLetter(r) || unicode.IsDigit(r) || r == '_' || r == '.' || r == '/' || r == '\\' || r == '-'
}

func forbiddenNext(r rune) bool {
	return unicode.IsLetter(r) || unicode.IsDigit(r) || r == '_' || r == '/' || r == '\\' || r == '-' || r == '('
}
