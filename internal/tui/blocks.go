package tui

import (
	"bytes"
	"strconv"
	"strings"
	"unicode/utf8"
)

type block struct {
	text    string
	code    bool
	diagram string
}

type blockSplitter struct {
	open       []string
	rest       []byte
	kind       string
	afterBlank bool
	fence      string
	fenceLang  string
	fenceBuf   []string
}

const streamTailBytes = 64 << 10

const maxStreamPending = 4 << 10

const (
	controlNone   = 0
	controlCSI    = 1
	controlString = 2
	controlShort  = 3
)

const (
	kindPara  = "para"
	kindList  = "list"
	kindTable = "table"
	kindQuote = "quote"
)

func (s *blockSplitter) Feed(delta string) ([]block, string) {
	base := len(s.rest)
	s.rest = append(s.rest, delta...)
	start, pos := 0, base
	var final []block
	for {
		i := bytes.IndexByte(s.rest[pos:], '\n')
		if i < 0 {
			break
		}
		nl := pos + i
		final = append(final, s.line(string(s.rest[start:nl]))...)
		start, pos = nl+1, nl+1
	}
	if start > 0 {
		s.rest = s.rest[start:]
	}
	return final, s.tail()
}

func (s *blockSplitter) Close() []block {
	var final []block
	if len(s.rest) > 0 {
		final = append(final, s.line(string(s.rest))...)
		s.rest = nil
	}
	if s.fenceBuf != nil {
		for _, text := range s.fenceBuf {
			final = append(final, block{text: text, code: true})
		}
		s.fenceBuf = nil
		s.fence = ""
		s.fenceLang = ""
	}
	return append(final, s.flush()...)
}

func (s *blockSplitter) tail() string {
	if len(s.rest) > 0 {
		return capTailBytes(s.rest)
	}
	if len(s.open) > 0 {
		return capTailString(s.open[len(s.open)-1])
	}
	return ""
}

func capTailBytes(b []byte) string {
	if len(b) <= streamTailBytes {
		return string(b)
	}
	start := len(b) - streamTailBytes
	for start < len(b) && !utf8.RuneStart(b[start]) {
		start++
	}
	return string(b[start:])
}

func capTailString(text string) string {
	if len(text) <= streamTailBytes {
		return text
	}
	start := len(text) - streamTailBytes
	for start < len(text) && !utf8.RuneStart(text[start]) {
		start++
	}
	return text[start:]
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
		lang := strings.Fields(strings.TrimSpace(text[strings.Index(text, marker)+len(marker):]))
		if len(lang) > 0 && isDiagramLang(lang[0]) {
			s.fenceLang = strings.ToLower(lang[0])
			s.fenceBuf = []string{text}
			return s.flush()
		}
		return append(s.flush(), block{text: text, code: true})
	}
	return s.join(text)
}

func (s *blockSplitter) fenceLine(text string) []block {
	trimmed := strings.TrimSpace(text)
	closing := trimmed != "" && strings.Trim(trimmed, s.fence[:1]) == "" && len(trimmed) >= len(s.fence)
	if s.fenceBuf != nil {
		s.fenceBuf = append(s.fenceBuf, text)
		if !closing {
			return nil
		}
		source := strings.Join(s.fenceBuf[1:len(s.fenceBuf)-1], "\n")
		out := block{text: source, code: true, diagram: s.fenceLang}
		s.fence = ""
		s.fenceLang = ""
		s.fenceBuf = nil
		return []block{out}
	}
	if closing {
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
	if item.pendingCtl != controlNone {
		raw = item.discardControl(raw)
		if len(raw) == 0 {
			return
		}
	}
	before := len(item.text)
	if len(item.pending) == 0 {
		safe := streamSafeLen(raw)
		item.text = appendSanitized(item.text, raw[:safe])
		item.setPending(raw[safe:])
	} else {
		item.pending = append(item.pending, raw...)
		safe := streamSafeLen(item.pending)
		item.text = appendSanitized(item.text, item.pending[:safe])
		item.setPending(item.pending[safe:])
	}
	final, tail := item.split.Feed(string(item.text[before:]))
	item.streamTail = tail
	m.commitBlocks(threadID, item, final)
}

func (l *liveItem) setPending(rest []byte) {
	if len(rest) > maxStreamPending {
		if len(rest) >= 2 && rest[0] == 0x1b {
			l.startControlDiscard(rest)
		} else {
			owned := make([]byte, maxStreamPending)
			copy(owned, rest[len(rest)-maxStreamPending:])
			l.pending = owned
		}
		return
	}
	if len(rest) == 0 {
		if cap(l.pending) > maxStreamPending {
			l.pending = nil
		} else {
			l.pending = l.pending[:0]
		}
		return
	}
	if cap(l.pending) > maxStreamPending {
		owned := make([]byte, len(rest))
		copy(owned, rest)
		l.pending = owned
		return
	}
	l.pending = append(l.pending[:0], rest...)
}

func (l *liveItem) startControlDiscard(rest []byte) {
	l.pendingCtl = controlKindOf(rest)
	l.pending = nil
	if l.pendingCtl == controlString && rest[len(rest)-1] == 0x1b {
		l.pending = []byte{0x1b}
	}
}

func controlKindOf(rest []byte) byte {
	switch rest[1] {
	case '[':
		return controlCSI
	case ']', 'P', 'X', '^', '_':
		return controlString
	default:
		return controlShort
	}
}

func (l *liveItem) discardControl(raw []byte) []byte {
	switch l.pendingCtl {
	case controlCSI:
		return l.discardCSI(raw)
	case controlShort:
		return l.discardShort(raw)
	case controlString:
		return l.discardString(raw)
	default:
		return raw
	}
}

func (l *liveItem) discardCSI(raw []byte) []byte {
	for i, b := range raw {
		if b >= 0x40 && b <= 0x7e {
			l.pendingCtl = controlNone
			return raw[i+1:]
		}
	}
	return nil
}

func (l *liveItem) discardShort(raw []byte) []byte {
	i := 0
	for i < len(raw) && raw[i] >= 0x20 && raw[i] <= 0x2f {
		i++
	}
	if i < len(raw) {
		l.pendingCtl = controlNone
		return raw[i+1:]
	}
	return nil
}

func (l *liveItem) discardString(raw []byte) []byte {
	if len(l.pending) == 1 && l.pending[0] == 0x1b {
		if len(raw) == 0 {
			return nil
		}
		l.pending = l.pending[:0]
		if raw[0] == '\\' {
			l.pendingCtl = controlNone
			return raw[1:]
		}
	}
	end := controlStringEnd(raw)
	if end >= 0 {
		l.pendingCtl = controlNone
		return raw[end:]
	}
	if len(raw) > 0 && raw[len(raw)-1] == 0x1b {
		l.pending = []byte{0x1b}
	}
	return nil
}

func controlStringEnd(raw []byte) int {
	for i := 0; i < len(raw); i++ {
		if raw[i] == 0x07 {
			return i + 1
		}
		if raw[i] == 0x1b && i+1 < len(raw) && raw[i+1] == '\\' {
			return i + 2
		}
	}
	return -1
}

func streamSafeLen(buf []byte) int {
	safe := len(buf) - utf8Holdback(buf)
	for i := 0; i < safe; {
		if buf[i] != 0x1b {
			_, size := utf8.DecodeRune(buf[i:safe])
			i += size
			continue
		}
		end, ok := escapeEnd(buf, i)
		if !ok {
			return i
		}
		i = end
	}
	return safe
}

func utf8Holdback(buf []byte) int {
	i := len(buf)
	start := i
	for start > 0 && i-start < utf8.UTFMax-1 && buf[start-1]&0xc0 == 0x80 {
		start--
	}
	if start == i {
		return utf8TailByte(buf)
	}
	if start == 0 {
		return 0
	}
	size := utf8LeadSize(buf[start-1])
	if size == 0 || i-start+1 >= size {
		return 0
	}
	return i - start + 1
}

func utf8TailByte(buf []byte) int {
	if len(buf) == 0 {
		return 0
	}
	b := buf[len(buf)-1]
	if b < 0x80 || b >= 0xf8 || b&0xc0 == 0x80 {
		return 0
	}
	return 1
}

func utf8LeadSize(b byte) int {
	switch {
	case b < 0x80:
		return 1
	case b < 0xc0:
		return 0
	case b < 0xe0:
		return 2
	case b < 0xf0:
		return 3
	case b < 0xf8:
		return 4
	}
	return 0
}

func escapeEnd(buf []byte, i int) (int, bool) {
	j := i + 1
	if j >= len(buf) {
		return 0, false
	}
	switch buf[j] {
	case '[':
		return csiEnd(buf, j+1)
	case ']', 'P', 'X', '^', '_':
		return stringEnd(buf, j+1)
	default:
		return paramEnd(buf, j)
	}
}

func csiEnd(buf []byte, j int) (int, bool) {
	for j < len(buf) && (buf[j] < 0x40 || buf[j] > 0x7e) {
		j++
	}
	if j >= len(buf) {
		return 0, false
	}
	return j + 1, true
}

func stringEnd(buf []byte, j int) (int, bool) {
	for ; j < len(buf); j++ {
		if buf[j] == 0x07 {
			return j + 1, true
		}
		if buf[j] == 0x1b && j+1 < len(buf) && buf[j+1] == '\\' {
			return j + 2, true
		}
	}
	return 0, false
}

func paramEnd(buf []byte, j int) (int, bool) {
	for ; j < len(buf) && buf[j] >= 0x20 && buf[j] <= 0x2f; j++ {
	}
	if j >= len(buf) {
		return 0, false
	}
	return j + 1, true
}

func (m *model) finishAnswer(threadID string, item *liveItem, final string) {
	var blocks []block
	if streamed := string(item.text); strings.HasPrefix(final, streamed) {
		blocks, _ = item.split.Feed(final[len(streamed):])
	}
	blocks = append(blocks, item.split.Close()...)
	m.commitBlocks(threadID, item, blocks)
	item.pending = nil
	item.pendingCtl = controlNone
	item.streamTail = ""
}

func (m *model) commitBlocks(threadID string, item *liveItem, blocks []block) {
	for _, b := range blocks {
		m.commit("block:"+threadID+":"+item.ID+":"+strconv.Itoa(item.blocks), m.renderBlock(b))
		item.blocks++
	}
}
