package keywords

import (
	"cmp"
	"embed"
	"slices"
	"strings"
	"unicode"
	"unicode/utf8"
)

type Keyword struct {
	Word     string
	Hue      [2]int
	Requires []string
	Effort   string
	Notice   string
	Enabled  bool
}

//go:embed notices/*.md
var noticeFiles embed.FS

func builtinTable(product string) []Keyword {
	return []Keyword{
		{Word: "ultrathink", Hue: [2]int{0, 330}, Effort: "max", Notice: notice("ultrathink"), Enabled: true},
		{Word: "jevify", Hue: [2]int{300, 420}, Requires: []string{"eval"}, Effort: "default", Notice: notice("jevify"), Enabled: product == "dalgona"},
		{Word: "workflowz", Hue: [2]int{30, 150}, Requires: []string{"eval", "task"}, Effort: "default", Notice: notice("workflowz"), Enabled: product == "dalgona"},
		{Word: "orchestrate", Hue: [2]int{150, 280}, Requires: []string{"task"}, Effort: "default", Notice: notice("orchestrate"), Enabled: product == "dalgona"},
	}
}

func Builtins(product string) []Keyword {
	all := builtinTable(product)
	out := make([]Keyword, 0, len(all))
	for _, item := range all {
		if item.Enabled {
			item.Requires = slices.Clone(item.Requires)
			out = append(out, item)
		}
	}
	return out
}

func DefaultEnabled(product, word string) bool {
	for _, item := range builtinTable(product) {
		if item.Word == word {
			return item.Enabled
		}
	}
	return true
}

func Merge(builtins, custom []Keyword) []Keyword {
	out := slices.Clone(builtins)
	for _, override := range custom {
		index := slices.IndexFunc(out, func(item Keyword) bool { return item.Word == override.Word })
		if index < 0 {
			out = append(out, override)
			continue
		}
		out[index] = mergeKeyword(out[index], override)
	}
	return slices.DeleteFunc(out, func(item Keyword) bool { return !item.Enabled })
}

func mergeKeyword(base, override Keyword) Keyword {
	if override.Notice == "" {
		override.Notice = base.Notice
	}
	if override.Hue == [2]int{} {
		override.Hue = base.Hue
	}
	if override.Effort == "" {
		override.Effort = base.Effort
	}
	if override.Requires == nil {
		override.Requires = slices.Clone(base.Requires)
	}
	return override
}

type Span struct {
	Keyword    Keyword
	Start, End int
}

func Usable(table []Keyword, tools []string) []Keyword {
	available := make(map[string]struct{}, len(tools))
	for _, tool := range tools {
		available[tool] = struct{}{}
	}
	result := make([]Keyword, 0, len(table))
	for _, item := range table {
		if matchable(item, available) {
			result = append(result, item)
		}
	}
	return result
}

func Spans(prompt string, table []Keyword) []Span {
	masked := mask([]rune(prompt))
	offsets := runeOffsets(prompt)
	var spans []Span
	for _, item := range table {
		word := []rune(item.Word)
		if len(word) == 0 {
			continue
		}
		for start := 0; start+len(word) <= len(masked); {
			end := start + len(word)
			if slices.Equal(masked[start:end], word) && boundary(masked, start, end) {
				spans = append(spans, Span{Keyword: item, Start: offsets[start], End: offsets[end]})
				start = end
				continue
			}
			start++
		}
	}
	slices.SortStableFunc(spans, func(a, b Span) int { return cmp.Compare(a.Start, b.Start) })
	return spans
}

func runeOffsets(text string) []int {
	offsets := make([]int, 0, utf8.RuneCountInString(text)+1)
	for i := range text {
		offsets = append(offsets, i)
	}
	return append(offsets, len(text))
}

func Match(prompt string, table []Keyword, tools []string) []Keyword {
	usable := Usable(table, tools)
	matched := make(map[string]bool, len(usable))
	for _, span := range Spans(prompt, usable) {
		matched[span.Keyword.Word] = true
	}
	result := make([]Keyword, 0, len(usable))
	for _, item := range usable {
		if matched[item.Word] {
			result = append(result, item)
		}
	}
	return result
}

func matchable(item Keyword, available map[string]struct{}) bool {
	return usableWord(item) && requirementsMet(item.Requires, available)
}

func usableWord(item Keyword) bool {
	return item.Enabled && item.Word != "" && item.Word == strings.ToLower(item.Word)
}

func Previewable(table []Keyword) []Keyword {
	result := make([]Keyword, 0, len(table))
	for _, item := range table {
		if usableWord(item) {
			result = append(result, item)
		}
	}
	return result
}

func notice(word string) string {
	data, err := noticeFiles.ReadFile("notices/" + word + ".md")
	if err != nil {
		panic("keywords: missing notice notices/" + word + ".md: " + err.Error())
	}
	text := strings.TrimSpace(string(data))
	if text == "" {
		panic("keywords: empty notice notices/" + word + ".md")
	}
	return text
}

func requirementsMet(requirements []string, available map[string]struct{}) bool {
	for _, required := range requirements {
		if _, ok := available[required]; !ok {
			return false
		}
	}
	return true
}

type fence struct {
	marker rune
	length int
}

type fenceMark struct {
	fence fence
	rest  []rune
}

func mask(input []rune) []rune {
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
	active := fence{}
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

type tag struct {
	name        []rune
	end         int
	closing     bool
	selfClosing bool
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

func boundary(text []rune, start, end int) bool {
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
