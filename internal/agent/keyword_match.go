package agent

import (
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/keywords"
)

type KeywordMatch struct {
	Word    string
	Start   int
	End     int
	Hue     [2]int
	Effort  string
	Summary string
}

func (h *Host) KeywordMatches(threadID, text string, kind InputKind) []KeywordMatch {
	table := keywords.Previewable(h.keywords)
	if thread, ok := h.loadedThread(threadID); ok {
		if set := thread.currentTools(); set != nil {
			table = keywords.Usable(h.keywords, set.names)
		}
	}
	spans := keywords.Spans(text, table)
	out := make([]KeywordMatch, 0, len(spans))
	for _, span := range spans {
		out = append(out, keywordMatch(span, kind))
	}
	return out
}

func keywordMatch(span keywords.Span, kind InputKind) KeywordMatch {
	match := KeywordMatch{Word: span.Keyword.Word, Start: span.Start, End: span.End, Hue: span.Keyword.Hue, Summary: "notice added"}
	if kind == InputPrompt && span.Keyword.Effort == string(config.ReasoningMax) {
		match.Effort = string(config.ReasoningMax)
		match.Summary = "effort max, this turn"
	}
	return match
}
