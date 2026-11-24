//go:build gui

package transcript

import (
	"errors"

	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document/code"
	"github.com/gosuda/dalgona/internal/dalgona/gui/figure"
)

type codeHighlight struct {
	fingerprint uint64
	language    string
	key         string
	job         uint64
	spans       []code.Span
	ready       bool
	failed      bool
}

func (t *Transcript) codeTokens(r *row, b document.Block) []code.Span {
	if b.Kind != document.KindCode || t.deps.Figures == nil {
		return nil
	}
	language, ok := code.Normalize(b.Language)
	if !ok {
		return nil
	}
	st := t.codeState(r, b, language)
	if st.ready || st.failed {
		return st.spans
	}
	if st.job != 0 {
		t.pollHighlight(st)
	} else if !b.Key.Provisional {
		t.submitHighlight(r, b, st, language, b.Projection.Text)
	}
	return st.spans
}

func (t *Transcript) codeState(r *row, b document.Block, language string) *codeHighlight {
	if r.highlights == nil {
		r.highlights = map[document.Key]*codeHighlight{}
	}
	st := r.highlights[b.Key]
	if st == nil || st.fingerprint != b.Fingerprint || st.language != language {
		if st != nil && st.job != 0 {
			t.deps.Figures.Supervisor.Cancel(st.job)
		}
		st = &codeHighlight{fingerprint: b.Fingerprint, language: language}
		r.highlights[b.Key] = st
	}
	return st
}

func (t *Transcript) submitHighlight(r *row, b document.Block, st *codeHighlight, language, source string) {
	if len(source) > figure.HighlightSourceLimit {
		st.failed = true
		t.notice = "Code highlighting needs a smaller block; showing plain code."
		return
	}
	id, key, err := t.deps.Figures.Supervisor.Highlight(figure.HighlightRequest{
		Document: r.id.Item,
		Revision: uint64(r.doc.Revision()),
		Language: language,
		Source:   []byte(source),
		Visible:  true,
	})
	if err != nil {
		if errors.Is(err, figure.ErrWaiting) || errors.Is(err, figure.ErrPaused) {
			return
		}
		st.failed = true
		t.notice = "Code highlighting failed: " + err.Error()
	}
	st.job, st.key = id, key
}

func (t *Transcript) pollHighlight(st *codeHighlight) {
	o, ok := t.deps.Figures.Result(st.key)
	if !ok || o.Job != st.job {
		return
	}
	if o.Err != nil {
		st.failed = true
		t.notice = "Code highlighting failed: " + o.Err.Error()
		return
	}
	spans := make([]code.Span, 0, len(o.Tokens))
	for _, tok := range o.Tokens {
		spans = append(spans, code.Span{Start: tok.Start, End: tok.End, Kind: figure.TokenKind(tok)})
	}
	st.spans, st.ready = spans, true
}
