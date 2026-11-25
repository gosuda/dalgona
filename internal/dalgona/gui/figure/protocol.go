package figure

import (
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	json "encoding/json/v2"
	"errors"
	"io"
	"math"

	"github.com/gosuda/dalgona/internal/dalgona/gui/clipboardimage"
)

const ProtocolVersion = 1

const (
	messageLaunch         = "launch"
	messageHello          = "hello"
	messageRender         = "render"
	messageImage          = "image"
	messageClipboardImage = "clipboard-image"
	messageHighlight      = "highlight"
	messageResult         = "result"
	messageChunk          = "chunk"
	messageComplete       = "complete"
	messageError          = "error"
)

const (
	resultDisplayList    = "display-list"
	resultImage          = "rgba-tiles"
	resultClipboardImage = "clipboard-image"
	resultTokens         = "tokens"
)

const (
	frameHeader       = 6
	requestFrameLimit = frameHeader + EnvelopeLimit + 5<<20
	chunkPayloadLimit = ReplyFrameLimit - frameHeader - EnvelopeLimit
	messageLimit      = 1024
)

type envelope struct {
	Type       string    `json:"type"`
	Job        uint64    `json:"job,omitzero"`
	Payload    int       `json:"payload,omitzero"`
	Version    int       `json:"version,omitzero"`
	Engines    string    `json:"engines,omitzero"`
	Fonts      string    `json:"fonts,omitzero"`
	Nonce      string    `json:"nonce,omitzero"`
	Document   string    `json:"document,omitzero"`
	Revision   uint64    `json:"revision,omitzero"`
	Key        string    `json:"key,omitzero"`
	Kind       string    `json:"kind,omitzero"`
	Theme      []string  `json:"theme,omitzero"`
	Scale      float64   `json:"scale,omitzero"`
	Viewport   []float64 `json:"viewport,omitzero"`
	DeadlineMs int64     `json:"deadlineMs,omitzero"`
	Format     string    `json:"format,omitzero"`
	Language   string    `json:"language,omitzero"`
	Budget     int       `json:"budget,omitzero"`
	PNGBytes   int       `json:"pngBytes,omitzero"`
	DIBBytes   int       `json:"dibBytes,omitzero"`
	Bounds     []float64 `json:"bounds,omitzero"`
	Baseline   float64   `json:"baseline,omitzero"`
	Labels     int       `json:"labels,omitzero"`
	Operations int       `json:"operations,omitzero"`
	Index      int       `json:"index,omitzero"`
	Tile       []int     `json:"tile,omitzero"`
	Chunks     int       `json:"chunks,omitzero"`
	Bytes      int       `json:"bytes,omitzero"`
	Digest     string    `json:"digest,omitzero"`
	Code       string    `json:"code,omitzero"`
	Message    string    `json:"message,omitzero"`
	Span       []int     `json:"span,omitzero"`
}

func encodeFrameHeader(env *envelope, payloadLen, limit int) ([]byte, error) {
	env.Payload = payloadLen
	head, err := json.Marshal(env)
	if err != nil {
		return nil, fail(FailureProtocol, "worker envelope did not encode: %v", err)
	}
	headLen := len(head)
	if headLen > EnvelopeLimit {
		return nil, fail(FailureProtocol, "worker envelope has %d bytes; the limit is %d", headLen, EnvelopeLimit)
	}
	rest := frameHeader - 4 + headLen + payloadLen
	if rest < 0 || int64(rest) > math.MaxUint32 || rest+4 > limit {
		return nil, fail(FailureProtocol, "worker frame has %d bytes; the limit is %d", rest+4, limit)
	}
	frame := make([]byte, 0, frameHeader+headLen)
	frame = binary.LittleEndian.AppendUint32(frame, uint32(rest))
	frame = binary.LittleEndian.AppendUint16(frame, uint16(headLen))
	return append(frame, head...), nil
}

func writeFrame(w io.Writer, env *envelope, payload []byte, limit int) error {
	head, err := encodeFrameHeader(env, len(payload), limit)
	if err != nil {
		return err
	}
	if err := writeFramePart(w, head); err != nil {
		return err
	}
	if len(payload) > 0 {
		return writeFramePart(w, payload)
	}
	return nil
}

func writeFramePart(w io.Writer, data []byte) error {
	n, err := w.Write(data)
	if err != nil {
		return fail(FailureProtocol, "worker pipe closed while writing: %v", err)
	}
	if n != len(data) {
		return fail(FailureProtocol, "worker pipe wrote %d of %d frame bytes", n, len(data))
	}
	return nil
}

type frameSizes struct {
	head, payload int64
}

func readFrameSizes(r io.Reader, limit int) (frameSizes, error) {
	var header [frameHeader]byte
	if _, err := io.ReadFull(r, header[:]); err != nil {
		if errors.Is(err, io.EOF) {
			return frameSizes{}, io.EOF
		}
		return frameSizes{}, fail(FailureProtocol, "worker frame header is truncated: %v", err)
	}
	rest := int64(binary.LittleEndian.Uint32(header[:4]))
	head := int64(binary.LittleEndian.Uint16(header[4:]))
	if rest < 2 || rest+4 > int64(limit) {
		return frameSizes{}, fail(FailureProtocol, "worker frame declares %d bytes; the limit is %d", rest+4, limit)
	}
	if head == 0 || head > EnvelopeLimit || head > rest-2 {
		return frameSizes{}, fail(FailureProtocol, "worker envelope declares %d bytes; the limit is %d", head, EnvelopeLimit)
	}
	return frameSizes{head: head, payload: rest - 2 - head}, nil
}

func readFrame(r io.Reader, limit int) (envelope, []byte, error) {
	return readFrameInto(r, limit, nil)
}

func readFrameInto(r io.Reader, limit int, scratch *[]byte) (envelope, []byte, error) {
	sizes, err := readFrameSizes(r, limit)
	if err != nil {
		return envelope{}, nil, err
	}
	head := make([]byte, sizes.head)
	if _, err := io.ReadFull(r, head); err != nil {
		return envelope{}, nil, fail(FailureProtocol, "worker envelope is truncated: %v", err)
	}
	var env envelope
	if err := json.Unmarshal(head, &env, json.RejectUnknownMembers(true)); err != nil {
		return envelope{}, nil, fail(FailureProtocol, "worker envelope is malformed: %v", err)
	}
	if int64(env.Payload) != sizes.payload {
		return envelope{}, nil, fail(FailureProtocol, "worker envelope declares a %d byte payload but the frame carries %d", env.Payload, sizes.payload)
	}
	payload := framePayload(env, scratch)
	if _, err := io.ReadFull(r, payload); err != nil {
		return envelope{}, nil, fail(FailureProtocol, "worker payload is truncated: %v", err)
	}
	return env, payload, nil
}

func framePayload(env envelope, scratch *[]byte) []byte {
	if scratch == nil || !clipboardChunk(env) {
		return make([]byte, env.Payload)
	}
	if cap(*scratch) < env.Payload {
		*scratch = make([]byte, env.Payload)
	}
	return (*scratch)[:env.Payload]
}

func clipboardChunk(env envelope) bool {
	return env.Type == messageChunk && (env.Format == clipboardimage.PNG || env.Format == clipboardimage.DIB)
}

func engineManifest() string { return hex.EncodeToString(EngineManifest[:]) }

func themeRoles(t Theme) []string {
	roles := []string{}
	for _, c := range [...]Color{t.Canvas, t.Surface, t.Tone, t.Line, t.Strong, t.Text, t.Dim, t.Accent} {
		roles = append(roles, c.hex())
	}
	return roles
}

func parseTheme(roles []string) (Theme, bool) {
	if len(roles) != 8 {
		return Theme{}, false
	}
	var colors [8]Color
	for i, r := range roles {
		c, ok := parseHexColor(trimHash(r))
		if !ok || len(r) != 7 {
			return Theme{}, false
		}
		colors[i] = c.color()
	}
	return Theme{Canvas: colors[0], Surface: colors[1], Tone: colors[2], Line: colors[3], Strong: colors[4], Text: colors[5], Dim: colors[6], Accent: colors[7]}, true
}

func trimHash(s string) string {
	if len(s) > 0 && s[0] == '#' {
		return s[1:]
	}
	return s
}

func digestOf(parts ...[]byte) string {
	h := sha256.New()
	for _, p := range parts {
		h.Write(p)
	}
	return hex.EncodeToString(h.Sum(nil))
}

func finiteScale(v float64) bool { return v > 0 && v <= 16 && !math.IsNaN(v) }

func failureEnvelope(job uint64, err error) *envelope {
	f := FailureOf(err)
	message := f.Message
	if len(message) > messageLimit {
		message = message[:messageLimit]
	}
	env := &envelope{Type: messageError, Job: job, Code: f.Code.String(), Message: message}
	if f.Span.Known() {
		env.Span = []int{f.Span.Line, f.Span.Column, f.Span.EndLine, f.Span.EndColumn}
	}
	return env
}

func failureFromEnvelope(env envelope) *Failure {
	code := FailureProtocol
	for c := FailureInvalid; c <= FailureProtocol; c++ {
		if c.String() == env.Code {
			code = c
		}
	}
	f := &Failure{Code: code, Message: env.Message}
	if len(env.Span) == 4 {
		f.Span = Span{Line: env.Span[0], Column: env.Span[1], EndLine: env.Span[2], EndColumn: env.Span[3]}
	}
	return f
}
