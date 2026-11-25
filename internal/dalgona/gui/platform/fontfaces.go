package platform

import (
	"crypto/sha256"
	_ "embed"
	"encoding/hex"
	"fmt"
	"sync"
)

const stixTwoMathDigest = "3a5f3f26f40d5698b3c62dd085d48d6663696a3f80825aab8b553d5097518e8c"

var (
	//go:embed fontfaces/HankenGrotesk-400.ttf
	hanken400 []byte
	//go:embed fontfaces/Pretendard-400.otf
	pretendard400 []byte
	//go:embed fontfaces/STIXTwoMath-Regular.otf
	stixTwoMath []byte
)

var MathFontData = sync.OnceValues(func() ([]byte, error) {
	return verifiedFace("STIX Two Math", stixTwoMath, stixTwoMathDigest)
})

func MathTextFaceData() [][]byte {
	return [][]byte{hanken400, pretendard400}
}

func verifiedFace(name string, data []byte, digest string) ([]byte, error) {
	sum := sha256.Sum256(data)
	if got := hex.EncodeToString(sum[:]); got != digest {
		return nil, fmt.Errorf("bundled font %s has SHA-256 %s instead of %s; restore the face recorded in fontfaces/SHA256SUMS", name, got, digest)
	}
	return data, nil
}
