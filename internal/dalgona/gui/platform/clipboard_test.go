package platform

import (
	"bytes"
	"errors"
	"image"
	"image/color"
	"image/png"
	"io"
	"strings"
	"testing"
	"testing/iotest"

	"github.com/gosuda/dalgona/internal/dal/textinput"
)

const exactSample = "line\r\nlone\rcr\n\u2028sep e\u0301 한글 \U0001F469\u200D\U0001F469\u200D\U0001F467 \uFEFF\U000E0001 \tend\n"

func TestReadTextKeepsTransferredScalarsExactly(t *testing.T) {
	got, err := ReadText(strings.NewReader(exactSample))
	if err != nil {
		t.Fatal(err)
	}
	if got != exactSample {
		t.Fatalf("ReadText changed the transfer:\n got %q\nwant %q", got, exactSample)
	}
}

func TestReadTextRejectsTheWholeCandidate(t *testing.T) {
	cases := []struct {
		name string
		data string
		want error
	}{
		{"malformed byte", "valid prefix \xff suffix", textinput.InvalidEncoding},
		{"encoded surrogate", "a\xed\xa0\x80b", textinput.InvalidEncoding},
		{"truncated scalar", "한\xea\xb8", textinput.InvalidEncoding},
		{"embedded NUL", "before\x00after", textinput.DisallowedControl},
		{"trailing NUL", "text\x00", textinput.DisallowedControl},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got, err := ReadText(strings.NewReader(tc.data))
			if !errors.Is(err, tc.want) {
				t.Fatalf("error %v, want %v", err, tc.want)
			}
			if got != "" {
				t.Fatalf("a rejected transfer returned %q", got)
			}
		})
	}
}

type countingReader struct{ read int64 }

func (r *countingReader) Read(p []byte) (int, error) {
	for i := range p {
		p[i] = 'a'
	}
	r.read += int64(len(p))
	return len(p), nil
}

func TestReadTextBoundsTheTransferBeforeDecoding(t *testing.T) {
	const limit = 32 * 1024 * 1024
	exact, err := ReadText(strings.NewReader(strings.Repeat("a", limit)))
	if err != nil || len(exact) != limit {
		t.Fatalf("a transfer of exactly 32 MiB: length %d, error %v", len(exact), err)
	}
	if _, err := ReadText(strings.NewReader(strings.Repeat("a", limit+1))); !errors.Is(err, ErrTransferTooLarge) {
		t.Fatalf("a transfer one byte over 32 MiB: error %v, want ErrTransferTooLarge", err)
	}
	endless := new(countingReader)
	if _, err := ReadText(endless); !errors.Is(err, ErrTransferTooLarge) {
		t.Fatalf("an endless transfer: error %v, want ErrTransferTooLarge", err)
	}
	if endless.read > limit+64*1024 {
		t.Fatalf("read %d bytes from an endless transfer, want the read bounded near 32 MiB", endless.read)
	}
}

func TestReadTextReportsTransferFailure(t *testing.T) {
	broken := io.MultiReader(strings.NewReader("partial"), iotest.ErrReader(errTransferBroken))
	if _, err := ReadText(broken); !errors.Is(err, errTransferBroken) {
		t.Fatalf("error %v, want the stream failure", err)
	}
}

var errTransferBroken = errors.New("transfer broken")

func TestCheckCopyAcceptsOnlyExactlyTransferableText(t *testing.T) {
	if err := CheckCopy(exactSample); err != nil {
		t.Fatalf("CheckCopy refused representable text: %v", err)
	}
	cases := []struct {
		name string
		text string
		want error
	}{
		{"embedded NUL", "a\x00b", textinput.DisallowedControl},
		{"malformed byte", "a\xffb", textinput.InvalidEncoding},
		{"over the transfer limit", strings.Repeat("a", 32*1024*1024+1), ErrTransferTooLarge},
	}
	for _, tc := range cases {
		if err := CheckCopy(tc.text); !errors.Is(err, tc.want) {
			t.Errorf("%s: error %v, want %v", tc.name, err, tc.want)
		}
	}
}

func TestPastedLineEndingsFollowTheFieldPolicy(t *testing.T) {
	pasted, err := ReadText(strings.NewReader("one\r\ntwo\rthree\n\u2028"))
	if err != nil {
		t.Fatal(err)
	}
	multi, err := NewComposer(textinput.Policy{Lines: textinput.MultiLine, Limit: textinput.Limit{Unit: textinput.Scalars, Max: 100}}, textinput.Draft{})
	if err != nil {
		t.Fatal(err)
	}
	if err = multi.Replace(pasted); err != nil {
		t.Fatal(err)
	}
	if got, want := multi.Draft().Text, "one\ntwo\nthree\n\u2028"; got != want {
		t.Fatalf("multiline paste stored %q, want %q", got, want)
	}
	previous := textinput.Draft{Text: "keep", Selection: textinput.Range{Start: 1, End: 3}}
	single, err := NewComposer(textinput.Policy{Lines: textinput.SingleLine, Limit: textinput.Limit{Unit: textinput.Scalars, Max: 100}}, previous)
	if err != nil {
		t.Fatal(err)
	}
	if err := single.Replace(pasted); !errors.Is(err, textinput.LineBreakNotAllowed) {
		t.Fatalf("single-line paste error %v, want LineBreakNotAllowed", err)
	}
	if single.Draft() != previous {
		t.Fatalf("a rejected single-line paste changed the draft to %+v", single.Draft())
	}
	if single.TakeRejected() != pasted {
		t.Fatal("the rejected paste is not recoverable")
	}
}

func TestPasteTargetRefusesAChangedBinding(t *testing.T) {
	bound := PasteTarget{Window: 3, Endpoint: "local", Thread: "t1", Field: "composer", Revision: 7, Selection: textinput.Range{Start: 2, End: 4}}
	if err := bound.Check(bound); err != nil {
		t.Fatalf("an unchanged target refused the paste: %v", err)
	}
	changes := map[string]func(*PasteTarget){
		"window generation": func(p *PasteTarget) { p.Window++ },
		"endpoint":          func(p *PasteTarget) { p.Endpoint = "box" },
		"thread":            func(p *PasteTarget) { p.Thread = "t2" },
		"field":             func(p *PasteTarget) { p.Field = "search" },
		"draft revision":    func(p *PasteTarget) { p.Revision++ },
		"selection":         func(p *PasteTarget) { p.Selection.End++ },
		"removed target":    func(p *PasteTarget) { *p = PasteTarget{} },
	}
	for name, change := range changes {
		current := bound
		change(&current)
		if err := bound.Check(current); !errors.Is(err, ErrPasteStale) {
			t.Errorf("%s changed: error %v, want ErrPasteStale", name, err)
		}
	}
}

func testPNG(t *testing.T, width, height int) []byte {
	t.Helper()
	img := image.NewNRGBA(image.Rect(0, 0, width, height))
	var shade byte
	for y := 0; y < height; y++ {
		for x := 0; x < width; x++ {
			img.SetNRGBA(x, y, color.NRGBA{R: shade, G: 0x80, B: shade ^ 0x55, A: 0xFF})
			shade++
		}
	}
	var encoded bytes.Buffer
	if err := png.Encode(&encoded, img); err != nil {
		t.Fatal(err)
	}
	return encoded.Bytes()
}
