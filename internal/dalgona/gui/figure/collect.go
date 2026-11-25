package figure

import (
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"hash"
	"math"
)

type collector struct {
	job       *job
	head      *envelope
	chunks    [][]byte
	tiles     [][]int
	clipboard *clipboardCollection
	count     int
	total     int
	digest    hash.Hash
	result    Outcome
}

const previewSideLimit = 4096

func newCollector(j *job) *collector {
	return &collector{job: j, digest: sha256.New(), result: Outcome{Job: j.id, Key: j.key}}
}

func (c *collector) failure(err error) Outcome {
	c.result.Err = FailureOf(err)
	return c.result
}

func (c *collector) outcome() Outcome { return c.result }

func (c *collector) reject(format string, args ...any) (bool, bool) {
	c.result = Outcome{Job: c.job.id, Key: c.job.key, Err: fail(FailureProtocol, format, args...)}
	return true, false
}

var expectedResult = map[string]string{
	messageRender:         resultDisplayList,
	messageImage:          resultImage,
	messageClipboardImage: resultClipboardImage,
	messageHighlight:      resultTokens,
}

func (c *collector) accept(f frame) (bool, bool) {
	if f.err != nil {
		c.result.Err = FailureOf(f.err)
		return true, false
	}
	if f.env.Job != c.job.id {
		return c.reject("figure renderer answered job %d while job %d runs", f.env.Job, c.job.id)
	}
	switch f.env.Type {
	case messageError:
		c.result.Err = failureFromEnvelope(f.env)
		return true, c.result.Err.Code != FailureTimeout && c.result.Err.Code != FailureProtocol
	case messageResult:
		return c.begin(f.env)
	case messageChunk:
		return c.chunk(f.env, f.payload)
	case messageComplete:
		return c.complete(f.env)
	}
	return c.reject("figure renderer sent unexpected %q frame", f.env.Type)
}

func (c *collector) begin(env envelope) (bool, bool) {
	if problem := c.headerProblem(env); problem != "" {
		return c.reject("%s", problem)
	}
	c.head = &env
	if env.Kind != resultClipboardImage {
		return false, true
	}
	clipboard, err := c.stageClipboard(env)
	if err != nil {
		return c.abort(err)
	}
	c.clipboard = clipboard
	return false, true
}

func (c *collector) headerProblem(env envelope) string {
	switch {
	case c.head != nil:
		return "figure renderer sent a second result header"
	case env.Key != c.job.key:
		return "figure result content key does not match the request"
	case env.Kind != expectedResult[c.job.request.Type]:
		return fmt.Sprintf("figure result kind %q does not match the request", env.Kind)
	case env.Budget < 0 || env.Budget > resultByteLimit(env.Kind) || env.Chunks < 0 || env.Chunks > env.Budget:
		return fmt.Sprintf("figure result declares %d bytes in %d chunks; the limit is %d bytes", env.Budget, env.Chunks, resultByteLimit(env.Kind))
	}
	return ""
}

func (c *collector) chunk(env envelope, payload []byte) (bool, bool) {
	switch {
	case c.head == nil:
		return c.reject("figure renderer sent a chunk before its result header")
	case env.Index != c.count:
		return c.reject("figure chunk %d arrived where chunk %d belongs", env.Index, c.count)
	case c.count >= c.head.Chunks:
		return c.reject("figure renderer sent more chunks than its header declares")
	case len(payload) == 0 || c.total+len(payload) > c.head.Budget:
		return c.reject("figure chunks exceed the declared %d byte budget", c.head.Budget)
	}
	if err := c.appendChunk(env, payload); err != nil {
		return c.abort(err)
	}
	c.count++
	c.total += len(payload)
	c.digest.Write(payload)
	return false, true
}

func (c *collector) abort(err error) (bool, bool) {
	c.result.Err = FailureOf(err)
	return true, false
}

func (c *collector) appendChunk(env envelope, payload []byte) error {
	if c.clipboard != nil {
		return c.clipboard.write(env.Format, payload)
	}
	if env.Format != c.head.Kind {
		return fail(FailureProtocol, "figure chunk format %q does not match result kind %q", env.Format, c.head.Kind)
	}
	c.chunks = append(c.chunks, payload)
	c.tiles = append(c.tiles, env.Tile)
	return nil
}

func (c *collector) complete(env envelope) (bool, bool) {
	if problem := c.completionProblem(env); problem != "" {
		return c.reject("%s", problem)
	}
	if err := c.assemble(); err != nil {
		c.result = Outcome{Job: c.job.id, Key: c.job.key, Err: FailureOf(err)}
		return true, false
	}
	return true, true
}

func (c *collector) completionProblem(env envelope) string {
	switch {
	case c.head == nil:
		return "figure renderer completed before its result header"
	case env.Chunks != c.count || env.Chunks != c.head.Chunks:
		return fmt.Sprintf("figure completion counts %d chunks but %d arrived", env.Chunks, c.count)
	case env.Bytes != c.total || env.Bytes != c.head.Budget:
		return fmt.Sprintf("figure completion counts %d bytes but %d arrived", env.Bytes, c.total)
	case env.Digest != hex.EncodeToString(c.digest.Sum(nil)):
		return "figure result digest does not match its chunks"
	}
	return ""
}

func (c *collector) assemble() error {
	var err error
	switch c.head.Kind {
	case resultDisplayList:
		c.result.List, err = c.displayList()
	case resultImage:
		c.result.Image, err = c.preview()
	case resultTokens:
		c.result.Tokens, err = c.tokens()
	}
	return err
}

func (c *collector) joined() []byte {
	out := make([]byte, 0, c.total)
	for _, ch := range c.chunks {
		out = append(out, ch...)
	}
	return out
}

func (c *collector) displayList() (*List, error) {
	list, err := DecodeList(c.joined())
	if err != nil {
		return nil, err
	}
	b := c.head.Bounds
	if len(b) != 2 || float32(b[0]) != list.Width || float32(b[1]) != list.Height {
		return nil, fail(FailureProtocol, "figure bounds in the result header do not match its display list")
	}
	if c.head.Labels != len(list.Labels) || c.head.Operations != list.Operations() {
		return nil, fail(FailureProtocol, "figure label or operation counts do not match its display list")
	}
	return list, nil
}

func (c *collector) preview() (*Preview, error) {
	b := c.head.Bounds
	if len(b) != 4 || !wholeSide(b[0], previewSideLimit) || !wholeSide(b[1], previewSideLimit) || !wholeSide(b[2], ImageSideLimit) || !wholeSide(b[3], ImageSideLimit) {
		return nil, fail(FailureProtocol, "image preview bounds are malformed")
	}
	p := &Preview{Width: int(b[0]), Height: int(b[1]), SourceWidth: int(b[2]), SourceHeight: int(b[3])}
	columns := (p.Width + TileSideLimit - 1) / TileSideLimit
	rows := (p.Height + TileSideLimit - 1) / TileSideLimit
	if len(c.chunks) != columns*rows {
		return nil, fail(FailureProtocol, "image preview has %d tiles; its bounds need %d", len(c.chunks), columns*rows)
	}
	seen := make(map[[2]int]bool, len(c.chunks))
	for i, tile := range c.tiles {
		t, err := checkTile(tile, c.chunks[i], p, seen)
		if err != nil {
			return nil, err
		}
		p.Tiles = append(p.Tiles, t)
	}
	return p, nil
}

func wholeSide(v float64, limit int) bool {
	return v >= 1 && v <= float64(limit) && v == math.Trunc(v)
}

func checkTile(tile []int, pix []byte, p *Preview, seen map[[2]int]bool) (Tile, error) {
	if len(tile) != 4 {
		return Tile{}, fail(FailureProtocol, "image tile has no bounds")
	}
	x, y, w, h := tile[0], tile[1], tile[2], tile[3]
	if !tileInGrid(p, x, y) || !tileFillsCell(p, tile) || seen[[2]int{x, y}] || len(pix) != w*h*4 {
		return Tile{}, fail(FailureProtocol, "image tile %v is misplaced, overlapping, or sized wrongly", tile)
	}
	seen[[2]int{x, y}] = true
	bounds := Rect{Min: Point{X: float32(x), Y: float32(y)}, Max: Point{X: float32(x + w), Y: float32(y + h)}}
	return Tile{Bounds: bounds, Width: w, Height: h, Pix: pix}, nil
}

func tileInGrid(p *Preview, x, y int) bool {
	return x >= 0 && y >= 0 && x < p.Width && y < p.Height && x%TileSideLimit == 0 && y%TileSideLimit == 0
}

func tileFillsCell(p *Preview, tile []int) bool {
	return tile[2] == min(TileSideLimit, p.Width-tile[0]) && tile[3] == min(TileSideLimit, p.Height-tile[1])
}

func (c *collector) tokens() ([]Token, error) {
	data := c.joined()
	if !c.tokenBounds(len(data)) {
		return nil, fail(FailureProtocol, "highlight result is malformed")
	}
	out := make([]Token, 0, len(data)/tokenRecord)
	end := 0
	for i := 0; i < len(data); i += tokenRecord {
		t := Token{
			Start: int(binary.LittleEndian.Uint32(data[i:])),
			End:   int(binary.LittleEndian.Uint32(data[i+4:])),
			Type:  binary.LittleEndian.Uint32(data[i+8:]),
		}
		if t.Start != end || t.End <= t.Start || t.End > len(c.job.payload) {
			return nil, fail(FailureProtocol, "highlight token %d covers bytes %d to %d out of order", len(out), t.Start, t.End)
		}
		end = t.End
		out = append(out, t)
	}
	if end != len(c.job.payload) {
		return nil, fail(FailureProtocol, "highlight tokens cover %d of %d source bytes", end, len(c.job.payload))
	}
	return out, nil
}

func (c *collector) tokenBounds(bytes int) bool {
	b := c.head.Bounds
	return bytes%tokenRecord == 0 && len(b) == 1 && b[0] >= 0 && b[0] <= HighlightSourceLimit && b[0] == float64(len(c.job.payload))
}
