package tool

import (
	"fmt"
	"math/rand/v2"
	"strings"
	"testing"
	"unicode/utf8"
)

const projectionSpill = "blobs/abc"

type projectionCase struct {
	data      []byte
	keep      Keep
	got       string
	truncated bool
}

func TestProjectProperty(t *testing.T) {
	source := rand.NewChaCha8([32]byte{20, 26, 11, 8})
	random := rand.New(source)
	for range 200 {
		data := randomProjectionInput(t, random, source)
		for _, keep := range []Keep{KeepHead, KeepTail} {
			got, truncated := Project(data, keep, projectionSpill)
			checkProjection(t, projectionCase{data: data, keep: keep, got: got, truncated: truncated})
		}
	}
}

func randomProjectionInput(t *testing.T, random *rand.Rand, source *rand.ChaCha8) []byte {
	t.Helper()
	size := random.IntN(256*1024) + 1
	switch random.IntN(3) {
	case 0:
		return []byte(strings.Repeat("aé世🙂", size/10+1))[:size]
	case 1:
		return []byte(strings.Repeat("aé世🙂", size/10+1))
	default:
		data := make([]byte, size)
		if _, err := source.Read(data); err != nil {
			t.Fatal(err)
		}
		return data
	}
}

func checkProjection(t *testing.T, c projectionCase) {
	t.Helper()
	if len(c.data) <= projectionLimit {
		if c.truncated || c.got != string(c.data) {
			t.Fatalf("small input truncated=%v", c.truncated)
		}
		return
	}
	if !c.truncated {
		t.Fatal("large input reported whole")
	}
	if utf8.Valid(c.data) && !utf8.ValidString(c.got) {
		t.Fatalf("valid input lost UTF-8 for keep %d", c.keep)
	}
	if len(c.got) > projectionLimit+256 {
		t.Fatalf("projection too large: %d", len(c.got))
	}
	checkProjectionWindow(t, c)
}

func checkProjectionWindow(t *testing.T, c projectionCase) {
	t.Helper()
	notice := fmt.Sprintf("\n[output truncated: %d bytes total; full output: %s; page it with read offset/limit]", len(c.data), projectionSpill)
	body, ok := strings.CutSuffix(c.got, notice)
	if !ok {
		t.Fatalf("projection notice = %q", c.got[max(0, len(c.got)-200):])
	}
	if len(body) < 64*1024-3 || len(body) > 64*1024 {
		t.Fatalf("window length = %d", len(body))
	}
	source := string(c.data)
	if c.keep == KeepHead && !strings.HasPrefix(source, body) {
		t.Fatal("head projection is not an input prefix")
	}
	if c.keep == KeepTail && !strings.HasSuffix(source, body) {
		t.Fatal("tail projection is not an input suffix")
	}
}
