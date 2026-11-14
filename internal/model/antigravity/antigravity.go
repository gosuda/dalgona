package antigravity

import (
	"bytes"
	"cmp"
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"net/http"
	"runtime"
	"strconv"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/google"
)

const (
	DefaultBaseURL = "https://daily-cloudcode-pa.googleapis.com"
	clientVersion  = "2.26.0"
	clientChange   = "1005016104"
)

type Config struct {
	BaseURL        string
	Token          func(context.Context) (string, error)
	Project        string
	Client         *http.Client
	Retry          model.RetryOptions
	ContextWindows map[string]int
}

type Provider struct {
	cfg       Config
	client    *http.Client
	userAgent string
	project   *projectCache
}

func New(cfg Config) *Provider {
	cfg.BaseURL = strings.TrimRight(cmp.Or(cfg.BaseURL, DefaultBaseURL), "/")
	return &Provider{
		cfg:       cfg,
		client:    model.NewHTTPClient(cfg.Client),
		userAgent: userAgent(runtime.GOOS),
		project:   newProjectCache(cfg.Project),
	}
}

func userAgent(goos string) string {
	osType, arch := "linux", "amd64"
	switch goos {
	case "darwin":
		osType, arch = "darwin", "arm64"
	case "windows":
		osType = "windows"
	}
	return "antigravity/hub/" + clientVersion + " (aidev_client; os_type=" + osType + "; arch=" + arch + "; cl=" + clientChange + ")"
}

func (p *Provider) Stream(ctx context.Context, req model.Request) (model.Stream, error) {
	return model.LaunchStream(ctx, p.cfg.Retry, func(attemptCtx context.Context, stream *model.EventStream) model.AttemptResult {
		return p.attempt(attemptCtx, stream, req)
	})
}

func (p *Provider) token(ctx context.Context) (string, error) {
	if p.cfg.Token == nil {
		return "", errors.New("antigravity: not logged in; run /login antigravity")
	}
	token, err := p.cfg.Token(ctx)
	if err != nil {
		return "", fmt.Errorf("antigravity: token: %w", err)
	}
	return token, nil
}

func (p *Provider) attempt(ctx context.Context, stream *model.EventStream, req model.Request) model.AttemptResult {
	token, err := p.token(ctx)
	if err != nil {
		return model.AttemptResult{Status: http.StatusUnauthorized, Err: err}
	}
	project, err := p.Project(ctx)
	if err != nil {
		return model.AttemptResult{Status: controlStatus(err), Err: err}
	}
	body, err := encodeEnvelope(req, project)
	if err != nil {
		return model.AttemptResult{Err: err}
	}
	gateCtx, gate := model.NewStartGate(ctx, p.cfg.Retry.StartTimeout)
	defer gate.Close()
	httpReq, err := p.newRequest(gateCtx, http.MethodPost, "/v1internal:streamGenerateContent?alt=sse", body, token)
	if err != nil {
		return model.AttemptResult{Err: err}
	}
	resp, err := p.client.Do(httpReq)
	if err != nil {
		return model.AttemptResult{Err: gate.Error(gateCtx, fmt.Errorf("antigravity: request: %w", err))}
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return model.HTTPFailure("antigravity", resp)
	}
	options := model.SSEOptions{IdleTimeout: p.cfg.Retry.IdleTimeout, Control: stream.Timers()}
	return consume(gateCtx, gate, resp.Body, google.NewDecoder(stream, req.Tools), options)
}

func (p *Provider) newRequest(ctx context.Context, method, path string, body []byte, token string) (*http.Request, error) {
	var reader io.Reader
	if body != nil {
		reader = bytes.NewReader(body)
	}
	httpReq, err := http.NewRequestWithContext(ctx, method, p.cfg.BaseURL+path, reader)
	if err != nil {
		return nil, fmt.Errorf("antigravity: create request: %w", err)
	}
	httpReq.Header.Set("Authorization", "Bearer "+token)
	httpReq.Header.Set("Content-Type", "application/json")
	httpReq.Header.Set("User-Agent", p.userAgent)
	return httpReq, nil
}

func encodeEnvelope(req model.Request, project string) ([]byte, error) {
	inner, err := google.EncodeRequest(req)
	if err != nil {
		return nil, fmt.Errorf("antigravity: %w", err)
	}
	requestID, err := newRequestID(len(req.Messages))
	if err != nil {
		return nil, err
	}
	var buffer bytes.Buffer
	buffer.Grow(len(inner) + 256)
	encoder := model.NewJSONEncoder(&buffer)
	encoder.BeginObject()
	encoder.StringKey("project", project)
	encoder.StringKey("requestId", requestID)
	encoder.ValueKey("request", jsontext.Value(inner))
	encoder.StringKey("model", wireModel(req.Model, req.Reasoning))
	encoder.StringKey("userAgent", "antigravity")
	encoder.EndObject()
	if err := encoder.Err(); err != nil {
		return nil, fmt.Errorf("antigravity: encode request: %w", err)
	}
	return buffer.Bytes(), nil
}

func newRequestID(messages int) (string, error) {
	var ids [32]byte
	if _, err := rand.Read(ids[:]); err != nil {
		return "", fmt.Errorf("antigravity: request id: %w", err)
	}
	agent, trajectory := hex.EncodeToString(ids[:16]), hex.EncodeToString(ids[16:])
	step := strconv.Itoa(max(messages, 1))
	return "agent/" + agent + "/" + strconv.FormatInt(time.Now().UnixMilli(), 10) + "/" + trajectory + "/" + step, nil
}

func consume(ctx context.Context, gate *model.StartGate, body io.ReadCloser, decoder *google.Decoder, options model.SSEOptions) model.AttemptResult {
	var failed []byte
	err := model.ReadSSEWithTimeouts(ctx, body, options, func(_, data []byte) error {
		gate.FirstEvent()
		if err := decodeChunk(decoder, data); err != nil {
			failed = bytes.Clone(data)
			return fmt.Errorf("antigravity: %w", err)
		}
		return nil
	})
	err = gate.Error(ctx, err)
	if err == nil {
		err = decoder.Finish()
	}
	return model.AttemptResult{Status: http.StatusOK, SawTool: decoder.SawTool(), SawContent: decoder.SawContent(), Body: failed, Err: err}
}

func decodeChunk(decoder *google.Decoder, data []byte) error {
	reader := jsontext.NewDecoder(bytes.NewReader(data))
	if reader.PeekKind() == '[' {
		return decodeArrayChunk(decoder, data, reader)
	}
	response, err := extractResponse(data, reader)
	if err != nil {
		return err
	}
	if err := model.RequireJSONEOF(reader); err != nil {
		return err
	}
	if response == nil {
		return decoder.Decode(data)
	}
	return decoder.Decode(response)
}

func decodeArrayChunk(decoder *google.Decoder, data []byte, reader *jsontext.Decoder) error {
	arrayErr := model.ReadArrayItems(reader, func(int) error {
		start := valueStart(data, reader.InputOffset())
		response, err := extractResponse(data, reader)
		if err != nil {
			return err
		}
		if response == nil {
			response = data[start:reader.InputOffset()]
		}
		return decoder.Decode(response)
	})
	if arrayErr != nil {
		return arrayErr
	}
	return model.RequireJSONEOF(reader)
}

func valueStart(data []byte, start int64) int64 {
	for start < int64(len(data)) {
		switch data[start] {
		case ',', ' ', '\t', '\n', '\r':
			start++
			continue
		}
		return start
	}
	return start
}

func extractResponse(data []byte, reader *jsontext.Decoder) ([]byte, error) {
	var response []byte
	err := model.ReadObjectFields(reader, func(key string) error {
		if key != "response" {
			return reader.SkipValue()
		}
		value, err := reader.ReadValue()
		end := reader.InputOffset()
		response = data[end-int64(len(value)) : end]
		return err
	})
	if err != nil {
		return nil, fmt.Errorf("decode chunk: %w", err)
	}
	return response, nil
}
