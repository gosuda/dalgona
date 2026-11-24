package code

import (
	"testing"

	"github.com/alecthomas/chroma/v2"
)

func TestKindOfMapsChromaTypesToRegisteredKinds(t *testing.T) {
	cases := map[chroma.TokenType]Kind{
		chroma.Keyword:         Keyword,
		chroma.KeywordType:     Keyword,
		chroma.NameFunction:    Name,
		chroma.NameVariable:    Name,
		chroma.LiteralString:   String,
		chroma.LiteralNumber:   Number,
		chroma.Operator:        Operator,
		chroma.Punctuation:     Punctuation,
		chroma.CommentSingle:   Comment,
		chroma.GenericStrong:   Generic,
		chroma.GenericInserted: Added,
		chroma.GenericDeleted:  Removed,
		chroma.Error:           Error,
		chroma.Text:            PlainText,
	}
	for token, want := range cases {
		if got := KindOf(token); got != want {
			t.Errorf("KindOf(%v) = %v, want %v", token, got, want)
		}
	}
}
