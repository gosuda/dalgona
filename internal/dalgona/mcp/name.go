package mcp

import (
	"crypto/sha256"
	"encoding/hex"
	"strings"
)

const maxToolName = 64

func toolName(server, tool string) string {
	joined := server + "__" + tool
	safe := safeToolName(joined)
	if safe == joined && len(safe) <= maxToolName {
		return safe
	}
	sum := sha256.Sum256([]byte(joined))
	suffix := "_" + hex.EncodeToString(sum[:4])
	return safe[:min(len(safe), maxToolName-len(suffix))] + suffix
}

func safeToolName(name string) string {
	var b strings.Builder
	for i, r := range name {
		if i == 0 && !isASCIILetter(r) && r != '_' {
			b.WriteByte('_')
		}
		if isNameRune(r) {
			b.WriteRune(r)
		} else {
			b.WriteByte('_')
		}
	}
	return b.String()
}

func isASCIILetter(r rune) bool {
	return r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z'
}

func isNameRune(r rune) bool {
	return isASCIILetter(r) || r >= '0' && r <= '9' || r == '_' || r == '-'
}
