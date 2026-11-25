package figure

import (
	"github.com/alecthomas/chroma/v2"

	"github.com/gosuda/dalgona/internal/dalgona/gui/document/code"
)

func TokenKind(t Token) code.Kind {
	return code.KindOf(chroma.TokenType(t.Type))
}
