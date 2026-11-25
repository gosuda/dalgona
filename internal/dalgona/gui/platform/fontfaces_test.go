package platform

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"os"
	"strings"
	"testing"
)

func recordedDigest(t *testing.T, name string) string {
	t.Helper()
	sums, err := os.ReadFile("fontfaces/SHA256SUMS")
	if err != nil {
		t.Fatal(err)
	}
	for line := range strings.Lines(string(sums)) {
		digest, file, ok := strings.Cut(strings.TrimSpace(line), "  ")
		if ok && file == name {
			return digest
		}
	}
	t.Fatalf("fontfaces/SHA256SUMS records no digest for %s", name)
	return ""
}

func TestBundledMathFaceMatchesItsRecordedDigest(t *testing.T) {
	data, err := MathFontData()
	if err != nil {
		t.Fatal(err)
	}
	sum := sha256.Sum256(data)
	if got, want := hex.EncodeToString(sum[:]), recordedDigest(t, "STIXTwoMath-Regular.otf"); got != want {
		t.Fatalf("embedded STIX Two Math has SHA-256 %s, SHA256SUMS records %s", got, want)
	}
	file, err := os.ReadFile("fontfaces/STIXTwoMath-Regular.otf")
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(file, data) {
		t.Fatal("MathFontData differs from fontfaces/STIXTwoMath-Regular.otf")
	}
}

func TestTamperedBundledFaceIsRefused(t *testing.T) {
	data, err := MathFontData()
	if err != nil {
		t.Fatal(err)
	}
	tampered := bytes.Clone(data)
	tampered[len(tampered)/2] ^= 1
	_, err = verifiedFace("STIX Two Math", tampered, stixTwoMathDigest)
	if err == nil || !strings.Contains(err.Error(), "STIX Two Math") {
		t.Fatalf("verifiedFace accepted a changed face or did not name it: %v", err)
	}
}

func TestMathTextFacesAreTheBundledDocumentFaces(t *testing.T) {
	faces := MathTextFaceData()
	want := []string{"HankenGrotesk-400.ttf", "Pretendard-400.otf"}
	if len(faces) != len(want) {
		t.Fatalf("MathTextFaceData has %d faces, want %d", len(faces), len(want))
	}
	for i, name := range want {
		sum := sha256.Sum256(faces[i])
		if got := hex.EncodeToString(sum[:]); got != recordedDigest(t, name) {
			t.Errorf("math text face %d is not %s", i, name)
		}
	}
}
