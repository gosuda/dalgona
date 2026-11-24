package figure

import (
	"bytes"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"io"
	"math"
	"reflect"
	"slices"
	"strings"
	"testing"
)

func codeOf(err error) FailureCode {
	if f := FailureOf(err); f != nil {
		return f.Code
	}
	return 0
}

func encodeFrame(env *envelope, payload []byte, limit int) ([]byte, error) {
	var transmitted bytes.Buffer
	err := writeFrame(&transmitted, env, payload, limit)
	return transmitted.Bytes(), err
}

func TestFrameRoundTrip(t *testing.T) {
	env := &envelope{Type: messageRender, Job: 7, Key: "k", Kind: "d2", Theme: themeRoles(testTheme), Scale: 2, Viewport: []float64{3, 4}}
	frame, err := encodeFrame(env, []byte("a -> b"), requestFrameLimit)
	if err != nil {
		t.Fatal(err)
	}
	got, payload, err := readFrame(bytes.NewReader(frame), requestFrameLimit)
	if err != nil {
		t.Fatal(err)
	}
	if string(payload) != "a -> b" || got.Job != 7 || got.Payload != 6 || !reflect.DeepEqual(got.Theme, env.Theme) {
		t.Fatalf("round trip = %+v %q", got, payload)
	}
	if int64(binary.LittleEndian.Uint32(frame)) != int64(len(frame))-4 {
		t.Fatalf("frame length prefix = %d, want %d", binary.LittleEndian.Uint32(frame), len(frame)-4)
	}
}

type countingReader struct {
	r    io.Reader
	read int
}

func (c *countingReader) Read(p []byte) (int, error) {
	n, err := c.r.Read(p)
	c.read += n
	return n, err
}

func TestReadFrameRefusesBeforeAllocation(t *testing.T) {
	header := binary.LittleEndian.AppendUint32(nil, 1<<30)
	header = binary.LittleEndian.AppendUint16(header, 32)
	src := &countingReader{r: io.MultiReader(bytes.NewReader(header), strings.NewReader(strings.Repeat("x", 1<<16)))}
	_, _, err := readFrame(src, ReplyFrameLimit)
	if codeOf(err) != FailureProtocol || src.read != frameHeader {
		t.Fatalf("oversized frame = %v after reading %d bytes, want a refusal after the %d byte header", err, src.read, frameHeader)
	}
	cases := map[string][]byte{}
	good, _ := encodeFrame(&envelope{Type: messageChunk, Job: 1}, []byte("abc"), ReplyFrameLimit)
	mismatch := append([]byte(nil), good...)
	binary.LittleEndian.PutUint32(mismatch, binary.LittleEndian.Uint32(mismatch)-1)
	cases["payload mismatch"] = mismatch[:len(mismatch)-1]
	huge := binary.LittleEndian.AppendUint32(nil, 100)
	cases["envelope too large"] = binary.LittleEndian.AppendUint16(huge, EnvelopeLimit+1)
	unknown, _ := encodeFrame(&envelope{Type: messageChunk}, nil, ReplyFrameLimit)
	unknown = bytes.Replace(unknown, []byte(`"type"`), []byte(`"tipe"`), 1)
	cases["unknown member"] = unknown
	cases["truncated payload"] = good[:len(good)-1]
	for name, data := range cases {
		if _, _, err := readFrame(bytes.NewReader(data), ReplyFrameLimit); codeOf(err) != FailureProtocol {
			t.Errorf("%s: error = %v, want a protocol failure", name, err)
		}
	}
	if _, err := encodeFrame(&envelope{Type: messageError, Message: strings.Repeat("m", EnvelopeLimit)}, nil, ReplyFrameLimit); err == nil {
		t.Error("an envelope over 4096 bytes was encoded")
	}
	if _, err := encodeFrame(&envelope{Type: messageChunk}, make([]byte, ReplyFrameLimit), ReplyFrameLimit); err == nil {
		t.Error("a reply frame over one MiB was encoded")
	}
}

func sampleList(t *testing.T) *List {
	t.Helper()
	return decodeRaw(t, svgDoc(`<linearGradient id="g"><stop offset="0" stop-color="red"/><stop offset="1" stop-color="blue"/></linearGradient>
<g opacity="0.5" transform="translate(1 2)"><rect width="10" height="10" fill="url(#g)"/><path d="M0 0 H8 V8 H0 Z M2 2 H6 V6 H2 Z" fill-rule="evenodd"/></g>
<clipPath id="c"><rect width="4" height="4"/></clipPath><text clip-path="url(#c)" x="1" y="2">label 한글</text><title>Figure</title>`))
}

func TestDisplayListCodecRoundTrip(t *testing.T) {
	l := sampleList(t)
	if len(l.Tiles) == 0 || len(l.Gradients) == 0 || len(l.Chunks) == 0 || l.Title != "Figure" {
		t.Fatalf("sample list lacks tiles, gradients, text or title: %+v", l)
	}
	got, err := DecodeList(EncodeList(l))
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(got, l) {
		t.Fatalf("round trip changed the display list\n got %+v\nwant %+v", got, l)
	}
}

func TestDisplayListDecoderRefusesDamage(t *testing.T) {
	data := EncodeList(sampleList(t))
	for n := range len(data) {
		if _, err := DecodeList(data[:n]); err == nil {
			t.Fatalf("truncated list of %d of %d bytes decoded", n, len(data))
		}
	}
	if _, err := DecodeList(append(append([]byte(nil), data...), 0)); err == nil {
		t.Fatal("list with a trailing byte decoded")
	}
	mutations := map[string]func(*List){
		"NaN point":  func(l *List) { l.Paths[0].Points[0].X = float32(math.NaN()) },
		"huge point": func(l *List) { l.Paths[0].Points[0].Y = 2 * CoordinateLimit },
		"path index": func(l *List) {
			i := slices.IndexFunc(l.Ops, func(o Op) bool { return o.Kind == OpFill })
			l.Ops[i].Index = entryIndex(t, len(l.Paths))
		},
		"unbalanced pop":   func(l *List) { l.Ops = append(l.Ops, Op{Kind: OpPop}) },
		"open push":        func(l *List) { l.Ops = append(l.Ops, Op{Kind: OpOpacity, Alpha: 1}) },
		"opacity range":    func(l *List) { l.Ops = append(l.Ops, Op{Kind: OpOpacity, Alpha: 2}, Op{Kind: OpPop}) },
		"gradient index":   func(l *List) { l.Ops = append(l.Ops, Op{Kind: OpFill, Index: 0, Paint: Paint{Gradient: 9}}) },
		"tile size":        func(l *List) { l.Tiles[0].Width = TileSideLimit + 1 },
		"tile pixels":      func(l *List) { l.Tiles[0].Pix = l.Tiles[0].Pix[:4] },
		"text weight":      func(l *List) { l.Chunks[0].Runs[0].Weight = 0 },
		"label index":      func(l *List) { l.Chunks[0].Label = entryIndex(t, len(l.Labels)) },
		"invalid utf8":     func(l *List) { l.Labels[0] = "\xff" },
		"stop order":       func(l *List) { l.Gradients[0].Stops[0].Offset = 2 },
		"verb point count": func(l *List) { l.Paths[0].Points = l.Paths[0].Points[1:] },
	}
	for name, mutate := range mutations {
		l := sampleList(t)
		mutate(l)
		if _, err := DecodeList(EncodeList(l)); codeOf(err) == 0 {
			t.Errorf("%s: damaged list decoded", name)
		}
	}
}

func entryIndex(t *testing.T, i int) int32 {
	t.Helper()
	index, err := listIndex(i)
	if err != nil {
		t.Fatal(err)
	}
	return index
}

func FuzzDecodeList(f *testing.F) {
	f.Add([]byte(displayMagic))
	f.Add(EncodeList(&List{Width: 1, Height: 1, Ops: []Op{{Kind: OpOpacity, Alpha: 0.5}, {Kind: OpPop}}}))
	f.Fuzz(func(t *testing.T, data []byte) {
		l, err := DecodeList(data)
		if err != nil {
			return
		}
		if err := l.Validate(); err != nil {
			t.Fatalf("decoded list fails validation: %v", err)
		}
		if !bytes.Equal(EncodeList(l), data) {
			t.Fatal("accepted list does not re-encode to the same bytes")
		}
	})
}

func testJob(kind string) *job {
	return &job{id: 3, key: "key", request: envelope{Type: kind}}
}

func feed(c *collector, frames ...frame) (bool, bool) {
	var done, healthy bool
	for _, f := range frames {
		if done, healthy = c.accept(f); done {
			return done, healthy
		}
	}
	return done, healthy
}

func chunkDigest(chunks ...[]byte) string {
	h := sha256.New()
	for _, c := range chunks {
		h.Write(c)
	}
	return hex.EncodeToString(h.Sum(nil))
}

func TestCollectorCommitsOnlyVerifiedResults(t *testing.T) {
	l := sampleList(t)
	data := EncodeList(l)
	half := len(data) / 2
	head := envelope{Type: messageResult, Job: 3, Key: "key", Kind: resultDisplayList, Budget: len(data), Chunks: 2, Bounds: []float64{float64(l.Width), float64(l.Height)}, Labels: len(l.Labels), Operations: l.Operations()}
	with := func(change func(*envelope)) frame { h := head; change(&h); return frame{env: h} }
	h := frame{env: head}
	c0 := frame{env: envelope{Type: messageChunk, Job: 3, Index: 0, Format: resultDisplayList}, payload: data[:half]}
	c1 := frame{env: envelope{Type: messageChunk, Job: 3, Index: 1, Format: resultDisplayList}, payload: data[half:]}
	done := frame{env: envelope{Type: messageComplete, Job: 3, Chunks: 2, Bytes: len(data), Digest: chunkDigest(data)}}
	c := newCollector(testJob(messageRender))
	if finished, _ := feed(c, h, c0, c1); finished || c.outcome().List != nil {
		t.Fatal("partial result became visible before completion")
	}
	if finished, healthy := c.accept(done); !finished || !healthy || c.outcome().List == nil {
		t.Fatalf("valid result was not committed: %+v", c.outcome().Err)
	}
	badDigest := done
	badDigest.env.Digest = chunkDigest(data[:1])
	bad := map[string][]frame{
		"duplicate chunk":   {h, c0, c0},
		"reordered chunks":  {h, c1},
		"missing chunk":     {h, c0, done},
		"wrong key":         {with(func(e *envelope) { e.Key = "other" })},
		"wrong job":         {with(func(e *envelope) { e.Job = 4 })},
		"wrong kind":        {with(func(e *envelope) { e.Kind = resultImage })},
		"over budget":       {with(func(e *envelope) { e.Budget = half }), c0, c1},
		"digest":            {h, c0, c1, badDigest},
		"bounds":            {with(func(e *envelope) { e.Bounds = []float64{1, 1} }), c0, c1, done},
		"operation count":   {with(func(e *envelope) { e.Operations++ }), c0, c1, done},
		"chunk before head": {c0},
	}
	for name, frames := range bad {
		requireRejected(t, name, frames)
	}
}

func requireRejected(t *testing.T, name string, frames []frame) {
	t.Helper()
	c := newCollector(testJob(messageRender))
	finished, healthy := feed(c, frames...)
	if !finished || healthy || c.outcome().Err == nil || c.outcome().List != nil {
		t.Errorf("%s: finished=%v healthy=%v err=%v, want a rejected result that ends the renderer", name, finished, healthy, c.outcome().Err)
	}
}

func TestCollectorValidatesImageTiles(t *testing.T) {
	wide, narrow := make([]byte, TileSideLimit*4*4), make([]byte, 4*4*4)
	head := frame{env: envelope{Type: messageResult, Job: 3, Key: "key", Kind: resultImage, Budget: len(wide) + len(narrow), Chunks: 2, Bounds: []float64{TileSideLimit + 4, 4, 520, 8}}}
	tile := func(i, x, w int, pix []byte) frame {
		return frame{env: envelope{Type: messageChunk, Job: 3, Index: i, Format: resultImage, Tile: []int{x, 0, w, 4}}, payload: pix}
	}
	done := frame{env: envelope{Type: messageComplete, Job: 3, Chunks: 2, Bytes: len(wide) + len(narrow), Digest: chunkDigest(wide, narrow)}}
	c := newCollector(testJob(messageImage))
	if finished, healthy := feed(c, head, tile(0, 0, TileSideLimit, wide), tile(1, TileSideLimit, 4, narrow), done); !finished || !healthy || len(c.outcome().Image.Tiles) != 2 {
		t.Fatalf("valid preview tiles were refused: %v", c.outcome().Err)
	}
	cases := map[string][]frame{
		"overlapping tile":   {head, tile(0, 0, TileSideLimit, wide), tile(1, 0, 4, narrow), done},
		"out of bounds tile": {head, tile(0, 0, TileSideLimit, wide), tile(1, TileSideLimit+4, 4, narrow), done},
		"misaligned tile":    {head, tile(0, 0, TileSideLimit, wide), tile(1, TileSideLimit-1, 4, narrow), done},
	}
	for name, frames := range cases {
		c = newCollector(testJob(messageImage))
		if finished, healthy := feed(c, frames...); !finished || healthy {
			t.Errorf("%s was accepted", name)
		}
	}
	big := head
	big.env.Bounds = []float64{TileSideLimit + 4, 4, ImageSideLimit + 1, 8}
	c = newCollector(testJob(messageImage))
	if finished, healthy := feed(c, big, tile(0, 0, TileSideLimit, wide), tile(1, TileSideLimit, 4, narrow), done); !finished || healthy {
		t.Fatal("image source dimensions over the limit were accepted")
	}
}

func TestFailureEnvelopeKeepsCodeAndSpan(t *testing.T) {
	for code := FailureInvalid; code <= FailureProtocol; code++ {
		in := &Failure{Code: code, Message: strings.Repeat("x", 3*messageLimit), Span: Span{Line: 2, Column: 3, EndLine: 2, EndColumn: 9}}
		out := failureFromEnvelope(*failureEnvelope(1, in))
		if out.Code != code || out.Span != in.Span || len(out.Message) != messageLimit {
			t.Fatalf("code %v round trip = %+v", code, out)
		}
	}
	if FailureOf(errors.New("plain")).Code != FailureEngine {
		t.Fatal("plain errors must map to engine failures")
	}
}
