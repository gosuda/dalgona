package tui

import (
	"errors"
	"strings"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

type keyword struct {
	Word string `json:"word"`
}

type keywordSpan struct {
	start int
	end   int
}

type keywordMatchParams struct {
	ThreadID string `json:"threadId,omitzero"`
	Text     string `json:"text"`
	Kind     string `json:"kind"`
}

type keywordRange struct {
	Start int `json:"start"`
	End   int `json:"end"`
}

type keywordReplyMatch struct {
	ByteRange keywordRange `json:"byteRange"`
}

type keywordReply struct {
	Matches []keywordReplyMatch `json:"matches"`
}

func (m *model) matchKind() string {
	if m.thread.state == stateRunning || m.thread.state == stateAwaitingApproval {
		return "steer"
	}
	return "prompt"
}

func (m *model) matchKeywords() tea.Cmd {
	m.matchSeq++
	draft := m.input.composer.Value()
	if draft == "" {
		m.input.marks, m.input.marksFor = nil, ""
		return nil
	}
	seq := m.matchSeq
	params := keywordMatchParams{ThreadID: m.thread.id, Text: draft, Kind: m.matchKind()}
	client, done := m.conn.client, m.conn.done
	return func() tea.Msg {
		if client == nil {
			return resultMsg{op: "keywordMatch", seq: seq, text: draft, err: errors.New("rpc client is not connected")}
		}
		ctx, cancel := requestContext(done)
		defer cancel()
		var reply keywordReply
		err := client.Call(ctx, "dalgona/keywords/match", params, &reply)
		return resultMsg{op: "keywordMatch", value: &reply, seq: seq, text: draft, err: err}
	}
}

func (m *model) applyKeywordMatch(msg resultMsg) {
	reply, ok := msg.value.(*keywordReply)
	if msg.err != nil || !ok || msg.seq != m.matchSeq || msg.text != m.input.composer.Value() {
		return
	}
	spans := make([]keywordSpan, 0, len(reply.Matches))
	for _, match := range reply.Matches {
		spans = append(spans, keywordSpan{start: match.ByteRange.Start, end: match.ByteRange.End})
	}
	m.input.marks, m.input.marksFor = spans, msg.text
}

func (m *model) draftSpans() []keywordSpan {
	if m.input.marksFor != m.input.composer.Value() {
		return nil
	}
	return m.input.marks
}

func spansOf(matches []rpc.KeywordMatch) []keywordSpan {
	spans := make([]keywordSpan, 0, len(matches))
	for _, match := range matches {
		spans = append(spans, keywordSpan{start: match.ByteRange.Start, end: match.ByteRange.End})
	}
	return spans
}

func markKeywords(text string, spans []keywordSpan, t theme) string {
	var out strings.Builder
	last := 0
	for _, span := range spans {
		if !validSpan(text, span, last) {
			continue
		}
		out.WriteString(text[last:span.start])
		out.WriteString(t.mark(text[span.start:span.end]))
		last = span.end
	}
	out.WriteString(text[last:])
	return out.String()
}

func validSpan(text string, span keywordSpan, last int) bool {
	if span.start < last || span.start >= span.end || span.end > len(text) {
		return false
	}
	return (span.start == len(text) || utf8.RuneStart(text[span.start])) && (span.end == len(text) || utf8.RuneStart(text[span.end]))
}
