package auth_test

import (
	"bytes"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/auth"
)

func TestMaskReaderPreservesWholeInputMasking(t *testing.T) {
	const credential = "edge-secret-123456789"
	const anchored = "anchor=prologue-token-9876;"
	t.Setenv("DAL_STREAM_TEST_PASSWORD", credential)
	patterns := []string{`\Aanchor=[a-z0-9-]+;`, `(?m)^configured=[a-z]+\d+$`, `edge-secret-[0-9]+`}
	legacyHome, streamHome := t.TempDir(), t.TempDir()
	legacy := openReaderMaskStore(t, legacyHome, patterns)
	streamed := openReaderMaskStore(t, streamHome, patterns)
	pad := strings.Repeat("f", (32<<10)-len(anchored)-4)
	pem := "-----BEGIN " + "RSA PRIVATE KEY-----\n" + strings.Repeat("z", 1<<20) + "\n" +
		awsKey("Z") + "\n-----END " + "RSA PRIVATE KEY-----"
	inside := "{{secret:auth:" + awsKey("X") + "}}"
	unmasked := anchored + pad + credential + " " + credential + "-longer\n" +
		"configured=abcdef123\nconfigured=zebra456\n" + anchored + " " + inside + " " +
		pem + " " + awsKey("R") + " sk-machine-learning-library sk-admin-abcd-efghijklmnopqrst"
	maskAndCompare(t, legacy, streamed, unmasked)
	if old, new := readMaskSecretsFile(t, legacyHome), readMaskSecretsFile(t, streamHome); old != new {
		t.Fatal("the streaming masker persisted different values or placeholder indices")
	}
	legacy = openReaderMaskStore(t, legacyHome, patterns)
	streamed = openReaderMaskStore(t, streamHome, patterns)
	second := awsKey("R") + " " + credential + "-longer\nconfigured=zebra456\nconfigured=novel789\n"
	masked := maskAndCompare(t, legacy, streamed, second)
	if restored, err := streamed.UnmaskStrict(masked); err != nil || restored != second {
		t.Fatalf("restart restoration = %q, %v", restored, err)
	}
}

func openReaderMaskStore(t *testing.T, home string, patterns []string) *auth.Store {
	t.Helper()
	store, err := auth.NewWithPatterns(home, patterns)
	if err != nil {
		t.Fatal(err)
	}
	if err := store.SetAPIKey("openai", "edge-secret-123456789-longer"); err != nil {
		t.Fatal(err)
	}
	return store
}

func maskAndCompare(t *testing.T, legacy, streamed *auth.Store, text string) string {
	t.Helper()
	want, err := legacy.MaskWithError(text)
	if err != nil {
		t.Fatal(err)
	}
	input := io.NewSectionReader(strings.NewReader(text), 0, int64(len(text)))
	var out bytes.Buffer
	if err := streamed.MaskReader(t.Context(), input, &out); err != nil {
		t.Fatal(err)
	}
	if got := out.String(); got != want {
		i := 0
		for i < min(len(got), len(want)) && got[i] == want[i] {
			i++
		}
		t.Fatalf("mask differs at %d: got %q, want %q", i, got[i:min(i+70, len(got))], want[i:min(i+70, len(want))])
	}
	return want
}

func TestMaskReaderDoesNotEmitUnpersistedSecret(t *testing.T) {
	home := t.TempDir()
	store := newPatternMaskStore(t, home)
	if err := os.Mkdir(filepath.Join(home, "secrets.json"), 0o700); err != nil {
		t.Fatal(err)
	}
	text := "prefix token-abcdefgh suffix"
	input := io.NewSectionReader(strings.NewReader(text), 0, int64(len(text)))
	var output bytes.Buffer
	if err := store.MaskReader(t.Context(), input, &output); err == nil || !strings.Contains(err.Error(), "persist pattern secrets") || output.Len() != 0 {
		t.Fatalf("failed secret persistence: output = %q, error = %v", output.String(), err)
	}
}

func projectSpill(t *testing.T, store *auth.Store, text string) string {
	t.Helper()
	raw := io.NewSectionReader(strings.NewReader(text), 0, int64(len(text)))
	var blob, tail, suffix bytes.Buffer
	if err := store.MaskReaderProjection(t.Context(), raw, nil, auth.ProjectionWriters{Blob: &blob, RawTail: &tail, Suffix: &suffix}); err != nil {
		t.Fatal(err)
	}
	return blob.String() + tail.String() + suffix.String()
}

func TestMaskReaderProjectionMasksOverlappingBuiltinPair(t *testing.T) {
	store := newBuiltinStore(t, t.TempDir())
	githubBody := "ABCDEFGHIJKLMNOPQRSTUVWXYZ" + "0123456789"
	text := "xoxb-" + "0123456789-" + "ghp_" + githubBody
	out := projectSpill(t, store, text)
	if strings.Contains(out, "xoxb-") {
		t.Fatalf("projection leaks the slack token: %q", out)
	}
	if strings.Contains(out, githubBody) {
		t.Fatalf("projection leaks the github token body: %q", out)
	}
}

func TestMaskReaderProjectionMasksSeparatedBuiltinPair(t *testing.T) {
	store := newBuiltinStore(t, t.TempDir())
	githubBody := strings.Repeat("B", 36)
	text := "xoxb-" + strings.Repeat("2", 10) + " ghp_" + githubBody
	out := projectSpill(t, store, text)
	if strings.Contains(out, "xoxb-") {
		t.Fatalf("projection leaks the slack token: %q", out)
	}
	if strings.Contains(out, githubBody) {
		t.Fatalf("projection leaks the github token body: %q", out)
	}
}
