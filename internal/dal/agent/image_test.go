package agent

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"testing"

	json "encoding/json/v2"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/model"
	"github.com/gosuda/dalgona/internal/dal/model/chatcompat"
	"github.com/gosuda/dalgona/internal/dal/tool"
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
		{
			name: "exact image and total limits pass",
			images: []Image{
				{Mime: "image/png", Data: make([]byte, event.MaxImageBytes)},
				{Mime: "image/png", Data: make([]byte, event.MaxImageBytes)},
			},
		},
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
	h.model.push(text("done"))
	plain := h.start()
	err = plain.Submit(t.Context(), Input{
		Kind: InputPrompt,
		Text: "plain",
		Images: []Image{{
			Path: "/tmp/shots/plain.png",
			Mime: "image/png",
			Data: data,
		}},
	})
	if err != nil {
		t.Fatalf("submit: %v", err)
	}
	h.waitIdle(plain)
	if path := storedImageMessage(t, h, plain.ID()).Images[0].Path; path != "/tmp/shots/plain.png" {
		t.Fatalf("nonsecret path = %q, want it unchanged", path)
	}
}

func TestSubmitImageMasksNameAndAlt(t *testing.T) {
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
			Path:    "/tmp/shots/plain.png",
			Mime:    "image/png",
			Data:    data,
			Name:    "token-abcdefgh.png",
			AltText: "a token-abcdefgh diagram",
		}},
	})
	if err != nil {
		t.Fatalf("submit: %v", err)
	}
	h.waitIdle(thread)
	image := storedImageMessage(t, h, thread.ID()).Images[0]
	if strings.Contains(image.Name, "token-abcdefgh") || !strings.Contains(image.Name, "{{secret:") {
		t.Fatalf("image name = %q, want the secret masked", image.Name)
	}
	if strings.Contains(image.AltText, "token-abcdefgh") || !strings.Contains(image.AltText, "{{secret:") {
		t.Fatalf("image alt = %q, want the secret masked", image.AltText)
	}
	if image.Path != "/tmp/shots/plain.png" {
		t.Fatalf("image path = %q, want it unchanged", image.Path)
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

func TestUserMessageImageJournalForm(t *testing.T) {
	data := []byte("\x89PNG\r\n\x1a\nxyz")
	h := newHarness(t)
	h.model.push(text("done"), text("done"))
	thread := h.start()
	digest := requireImageJournalForm(t, h, thread, data)
	requireStoredBlob(t, h, data, digest)
	requireTextOnlyMessage(t, h, thread)
	requireMissingBlobFails(t, data)
}

func requireImageJournalForm(t *testing.T, h *harness, thread *Thread, data []byte) string {
	t.Helper()
	err := thread.Submit(t.Context(), Input{Kind: InputPrompt, Text: "what is this", Images: []Image{{Mime: "image/png", Data: data}}})
	if err != nil {
		t.Fatalf("submit: %v", err)
	}
	h.waitIdle(thread)
	records := journalUserMessages(t, h, thread.ID())
	if len(records) != 1 {
		t.Fatalf("user messages = %d, want 1", len(records))
	}
	image := journalImage(t, records[0])
	sum := sha256.Sum256(data)
	digest := hex.EncodeToString(sum[:])
	if image.mime != http.DetectContentType(data) || image.blob != digest || image.bytes != int64(len(data)) {
		t.Fatalf("image = %+v, want %s at %s with %d bytes", image, http.DetectContentType(data), digest, len(data))
	}
	return digest
}

func requireStoredBlob(t *testing.T, h *harness, data []byte, digest string) {
	t.Helper()
	stored, err := os.ReadFile(filepath.Join(h.home, "blobs", digest))
	if err != nil {
		t.Fatalf("blob %s: %v", digest, err)
	}
	if !bytes.Equal(stored, data) {
		t.Fatalf("blob bytes = %x, want the submitted bytes", stored)
	}
}

func requireTextOnlyMessage(t *testing.T, h *harness, thread *Thread) {
	t.Helper()
	err := thread.Submit(t.Context(), Input{Kind: InputPrompt, Text: "plain"})
	if err != nil {
		t.Fatalf("submit: %v", err)
	}
	h.waitIdle(thread)
	records := journalUserMessages(t, h, thread.ID())
	if len(records) != 2 {
		t.Fatalf("user messages = %d, want 2", len(records))
	}
	last := records[len(records)-1]
	if _, ok := last["images"]; ok {
		t.Fatalf("text-only user message carries images: %v", last)
	}
	if last["text"] != "plain" {
		t.Fatalf("text-only message = %v, want the submitted text", last)
	}
}

func requireMissingBlobFails(t *testing.T, data []byte) {
	t.Helper()
	missing := strings.Repeat("b", 64)
	var rec *requestRecorder
	h2 := newHarness(t, func(h *harness) { rec = recordRequests(h) })
	h2.seed(
		event.TurnStarted{TurnID: "t"},
		event.UserMessage{ItemID: "u", TurnID: "t", Text: "what is this", Kind: "prompt", Images: []event.Image{{
			Mime: "image/png", Blob: event.BlobRef{Blob: missing, Bytes: int64(len(data))},
		}}},
	)
	turn := lastTurn(h2.recoverSession())
	prefix := "agent: load image blob " + missing + ":"
	if turn.Status != statusFailed || !strings.HasPrefix(turn.Error, prefix) {
		t.Fatalf("turn = %q %q, want failed with %q", turn.Status, turn.Error, prefix)
	}
	if requests := rec.snapshot(); len(requests) != 0 {
		t.Fatalf("missing blob sent %d model requests, want 0", len(requests))
	}
}

type journalImageRecord struct {
	mime  string
	blob  string
	bytes int64
}

func journalImage(t *testing.T, record map[string]any) journalImageRecord {
	t.Helper()
	images, ok := record["images"].([]any)
	if !ok || len(images) != 1 {
		t.Fatalf("images = %v, want one entry", record["images"])
	}
	entry, ok := images[0].(map[string]any)
	if !ok {
		t.Fatalf("image entry = %v, want an object", images[0])
	}
	blob, ok := entry["image"].(map[string]any)
	if !ok {
		t.Fatalf("image entry = %v, want a blob reference", entry)
	}
	size, ok := blob["bytes"].(float64)
	if !ok {
		t.Fatalf("image bytes = %v, want a number", blob["bytes"])
	}
	return journalImageRecord{mime: fmt.Sprint(entry["mime"]), blob: fmt.Sprint(blob["blob"]), bytes: int64(size)}
}

func journalUserMessages(t *testing.T, h *harness, id string) []map[string]any {
	t.Helper()
	var out []map[string]any
	err := h.journal.Replay(id, func(_ uint64, raw []byte) error {
		var record map[string]any
		if err := json.Unmarshal(raw, &record); err != nil {
			return err
		}
		if record["type"] == "user.message" {
			out = append(out, record)
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	return out
}

func TestSubmitImageTypeMismatchRefused(t *testing.T) {
	h := newHarness(t)
	thread := h.start()
	err := thread.Submit(t.Context(), Input{Kind: InputPrompt, Text: "look", Images: []Image{{Mime: "image/jpeg", Data: []byte("\x89PNG\r\n\x1a\nxyz")}}})
	want := "image type image/png does not match its declared type image/jpeg"
	if err == nil || err.Error() != want {
		t.Fatalf("submit error = %v, want %q", err, want)
	}
	if messages := journalUserMessages(t, h, thread.ID()); len(messages) != 0 {
		t.Fatalf("refused submit appended %d user messages, want 0", len(messages))
	}
}

func TestSubmitImageMaskFailureFailsSubmit(t *testing.T) {
	authHome := t.TempDir()
	store, err := auth.NewWithPatterns(authHome, []string{`token-[a-z0-9]{8,}`})
	if err != nil {
		t.Fatal(err)
	}
	h := newHarness(t, func(h *harness) { h.cfg.Auth = store })
	thread := h.start()
	breakAuthHome(t, authHome)
	err = thread.Submit(t.Context(), Input{
		Kind: InputPrompt, Text: "look",
		Images: []Image{{Path: "/tmp/token-abcdefgh/shot.png", Mime: "image/png", Data: []byte("\x89PNG\r\n\x1a\nxyz")}},
	})
	if err == nil || !strings.Contains(err.Error(), "secret masking failed") {
		t.Fatalf("submit error = %v, want a masking failure", err)
	}
	if store.MaskError() == nil {
		t.Fatal("store reports no mask error after the refused submit")
	}
	if messages := journalUserMessages(t, h, thread.ID()); len(messages) != 0 {
		t.Fatalf("refused submit appended %d user messages, want 0", len(messages))
	}
}

type requestRecorder struct {
	provider model.Provider
	images   bool
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
	models, err := r.provider.Models(ctx)
	for i := range models {
		models[i].Images = r.images
	}
	return models, err
}

func (r *requestRecorder) snapshot() []model.Request {
	r.mu.Lock()
	defer r.mu.Unlock()
	return slices.Clone(r.requests)
}

func openaiProvider(h *harness, onRetry RetryFunc) (model.Provider, error) {
	inner, err := h.providers(onRetry)
	if err != nil {
		return nil, err
	}
	provider, ok := inner.Provider("openai")
	if !ok {
		return nil, errors.New("openai provider is not registered")
	}
	return provider, nil
}

func recordRequests(h *harness) *requestRecorder {
	rec := &requestRecorder{images: true}
	h.cfg.Providers = func(onRetry RetryFunc) (*model.Registry, error) {
		provider, err := openaiProvider(h, onRetry)
		if err != nil {
			return nil, err
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
	var rec *requestRecorder
	h := newHarness(t, func(h *harness) { rec = recordRequests(h) })
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
	if requests := rec.snapshot(); len(requests) != 0 {
		t.Fatalf("corrupted blob sent %d model requests, want 0", len(requests))
	}
}

func TestImageBlobRecordMismatchOnCacheMiss(t *testing.T) {
	data := []byte("\x89PNG\r\n\x1a\nabc")
	for _, tc := range []struct {
		name  string
		mime  string
		bytes int64
	}{
		{"bytes", "image/png", int64(len(data)) + 1},
		{"mime", "image/jpeg", int64(len(data))},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var rec *requestRecorder
			h := newHarness(t, func(h *harness) { rec = recordRequests(h) })
			sha, err := h.journal.PutBlob(data)
			if err != nil {
				t.Fatal(err)
			}
			h.seed(
				event.TurnStarted{TurnID: "t"},
				event.UserMessage{ItemID: "u", TurnID: "t", Text: "what is this", Kind: "prompt", Images: []event.Image{{
					Mime: tc.mime, Blob: event.BlobRef{Blob: sha, Bytes: tc.bytes},
				}}},
			)
			turn := lastTurn(h.recoverSession())
			want := "agent: image blob " + sha + " does not match its journal record"
			if turn.Status != statusFailed || turn.Error != want {
				t.Fatalf("turn = %q %q, want failed with %q", turn.Status, turn.Error, want)
			}
			if requests := rec.snapshot(); len(requests) != 0 {
				t.Fatalf("mismatched record sent %d model requests, want 0", len(requests))
			}
		})
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
	thread.images.stateMu.Lock()
	loads := thread.images.loads
	thread.images.stateMu.Unlock()
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

func TestTextOnlyModelRefusesCurrentTurnImage(t *testing.T) {
	h := newHarness(t, func(h *harness) {
		h.cfg.Providers = func(onRetry RetryFunc) (*model.Registry, error) {
			provider, err := openaiProvider(h, onRetry)
			if err != nil {
				return nil, err
			}
			registry := model.NewRegistry()
			return registry, registry.Register("openai", &requestRecorder{provider: provider})
		}
	})
	thread := h.start()
	err := thread.Submit(t.Context(), Input{
		Kind: InputPrompt, Text: "what is this",
		Images: []Image{{Mime: "image/png", Data: []byte("\x89PNG\r\n\x1a\nabc")}},
	})
	if err != nil {
		t.Fatalf("submit: %v", err)
	}
	h.waitIdle(thread)
	turn := lastTurn(h.events(thread.ID()))
	want := "this model does not accept images"
	if turn.Status != statusFailed || turn.Error != want {
		t.Fatalf("turn = %q %q, want failed with %q", turn.Status, turn.Error, want)
	}
	if n := h.model.count(); n != 0 {
		t.Fatalf("refused turn sent %d model requests, want 0", n)
	}
}

func TestTextOnlyModelOmitsHistoryImage(t *testing.T) {
	visionRec := &requestRecorder{images: true}
	plainRec := &requestRecorder{}
	h := newHarness(t, func(h *harness) {
		h.cfg.ProviderIDs = []string{"openai", "vision"}
		h.cfg.Providers = func(onRetry RetryFunc) (*model.Registry, error) {
			provider, err := openaiProvider(h, onRetry)
			if err != nil {
				return nil, err
			}
			visionRec.provider, plainRec.provider = provider, provider
			registry := model.NewRegistry()
			if err := registry.Register("vision", visionRec); err != nil {
				return nil, err
			}
			return registry, registry.Register("openai", plainRec)
		}
	})
	thread := h.start()
	submit := func(in Input) {
		t.Helper()
		if err := thread.Submit(t.Context(), in); err != nil {
			t.Fatalf("submit: %v", err)
		}
		h.waitIdle(thread)
	}
	png := []byte("\x89PNG\r\n\x1a\nabc")
	submit(Input{Kind: InputPrompt, Text: "what is this", Model: "vision/gpt-test", Images: []Image{{Mime: "image/png", Data: png}}})
	submit(Input{Kind: InputPrompt, Text: "and now", Model: "openai/gpt-test"})
	visionRequests := visionRec.snapshot()
	if len(visionRequests) == 0 {
		t.Fatal("image turn sent no request to the vision model")
	}
	if sent := imagesOf(visionRequests[0]); len(sent) != 1 || !bytes.Equal(sent[0].Data, png) || sent[0].Mime != "image/png" {
		t.Fatalf("vision request images = %+v, want the submitted PNG", sent)
	}
	requireOmittedImageNotice(t, plainRec.snapshot())
}

func requireOmittedImageNotice(t *testing.T, requests []model.Request) {
	t.Helper()
	if len(requests) == 0 {
		t.Fatal("text-only turn sent no model request")
	}
	last := requests[len(requests)-1]
	noticed := false
	for _, message := range last.Messages {
		if len(message.Images) > 0 {
			t.Fatalf("text-only request carries %d images", len(message.Images))
		}
		noticed = noticed || strings.Contains(message.Text, "[image omitted: this model does not accept images]")
	}
	if !noticed {
		t.Fatalf("text-only request holds no omitted-image notice: %+v", last.Messages)
	}
}

func TestChatCompatRefusesImageOutsideListing(t *testing.T) {
	png := []byte("\x89PNG\r\n\x1a\nabc")
	tests := []struct {
		name    string
		catalog string
		images  bool
	}{
		{"model absent", `{"data":[{"id":"other-model","architecture":{"input_modalities":["text","image"]}}]}`, false},
		{"text-only model", `{"data":[{"id":"gpt-test","architecture":{"input_modalities":["text"]}}]}`, false},
		{"vision model", `{"data":[{"id":"gpt-test","architecture":{"input_modalities":["text","image"]}}]}`, true},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			serverURL, sent := chatCompatImageServer(t, tc.catalog)
			h := chatCompatHarness(t, serverURL)
			thread := h.start()
			err := thread.Submit(t.Context(), Input{Kind: InputPrompt, Text: "what is this", Images: []Image{{Mime: "image/png", Data: png}}})
			if err != nil {
				t.Fatalf("submit: %v", err)
			}
			h.waitIdle(thread)
			turn := lastTurn(h.events(thread.ID()))
			if tc.images {
				requireVisionTurnSentImage(t, turn, sent(), png)
				return
			}
			requireImageRefusedOutsideListing(t, turn, sent())
		})
	}
}

func chatCompatImageServer(t *testing.T, catalog string) (string, func() [][]byte) {
	t.Helper()
	var mu sync.Mutex
	var bodies [][]byte
	mux := http.NewServeMux()
	mux.HandleFunc("GET /models", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(w, catalog)
	})
	mux.HandleFunc("POST /chat/completions", func(w http.ResponseWriter, r *http.Request) {
		body, err := io.ReadAll(r.Body)
		if err != nil {
			t.Errorf("read chat request: %v", err)
		}
		mu.Lock()
		bodies = append(bodies, body)
		mu.Unlock()
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = io.WriteString(w, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")
	})
	server := httptest.NewServer(mux)
	t.Cleanup(server.Close)
	return server.URL, func() [][]byte {
		mu.Lock()
		defer mu.Unlock()
		return slices.Clone(bodies)
	}
}

func chatCompatHarness(t *testing.T, serverURL string) *harness {
	t.Helper()
	return newHarness(t, func(h *harness) {
		h.cfg.Config.Model = "chat/gpt-test"
		h.cfg.Providers = func(onRetry RetryFunc) (*model.Registry, error) {
			retry := model.DefaultRetryOptions()
			retry.Sleep = h.sleep
			retry.OnRetry = onRetry
			registry := model.NewRegistry()
			return registry, registry.Register("chat", chatcompat.New(chatcompat.Config{
				BaseURL: serverURL,
				APIKey:  func(context.Context) (string, error) { return "test-key", nil },
				Retry:   retry,
			}))
		}
	})
}

func requireImageRefusedOutsideListing(t *testing.T, turn event.TurnCompleted, sent [][]byte) {
	t.Helper()
	want := "this model does not accept images"
	if turn.Status != statusFailed || turn.Error != want {
		t.Fatalf("turn = %q %q, want failed with %q", turn.Status, turn.Error, want)
	}
	if len(sent) != 0 {
		t.Fatalf("refused turn sent %d chat requests, want 0", len(sent))
	}
}

func requireVisionTurnSentImage(t *testing.T, turn event.TurnCompleted, sent [][]byte, png []byte) {
	t.Helper()
	if turn.Status != statusCompleted {
		t.Fatalf("turn = %q %q, want completed", turn.Status, turn.Error)
	}
	if len(sent) != 1 {
		t.Fatalf("vision turn sent %d chat requests, want 1", len(sent))
	}
	if !bytes.Contains(sent[0], []byte(base64.StdEncoding.EncodeToString(png))) {
		t.Fatalf("vision request carries no submitted image: %s", sent[0])
	}
}
