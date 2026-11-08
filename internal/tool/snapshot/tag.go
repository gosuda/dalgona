package snapshot

import (
	"crypto/sha256"
	"encoding/hex"
	"strings"
)

func Tag(domain string, data []byte) string {
	hash := sha256.New()
	_, _ = hash.Write([]byte(domain))
	_, _ = hash.Write([]byte(":"))
	_, _ = hash.Write(data)
	return strings.ToUpper(hex.EncodeToString(hash.Sum(nil)[:4]))
}

func VersionTag(data []byte) string {
	return Tag("version", data)[:4]
}
