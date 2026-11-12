package agent

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json/jsontext"
	"errors"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/tool"
)

func TestImageMIME(t *testing.T) {
	tests := []struct {
		name string
		data string
		want string
		err  string
	}{
		{name: "png", data: "\x89PNG\r\n\x1a\n", want: "image/png"},
		{name: "jpeg", data: "\xff\xd8\xff\xe0", want: "image/jpeg"},
		{name: "gif", data: "GIF89a", want: "image/gif"},
		{name: "webp refused", data: "RIFF\x00\x00\x00\x00WEBPVP8 ", err: "unsupported image type image/webp; send PNG, JPEG or GIF"},
		{name: "empty refused", data: "", err: "unsupported image type text/plain; charset=utf-8; send PNG, JPEG or GIF"},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got, err := ImageMIME([]byte(tc.data))
			if tc.err != "" {
				if err == nil || err.Error() != tc.err {
					t.Fatalf("ImageMIME error = %v, want %q", err, tc.err)
				}
				return
			}
			if err != nil || got != tc.want {
				t.Fatalf("ImageMIME = %q, %v; want %q", got, err, tc.want)
			}
		})
	}
}

func TestCheckImages(t *testing.T) {
	oneByte := func(n int) []Image {
		images := make([]Image, n)
		for i := range images {
			images[i] = Image{Mime: "image/png", Data: make([]byte, 1)}
		}
		return images
	}
	tests := []struct {
		name   string
		images []Image
		err    string
	}{
		{name: "ten images pass", images: oneByte(10)},
		{name: "eleven images refused", images: oneByte(11), err: "a request takes at most 10 images, got 11"},
		{
			name:   "one image over the limit",
			images: []Image{{Mime: "image/png", Data: make([]byte, 5242881)}},
			err:    "image 1 is 5242881 bytes; the limit is 5242880 bytes (5 MiB)",
		},
		{
			name: "three images over the total",
			images: []Image{
				{Mime: "image/png", Data: make([]byte, 4194304)},
				{Mime: "image/png", Data: make([]byte, 4194304)},
				{Mime: "image/png", Data: make([]byte, 4194304)},
			},
			err: "images total 12582912 bytes; the limit is 10485760 bytes (10 MiB) per request",
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			err := CheckImages(tc.images)
			if tc.err == "" {
				if err != nil {
					t.Fatalf("CheckImages = %v, want nil", err)
				}
				return
			}
			if err == nil || err.Error() != tc.err {
				t.Fatalf("CheckImages = %v, want %q", err, tc.err)
			}
		})
	}
}

func TestSubmitImageStoresBlobAndMasksPath(t *testing.T) {
	t.Setenv("TEST_API_KEY", "sk-test-0123456789")
	store, err := auth.NewWithPatterns(t.TempDir(), []string{`token-[a-z0-9]{8,}`})
	if err != nil {
		t.Fatal(err)
	}
	h := newHarness(t, func(h *harness) { h.cfg.Auth = store })
	h.model.push(text("done"))
	thread := h.start()
	data := []byte("\x89PNG\r\n\x1a\nxyz")
	err = thread.Submit(t.Context(), Input{
		Kind: InputPrompt,
		Text: "what is this",
		Images: []Image{{
			Path: "/tmp/keys/sk-test-0123456789/shot.png",
			Mime: "image/png",
			Data: data,
		}},
	})
	if err != nil {
		t.Fatalf("submit: %v", err)
	}
	h.waitIdle(thread)
	message := storedImageMessage(t, h, thread.ID())
	if len(message.Images) != 1 {
		t.Fatalf("images = %d, want 1", len(message.Images))
	}
	image := message.Images[0]
	if want := "/tmp/keys/{{secret:TEST_API_KEY}}/shot.png"; image.Path != want {
		t.Fatalf("image path = %q, want %q", image.Path, want)
	}
	sum := sha256.Sum256(data)
	if want := hex.EncodeToString(sum[:]); image.Blob.Blob != want {
		t.Fatalf("image blob = %q, want %q", image.Blob.Blob, want)
	}
	if image.Blob.Bytes != int64(len(data)) {
		t.Fatalf("image bytes = %d, want %d", image.Blob.Bytes, len(data))
	}
}

func storedImageMessage(t *testing.T, h *harness, id string) event.UserMessage {
	t.Helper()
	for _, item := range h.events(id) {
		if message, ok := item.Event.(event.UserMessage); ok && len(message.Images) > 0 {
			return message
		}
	}
	t.Fatal("no user message with images in journal")
	return event.UserMessage{}
}

type requestRecorder struct {
	provider model.Provider
	mu       sync.Mutex
	requests []model.Request
}

func (r *requestRecorder) Stream(ctx context.Context, req model.Request) (model.Stream, error) {
	r.mu.Lock()
	r.requests = append(r.requests, req)
	r.mu.Unlock()
	return r.provider.Stream(ctx, req)
}

func (r *requestRecorder) Models(ctx context.Context) ([]model.Model, error) {
	return r.provider.Models(ctx)
}

func (r *requestRecorder) snapshot() []model.Request {
	r.mu.Lock()
	defer r.mu.Unlock()
	return slices.Clone(r.requests)
}

func recordRequests(h *harness) *requestRecorder {
	rec := &requestRecorder{}
	h.cfg.Providers = func(onRetry RetryFunc) (*model.Registry, error) {
		inner, err := h.providers(onRetry)
		if err != nil {
			return nil, err
		}
		provider, ok := inner.Provider("openai")
		if !ok {
			return nil, errors.New("openai provider is not registered")
		}
		rec.provider = provider
		registry := model.NewRegistry()
		return registry, registry.Register("openai", rec)
	}
	return rec
}

func imagesOf(request model.Request) []model.Image {
	for _, message := range request.Messages {
		if len(message.Images) > 0 {
			return message.Images
		}
	}
	return nil
}

type dropBlobTool struct{ path string }

func (dropBlobTool) Spec() tool.Spec {
	return tool.Spec{Name: "drop_blob", Description: "remove the image blob", Schema: jsontext.Value(`{"type":"object"}`)}
}

func (dropBlobTool) Class() tool.Class { return tool.ClassRead }

func (d dropBlobTool) Run(context.Context, tool.Call, tool.Env) (tool.Result, error) {
	if err := os.Remove(d.path); err != nil {
		return tool.Result{}, err
	}
	return tool.Result{Output: []byte("dropped"), OK: true}, nil
}

func TestRecoveredTurnSendsImageFromBlob(t *testing.T) {
	var rec *requestRecorder
	h := newHarness(t, func(h *harness) { rec = recordRequests(h) })
	data := []byte("\x89PNG\r\n\x1a\nabc")
	sha, err := h.journal.PutBlob(data)
	if err != nil {
		t.Fatal(err)
	}
	h.seed(
		event.TurnStarted{TurnID: "t"},
		event.UserMessage{ItemID: "u", TurnID: "t", Text: "what is this", Kind: "prompt", Images: []event.Image{{
			Mime: "image/png", Path: "/nonexistent/shot.png", Blob: event.BlobRef{Blob: sha, Bytes: int64(len(data))},
		}}},
	)
	h.model.push(text("done"))
	h.recoverSession()
	requests := rec.snapshot()
	if len(requests) == 0 {
		t.Fatal("recovered turn sent no model request")
	}
	images := imagesOf(requests[0])
	if len(images) != 1 || !bytes.Equal(images[0].Data, data) || images[0].Mime != "image/png" {
		t.Fatalf("recovered request images = %+v, want the blob bytes", images)
	}
}

func TestImageBlobMismatchFailsTurn(t *testing.T) {
	h := newHarness(t)
	data := []byte("\x89PNG\r\n\x1a\nabc")
	sha, err := h.journal.PutBlob(data)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(h.home, "blobs", sha), []byte("\x89PNG\r\n\x1a\nzzz"), 0o600); err != nil {
		t.Fatal(err)
	}
	h.seed(
		event.TurnStarted{TurnID: "t"},
		event.UserMessage{ItemID: "u", TurnID: "t", Text: "what is this", Kind: "prompt", Images: []event.Image{{
			Mime: "image/png", Path: "/nonexistent/shot.png", Blob: event.BlobRef{Blob: sha, Bytes: int64(len(data))},
		}}},
	)
	events := h.recoverSession()
	turn := lastTurn(events)
	want := "agent: image blob " + sha + " does not match its journal record"
	if turn.Status != statusFailed || turn.Error != want {
		t.Fatalf("turn = %q %q, want failed with %q", turn.Status, turn.Error, want)
	}
}

func TestImageBlobReadOncePerThread(t *testing.T) {
	var rec *requestRecorder
	data := []byte("\x89PNG\r\n\x1a\nabc")
	var sha string
	h := newHarness(t, func(h *harness) {
		rec = recordRequests(h)
		h.cfg.Tools = func(context.Context, string, string) ([]tool.Tool, error) {
			return []tool.Tool{dropBlobTool{path: filepath.Join(h.home, "blobs", sha)}}, nil
		}
	})
	sha, err := h.journal.PutBlob(data)
	if err != nil {
		t.Fatal(err)
	}
	h.model.push(calls(call{id: "c1", name: "drop_blob", args: `{}`}), text("done"))
	thread := h.start()
	err = thread.Submit(t.Context(), Input{
		Kind: InputPrompt, Text: "look",
		Images: []Image{{Mime: "image/png", Data: data}},
	})
	if err != nil {
		t.Fatalf("submit: %v", err)
	}
	h.waitIdle(thread)
	requests := rec.snapshot()
	if len(requests) != 2 {
		t.Fatalf("model steps = %d, want 2", len(requests))
	}
	images := imagesOf(requests[1])
	if len(images) != 1 || !bytes.Equal(images[0].Data, data) {
		t.Fatalf("second step images = %+v, want the cached bytes", images)
	}
	if turn := lastTurn(h.events(thread.ID())); turn.Status != statusCompleted {
		t.Fatalf("turn status = %q, want completed", turn.Status)
	}
	thread.images.mu.Lock()
	loads := thread.images.loads
	thread.images.mu.Unlock()
	if loads != 1 {
		t.Fatalf("journal blob reads = %d, want 1 for two model steps", loads)
	}
}

func TestImageRecordCheckedOnCacheHit(t *testing.T) {
	data := []byte("\x89PNG\r\n\x1a\nabc")
	h := newHarness(t)
	sha, err := h.journal.PutBlob(data)
	if err != nil {
		t.Fatal(err)
	}
	thread := h.start()
	record := event.Image{Mime: "image/png", Blob: event.BlobRef{Blob: sha, Bytes: int64(len(data))}}
	if _, err := thread.verifiedImage(record); err != nil {
		t.Fatalf("load: %v", err)
	}
	for _, tc := range []struct {
		name  string
		image event.Image
	}{
		{"mime", event.Image{Mime: "image/jpeg", Blob: record.Blob}},
		{"bytes", event.Image{Mime: "image/png", Blob: event.BlobRef{Blob: sha, Bytes: record.Blob.Bytes + 1}}},
	} {
		if _, err := thread.verifiedImage(tc.image); err == nil || !strings.Contains(err.Error(), "does not match its journal record") {
			t.Errorf("%s mismatch on a cache hit: err = %v, want a journal record mismatch", tc.name, err)
		}
	}
}
