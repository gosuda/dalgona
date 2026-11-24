package figure

import (
	"strings"
	"testing"

	"github.com/alecthomas/chroma/v2"

	"github.com/gosuda/dalgona/internal/dalgona/gui/document/code"
)

func TestTokenKindFollowsTheSharedClassifier(t *testing.T) {
	if got := TokenKind(Token{Start: 0, End: 5, Type: uint32(chroma.Keyword)}); got != code.Keyword {
		t.Fatalf("TokenKind(keyword) = %v, want keyword", got)
	}
	if got := TokenKind(Token{Start: 5, End: 9, Type: uint32(chroma.Text)}); got != code.PlainText {
		t.Fatalf("TokenKind(text) = %v, want plain text", got)
	}
}

func TestRenderRefusesWorkWhenTheQueueIsSaturated(t *testing.T) {
	sup := &Supervisor{}
	for range PendingJobLimit {
		sup.queue = append(sup.queue, &job{})
	}
	_, _, err := sup.Render(Request{Kind: KindMermaid, Source: []byte(strings.Repeat("graph TD\nA-->B\n", 8))})
	if err == nil || !strings.Contains(err.Error(), "waiting") {
		t.Fatalf("saturated render returned %v, want the waiting signal", err)
	}
}
