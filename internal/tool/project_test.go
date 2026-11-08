package tool_test

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"math/rand/v2"
	"strings"
	"testing"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/tool"
)

const testProjectionLimit = 64 * 1024

func TestProject_property(t *testing.T) {
	t.Parallel()
	random := rand.New(rand.NewPCG(19, 77))
	for _, keep := range []tool.Keep{tool.KeepHead, tool.KeepTail} {
		for index := range 200 {
			size := random.IntN(256*1024 + 1)
			input := randomUTF8(random, size)
			if index%2 == 0 {
				input = randomBytes(random, size)
			}
			checkProjectionCase(t, projectionCase{keep: keep, index: index, input: input})
		}
	}
}

type projectionCase struct {
	keep  tool.Keep
	index int
	input []byte
}

func checkProjectionCase(t *testing.T, test projectionCase) {
	t.Helper()
	got, truncated := tool.Project(test.input, test.keep, "spill")
	if len(test.input) <= testProjectionLimit {
		checkProjectionIdentity(t, test, got, truncated)
		return
	}
	notice := fmt.Sprintf("\n[output truncated: %d bytes total; full output: spill; page it with read offset/limit]", len(test.input))
	if !truncated || !strings.HasSuffix(got, notice) {
		t.Fatalf("keep %v case %d: missing truncation notice", test.keep, test.index)
	}
	if len(got) > testProjectionLimit+len(notice) {
		t.Fatalf("keep %v case %d: projected length %d", test.keep, test.index, len(got))
	}
	if utf8.Valid(test.input) {
		checkUTF8Projection(t, test, got, notice)
	}
}

func checkProjectionIdentity(t *testing.T, test projectionCase, got string, truncated bool) {
	t.Helper()
	if truncated || got != string(test.input) {
		t.Fatalf("keep %v case %d: identity failed", test.keep, test.index)
	}
}

func checkUTF8Projection(t *testing.T, test projectionCase, got string, notice string) {
	t.Helper()
	want := expectedProjection(string(test.input), test.keep) + notice
	if got != want {
		t.Fatalf("keep %v case %d: projection differs from rune oracle", test.keep, test.index)
	}
	if !utf8.ValidString(got) {
		t.Fatalf("keep %v case %d: valid input became invalid", test.keep, test.index)
	}
}

func TestProject_runeBoundaries(t *testing.T) {
	t.Parallel()
	headInput := []byte(strings.Repeat("a", testProjectionLimit-1) + "€" + strings.Repeat("b", 20))
	head, _ := tool.Project(headInput, tool.KeepHead, "s")
	if !strings.HasPrefix(head, strings.Repeat("a", testProjectionLimit-1)+"\n[output truncated:") {
		t.Fatal("head projection kept part of a rune")
	}
	tailInput := []byte(strings.Repeat("b", 20) + "€" + strings.Repeat("a", testProjectionLimit-1))
	tail, _ := tool.Project(tailInput, tool.KeepTail, "s")
	if !strings.HasPrefix(tail, strings.Repeat("a", testProjectionLimit-1)+"\n[output truncated:") {
		t.Fatal("tail projection kept part of a rune")
	}
}

func randomBytes(random *rand.Rand, size int) []byte {
	data := make([]byte, size)
	var chunk [8]byte
	for index := 0; index < len(data); index += len(chunk) {
		binary.LittleEndian.PutUint64(chunk[:], random.Uint64())
		copy(data[index:], chunk[:])
	}
	return data
}

func randomUTF8(random *rand.Rand, size int) []byte {
	alphabet := []rune{'a', 'Z', '0', 'é', '€', '界', '🙂', '\n'}
	var data bytes.Buffer
	data.Grow(size)
	for data.Len() < size {
		_, _ = data.WriteRune(alphabet[random.IntN(len(alphabet))])
	}
	return data.Bytes()
}

func expectedProjection(input string, keep tool.Keep) string {
	if keep == tool.KeepTail {
		used := 0
		start := len(input)
		for start > 0 {
			_, size := utf8.DecodeLastRuneInString(input[:start])
			if used+size > testProjectionLimit {
				break
			}
			used += size
			start -= size
		}
		return input[start:]
	}
	used := 0
	end := 0
	for index, r := range input {
		size := utf8.RuneLen(r)
		if used+size > testProjectionLimit {
			break
		}
		used += size
		end = index + size
	}
	return input[:end]
}
