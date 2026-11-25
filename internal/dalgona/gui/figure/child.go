package figure

import (
	"bufio"
	"context"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"math"
	"os"
	"runtime/debug"
	"runtime/metrics"
	"time"

	"github.com/alecthomas/chroma/v2"
	"github.com/alecthomas/chroma/v2/lexers"
)

const ChildArgument = "--dalgona-figure-renderer"

const (
	launchTimeout    = 5 * time.Second
	childExitInvalid = 2
	childExitMemory  = 3
)

func IsChild(args []string) bool { return len(args) == 2 && args[1] == ChildArgument }

type child struct {
	in      *bufio.Reader
	out     io.Writer
	engines *engines
}

func ServeChild(fonts Fonts) int {
	if err := inheritedPipes(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		return childExitInvalid
	}
	c := &child{in: bufio.NewReader(os.Stdin), out: os.Stdout}
	nonce, err := c.launch()
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		return childExitInvalid
	}
	debug.SetMemoryLimit(ChildMemoryLimit)
	go watchMemory()
	if err := verifyEngines(); err != nil {
		_ = writeFrame(c.out, failureEnvelope(0, err), nil, ReplyFrameLimit)
		return 1
	}
	c.engines = newEngines(fonts)
	defer c.engines.close()
	hello := &envelope{Type: messageHello, Version: ProtocolVersion, Engines: engineManifest(), Fonts: fonts.manifest(), Nonce: nonce}
	if err := writeFrame(c.out, hello, nil, ReplyFrameLimit); err != nil {
		return 1
	}
	return c.serve()
}

func inheritedPipes() error {
	for _, f := range [...]*os.File{os.Stdin, os.Stdout} {
		info, err := f.Stat()
		if err != nil || info.Mode()&os.ModeNamedPipe == 0 {
			return fmt.Errorf("figure renderer must be started by dalgona with worker pipes; %s is not a pipe", f.Name())
		}
	}
	return nil
}

func (c *child) launch() (string, error) {
	type launched struct {
		env envelope
		err error
	}
	got := make(chan launched, 1)
	go func() {
		env, _, err := readFrame(c.in, frameHeader+EnvelopeLimit)
		got <- launched{env, err}
	}()
	select {
	case l := <-got:
		if l.err != nil || l.env.Type != messageLaunch || l.env.Version != ProtocolVersion || !validNonce(l.env.Nonce) {
			return "", fmt.Errorf("figure renderer received no valid launch frame")
		}
		return l.env.Nonce, nil
	case <-time.After(launchTimeout):
		return "", fmt.Errorf("figure renderer received no launch frame within %s", launchTimeout)
	}
}

func validNonce(nonce string) bool {
	raw, err := hex.DecodeString(nonce)
	return err == nil && len(raw) == 32
}

func watchMemory() {
	sample := []metrics.Sample{{Name: "/memory/classes/total:bytes"}}
	for range time.Tick(MemorySamplePeriod) {
		metrics.Read(sample)
		if sample[0].Value.Kind() == metrics.KindUint64 && sample[0].Value.Uint64() > ChildMemoryLimit {
			fmt.Fprintln(os.Stderr, "figure renderer exceeded its memory threshold")
			os.Exit(childExitMemory)
		}
	}
}

func (c *child) serve() int {
	for {
		env, payload, err := readFrame(c.in, requestFrameLimit)
		if errors.Is(err, io.EOF) {
			return 0
		}
		if err != nil {
			fmt.Fprintln(os.Stderr, err)
			return childExitInvalid
		}
		if err := c.job(env, payload); err != nil {
			fmt.Fprintln(os.Stderr, err)
			return 1
		}
	}
}

func (c *child) job(env envelope, payload []byte) error {
	deadline := time.Duration(env.DeadlineMs) * time.Millisecond
	if deadline <= 0 || deadline > warmupDeadline {
		deadline = RenderDeadline
	}
	ctx, cancel := context.WithTimeout(context.Background(), deadline)
	defer cancel()
	var result *jobResult
	var err error
	switch env.Type {
	case messageRender:
		result, err = c.render(ctx, env, payload)
	case messageHighlight:
		result, err = highlight(ctx, env, payload)
	case messageImage:
		result, err = imageJob(env, payload)
	case messageClipboardImage:
		return c.clipboardImage(ctx, env, payload)
	default:
		err = fail(FailureProtocol, "worker request type %q is not registered", env.Type)
	}
	if err != nil {
		return writeFrame(c.out, failureEnvelope(env.Job, err), nil, ReplyFrameLimit)
	}
	return result.send(c.out, env)
}

type jobResult struct {
	kind   string
	bounds []float64
	labels int
	ops    int
	chunks []resultChunk
}

type resultChunk struct {
	tile    []int
	payload []byte
}

func (r *jobResult) send(w io.Writer, req envelope) error {
	total := 0
	for _, ch := range r.chunks {
		total += len(ch.payload)
	}
	if total > resultByteLimit(r.kind) {
		return writeFrame(w, failureEnvelope(req.Job, fail(FailureLimit, "figure result has %d bytes; the limit is %d", total, resultByteLimit(r.kind))), nil, ReplyFrameLimit)
	}
	head := &envelope{Type: messageResult, Job: req.Job, Key: req.Key, Kind: r.kind, Budget: total, Bounds: r.bounds, Labels: r.labels, Operations: r.ops, Chunks: len(r.chunks)}
	if err := writeFrame(w, head, nil, ReplyFrameLimit); err != nil {
		return err
	}
	digest := sha256.New()
	for i, ch := range r.chunks {
		digest.Write(ch.payload)
		if err := writeFrame(w, &envelope{Type: messageChunk, Job: req.Job, Index: i, Format: r.kind, Tile: ch.tile}, ch.payload, ReplyFrameLimit); err != nil {
			return err
		}
	}
	return writeFrame(w, &envelope{Type: messageComplete, Job: req.Job, Chunks: len(r.chunks), Bytes: total, Digest: hex.EncodeToString(digest.Sum(nil))}, nil, ReplyFrameLimit)
}

func splitPayload(data []byte) []resultChunk {
	var out []resultChunk
	for len(data) > 0 {
		n := min(len(data), chunkPayloadLimit)
		out = append(out, resultChunk{payload: data[:n]})
		data = data[n:]
	}
	return out
}

func (c *child) render(ctx context.Context, env envelope, source []byte) (*jobResult, error) {
	kind, ok := ParseKind(env.Kind)
	theme, themeOK := parseTheme(env.Theme)
	if !ok || !themeOK || !finiteScale(env.Scale) || env.Payload != len(source) {
		return nil, fail(FailureProtocol, "render request has an unregistered kind, theme, scale, or length")
	}
	list, err := c.engines.render(ctx, figureRequest{kind: kind, source: string(source), theme: theme, scale: env.Scale})
	if err != nil {
		return nil, err
	}
	data := EncodeList(list)
	return &jobResult{
		kind:   resultDisplayList,
		bounds: []float64{float64(list.Width), float64(list.Height)},
		labels: len(list.Labels),
		ops:    list.Operations(),
		chunks: splitPayload(data),
	}, nil
}

const tokenRecord = 12

func highlight(ctx context.Context, env envelope, source []byte) (*jobResult, error) {
	size := len(source)
	if size > HighlightSourceLimit {
		return nil, fail(FailureLimit, "code has %d bytes; the highlighting limit is %d", size, HighlightSourceLimit)
	}
	lexer := lexers.Get(env.Language)
	if lexer == nil {
		return nil, fail(FailureInvalid, "no highlighter is registered for %q", env.Language)
	}
	it, err := lexer.Tokenise(&chroma.TokeniseOptions{State: "root"}, string(source))
	if err != nil {
		return nil, fail(FailureEngine, "highlighter failed: %v", err)
	}
	var out []byte
	offset := 0
	for tok := it(); tok != chroma.EOF; tok = it() {
		if ctx.Err() != nil {
			return nil, fail(FailureTimeout, "highlighting stopped at its deadline")
		}
		end := min(offset+len(tok.Value), size)
		if end > offset {
			out = binary.LittleEndian.AppendUint32(out, uint32(offset))
			out = binary.LittleEndian.AppendUint32(out, uint32(end))
			out = binary.LittleEndian.AppendUint32(out, uint32(int64(tok.Type)&math.MaxUint32))
		}
		offset = end
	}
	if offset != size {
		return nil, fail(FailureEngine, "highlighter covered %d of %d source bytes", offset, size)
	}
	return &jobResult{kind: resultTokens, bounds: []float64{float64(size)}, chunks: splitPayload(out)}, nil
}
