package tui

import (
	"strconv"
	"strings"
)

type block struct {
	text string
	code bool
}

type blockSplitter struct {
	open       []string
	rest       string
	kind       string
	afterBlank bool
	fence      string
}

const (
	kindPara  = "para"
	kindList  = "list"
	kindTable = "table"
	kindQuote = "quote"
)

func (s *blockSplitter) Feed(delta string) ([]block, string) {
	s.rest += delta
	var final []block
	for {
		i := strings.IndexByte(s.rest, '\n')
		if i < 0 {
			break
		}
		line := s.rest[:i]
		s.rest = s.rest[i+1:]
		final = append(final, s.line(line)...)
	}
	return final, s.tail()
}

func (s *blockSplitter) Close() []block {
	var final []block
	if s.rest != "" {
		final = append(final, s.line(s.rest)...)
		s.rest = ""
	}
	return append(final, s.flush()...)
}

func (s *blockSplitter) tail() string {
	if s.rest != "" {
		return s.rest
	}
	if len(s.open) > 0 {
		return s.open[len(s.open)-1]
	}
	return ""
}

func (s *blockSplitter) line(text string) []block {
	if s.fence != "" {
		return s.fenceLine(text)
	}
	switch {
	case strings.TrimSpace(text) == "":
		return s.blank()
	case isHeading(text), isRule(text):
		return append(s.flush(), block{text: text})
	}
	if marker, ok := fenceOpener(text); ok {
		s.fence = marker
		return append(s.flush(), block{text: text, code: true})
	}
	return s.join(text)
}

func (s *blockSplitter) fenceLine(text string) []block {
	trimmed := strings.TrimSpace(text)
	if trimmed != "" && strings.Trim(trimmed, s.fence[:1]) == "" && len(trimmed) >= len(s.fence) {
		s.fence = ""
	}
	return []block{{text: text, code: true}}
}

func (s *blockSplitter) blank() []block {
	switch s.kind {
	case kindPara:
		return s.flush()
	case kindList, kindTable, kindQuote:
		s.afterBlank = true
	}
	return nil
}

func (s *blockSplitter) join(text string) []block {
	var final []block
	if s.afterBlank && !belongs(s.kind, text) {
		final = s.flush()
	}
	s.afterBlank = false
	if len(s.open) == 0 {
		s.kind = kindOf(text)
	}
	s.open = append(s.open, text)
	return final
}

func (s *blockSplitter) flush() []block {
	if len(s.open) == 0 {
		s.kind = ""
		s.afterBlank = false
		return nil
	}
	text := strings.Join(s.open, "\n")
	s.open = nil
	s.kind = ""
	s.afterBlank = false
	return []block{{text: text}}
}

func isHeading(text string) bool {
	trimmed := strings.TrimLeft(text, " ")
	n := len(trimmed) - len(strings.TrimLeft(trimmed, "#"))
	return n >= 1 && n <= 6 && (len(trimmed) == n || trimmed[n] == ' ' || trimmed[n] == '\t')
}

func isRule(text string) bool {
	compact := strings.ReplaceAll(strings.TrimSpace(text), " ", "")
	return len(compact) >= 3 && strings.ContainsAny(compact[:1], "-*_") && strings.Trim(compact, compact[:1]) == ""
}

func fenceOpener(text string) (string, bool) {
	indent := len(text) - len(strings.TrimLeft(text, " "))
	if indent > 3 {
		return "", false
	}
	body := text[indent:]
	if len(body) < 3 || (body[0] != '`' && body[0] != '~') {
		return "", false
	}
	n := len(body) - len(strings.TrimLeft(body, body[:1]))
	if n < 3 {
		return "", false
	}
	return body[:n], true
}

func kindOf(text string) string {
	switch {
	case isListMarker(text):
		return kindList
	case strings.HasPrefix(strings.TrimLeft(text, " "), "|"):
		return kindTable
	case strings.HasPrefix(strings.TrimLeft(text, " "), ">"):
		return kindQuote
	}
	return kindPara
}

func belongs(kind, text string) bool {
	switch kind {
	case kindList:
		return isListMarker(text) || strings.HasPrefix(text, "  ")
	case kindTable:
		return strings.HasPrefix(strings.TrimLeft(text, " "), "|")
	case kindQuote:
		return strings.HasPrefix(strings.TrimLeft(text, " "), ">")
	}
	return false
}

func isListMarker(text string) bool {
	trimmed := strings.TrimLeft(text, " ")
	if len(trimmed) >= 2 && strings.ContainsRune("-*+", rune(trimmed[0])) && trimmed[1] == ' ' {
		return true
	}
	digits := len(trimmed) - len(strings.TrimLeft(trimmed, "0123456789"))
	return digits > 0 && digits+1 < len(trimmed) && (trimmed[digits] == '.' || trimmed[digits] == ')') && trimmed[digits+1] == ' '
}

func (m *model) streamAnswer(threadID string, item *liveItem, raw []byte) {
	item.raw = append(item.raw, raw...)
	streamed := appendSanitized(nil, item.raw)
	fragment := streamed[len(item.text):]
	item.text = streamed
	final, tail := item.split.Feed(string(fragment))
	item.streamTail = tail
	m.commitBlocks(threadID, item, final)
}

func (m *model) finishAnswer(threadID string, item *liveItem, final string) {
	var blocks []block
	if streamed := string(item.text); strings.HasPrefix(final, streamed) {
		blocks, _ = item.split.Feed(final[len(streamed):])
	}
	blocks = append(blocks, item.split.Close()...)
	m.commitBlocks(threadID, item, blocks)
	item.streamTail = ""
}

func (m *model) commitBlocks(threadID string, item *liveItem, blocks []block) {
	for _, b := range blocks {
		m.commit("block:"+threadID+":"+item.ID+":"+strconv.Itoa(item.blocks), m.renderBlock(b))
		item.blocks++
	}
}
