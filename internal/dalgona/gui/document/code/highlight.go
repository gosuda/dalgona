package code

import (
	"fmt"
	"image/color"
	"unicode/utf8"

	"github.com/alecthomas/chroma/v2"
	"github.com/alecthomas/chroma/v2/lexers"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const MaxHighlightBytes = 4 << 20

type Kind uint8

const (
	PlainText Kind = iota
	Keyword
	Name
	String
	Number
	Comment
	Operator
	Punctuation
	Generic
	Added
	Removed
	Error
)

func (k Kind) Color(p theme.Palette) color.NRGBA {
	switch k {
	case Keyword:
		return p.AccentText
	case String, Added:
		return p.AddedText
	case Number:
		return p.WarningText
	case Comment, Punctuation:
		return p.Dim
	case Removed, Error:
		return p.FailedText
	case PlainText, Name, Operator, Generic:
		return p.Text
	default:
		return p.Text
	}
}

type Span struct {
	Start int
	End   int
	Kind  Kind
}

type Block struct {
	Language string
	Label    string
	Known    bool
	Spans    []Span
}

type EncodingError struct {
	Language string
	Bytes    int
}

func (e *EncodingError) Error() string {
	return fmt.Sprintf("code: refuse %s block of %d bytes: invalid UTF-8; send valid UTF-8 source", e.Language, e.Bytes)
}

type BudgetError struct {
	Name     string
	Language string
	Bytes    int
	Limit    int
}

func (e *BudgetError) Error() string {
	return fmt.Sprintf("code: %s: %s block of %d bytes exceeds the %d-byte highlighting budget; showing plain code", e.Name, e.Language, e.Bytes, e.Limit)
}

func plain(source string) []Span {
	if len(source) == 0 {
		return nil
	}
	return []Span{{Start: 0, End: len(source), Kind: PlainText}}
}

func Highlight(source, language string) (Block, error) {
	label := language
	canonical, known := Normalize(language)
	block := Block{Language: canonical, Label: label, Known: known}
	if !utf8.ValidString(source) {
		return block, &EncodingError{Language: label, Bytes: len(source)}
	}
	if len(source) > MaxHighlightBytes {
		block.Spans = plain(source)
		return block, &BudgetError{Name: "highlight-budget", Language: label, Bytes: len(source), Limit: MaxHighlightBytes}
	}
	if !known {
		block.Spans = plain(source)
		return block, nil
	}
	lexer := lexers.Get(canonical)
	if lexer == nil {
		block.Spans = plain(source)
		return block, nil
	}
	spans, ok := lexSpans(lexer, source)
	block.Spans = spans
	if !ok {
		return block, &BudgetError{Name: "highlight-lexer", Language: label, Bytes: len(source), Limit: MaxHighlightBytes}
	}
	return block, nil
}

func lexSpans(lexer chroma.Lexer, source string) ([]Span, bool) {
	it, err := lexer.Tokenise(&chroma.TokeniseOptions{State: "root", EnsureLF: false}, source)
	if err != nil {
		return plain(source), false
	}
	var spans []Span
	offset := 0
	for token := it(); token != chroma.EOF; token = it() {
		if len(token.Value) == 0 {
			continue
		}
		end := offset + len(token.Value)
		if end > len(source) {
			return plain(source), false
		}
		spans = append(spans, Span{Start: offset, End: end, Kind: classify(token.Type)})
		offset = end
	}
	if offset != len(source) {
		return plain(source), false
	}
	if len(spans) == 0 {
		return plain(source), true
	}
	return spans, true
}

func KindOf(t chroma.TokenType) Kind {
	return classify(t)
}

func classify(t chroma.TokenType) Kind {
	if kind, ok := classifySpecial(t); ok {
		return kind
	}
	if kind, ok := classifyWord(t); ok {
		return kind
	}
	if kind, ok := classifyLiteral(t); ok {
		return kind
	}
	if kind, ok := classifySymbol(t); ok {
		return kind
	}
	return PlainText
}

func classifySpecial(t chroma.TokenType) (Kind, bool) {
	if t == chroma.Error {
		return Error, true
	}
	if t == chroma.GenericDeleted {
		return Removed, true
	}
	if t == chroma.GenericInserted {
		return Added, true
	}
	return PlainText, false
}

func classifyWord(t chroma.TokenType) (Kind, bool) {
	switch {
	case t >= chroma.Keyword && t < chroma.Name:
		return Keyword, true
	case t >= chroma.Name && t < chroma.Literal:
		return Name, true
	}
	return PlainText, false
}

func classifyLiteral(t chroma.TokenType) (Kind, bool) {
	switch {
	case t >= chroma.LiteralString && t < chroma.LiteralNumber:
		return String, true
	case t >= chroma.LiteralNumber && t < chroma.Operator:
		return Number, true
	case t == chroma.Literal || t == chroma.LiteralDate || t == chroma.LiteralOther:
		return Number, true
	}
	return PlainText, false
}

func classifySymbol(t chroma.TokenType) (Kind, bool) {
	switch {
	case t >= chroma.Operator && t < chroma.Punctuation:
		return Operator, true
	case t >= chroma.Punctuation && t < chroma.Comment:
		return Punctuation, true
	case t >= chroma.Comment && t < chroma.Generic:
		return Comment, true
	case t >= chroma.Generic && t < chroma.Text:
		return Generic, true
	}
	return PlainText, false
}
