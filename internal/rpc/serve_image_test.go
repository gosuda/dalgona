package rpc

import (
	"encoding/base64"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
)

const pngSample = "\x89PNG\r\n\x1a\ndata"

func writeImage(t *testing.T, name, data string) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), name)
	if err := os.WriteFile(path, []byte(data), 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

func userMessages(t *testing.T, r *rig, threadID string) []event.UserMessage {
	t.Helper()
	thread, err := r.host.Resume(t.Context(), threadID)
	if err != nil {
		t.Fatal(err)
	}
	records, _, err := thread.Records()
	if err != nil {
		t.Fatal(err)
	}
	var out []event.UserMessage
	for _, item := range records {
		if message, ok := item.Event.(event.UserMessage); ok {
			out = append(out, message)
		}
	}
	return out
}

func TestTurnStartLocalImagePath(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	path := writeImage(t, "shot.png", pngSample)
	input := []map[string]string{{"type": "text", "text": "what is this"}, {"type": "localImage", "path": path}}
	var response TurnStartResponse
	c.ok("turn/start", map[string]any{"threadId": threadID, "input": input}, &response)
	c.collect("turn/completed")
	messages := userMessages(t, r, threadID)
	if len(messages) != 1 || len(messages[0].Images) != 1 {
		t.Fatalf("user messages %+v, want one message with one image", messages)
	}
	image := messages[0].Images[0]
	if image.Mime != "image/png" || image.Blob.Bytes != int64(len(pngSample)) || image.Path != path {
		t.Fatalf("image %+v, want image/png, %d bytes, path %s", image, len(pngSample), path)
	}
}

func TestTurnStartImageDataURL(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	input := []map[string]string{{"type": "image", "url": "data:image/png;base64,iVBORw0KGgo=", "detail": "high"}}
	var response TurnStartResponse
	c.ok("turn/start", map[string]any{"threadId": threadID, "input": input}, &response)
	c.collect("turn/completed")
	messages := userMessages(t, r, threadID)
	if len(messages) != 1 || len(messages[0].Images) != 1 || messages[0].Images[0].Blob.Bytes != 8 {
		t.Fatalf("user messages %+v, want one 8-byte image", messages)
	}
}

func TestTurnImageErrors(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	large := writeImage(t, "large.png", "")
	if err := os.Truncate(large, 5<<20+1); err != nil {
		t.Fatal(err)
	}
	webp := base64.StdEncoding.EncodeToString([]byte("RIFF\x00\x00\x00\x00WEBPVP8 "))
	png := base64.StdEncoding.EncodeToString([]byte(pngSample))
	dir := t.TempDir()
	missing := filepath.Join(dir, "missing.png")
	for _, tc := range []struct {
		name string
		item map[string]string
		text string
	}{
		{"relative path", map[string]string{"type": "localImage", "path": "shot.png"}, "image path must be absolute: shot.png"},
		{"missing file", map[string]string{"type": "localImage", "path": missing}, "image file not found: " + missing},
		{"directory", map[string]string{"type": "localImage", "path": dir}, "image path must be a regular file: " + dir},
		{"https url", map[string]string{"type": "image", "url": "https://example.test/shot.png"}, "image url must be a data: URL with base64 data"},
		{"webp", map[string]string{"type": "image", "url": "data:image/webp;base64," + webp}, "unsupported image type image/webp; send PNG, JPEG or GIF"},
		{"declared type", map[string]string{"type": "image", "url": "data:image/jpeg;base64," + png}, "image type image/png does not match the data URL type image/jpeg"},
		{"too large", map[string]string{"type": "localImage", "path": large}, "image 1 is 5242881 bytes; the limit is 5242880 bytes (5 MiB)"},
		{"bad base64", map[string]string{"type": "image", "url": "data:image/png;base64,!!!"}, "image data is not valid base64"},
		{"long path", map[string]string{"type": "localImage", "path": "/" + strings.Repeat("a", 4096)}, "image path is longer than 4096 bytes"},
	} {
		c.fails("turn/start", map[string]any{"threadId": threadID, "input": []map[string]string{tc.item}}, codeInvalidParams, tc.text)
	}
}

func TestTurnSteerAcceptsImage(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	gate := make(chan struct{})
	r.llm.push(llmReply{body: sseText("slow"), gate: gate})
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	turnID := startTurn(t, c, threadID, "wait")
	path := writeImage(t, "shot.png", pngSample)
	var steer TurnSteerResponse
	input := []map[string]string{{"type": "localImage", "path": path}}
	c.ok("turn/steer", map[string]any{"threadId": threadID, "expectedTurnId": turnID, "input": input}, &steer)
	close(gate)
	c.collect("turn/completed")
	messages := userMessages(t, r, threadID)
	last := messages[len(messages)-1]
	if string(last.Kind) != "steer" || len(last.Images) != 1 || last.Images[0].Blob.Bytes != int64(len(pngSample)) {
		t.Fatalf("steer message %+v, want kind steer with one %d-byte image", last, len(pngSample))
	}
}

func TestImagePathRejectsFIFO(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("a FIFO needs mkfifo, which Windows lacks")
	}
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	fifo := filepath.Join(t.TempDir(), "pipe.png")
	if out, err := exec.Command("mkfifo", fifo).CombinedOutput(); err != nil {
		t.Skipf("mkfifo: %v: %s", err, out)
	}
	input := []map[string]string{{"type": "localImage", "path": fifo}}
	reply, ok := callWithin(c, "turn/start", map[string]any{"threadId": threadID, "input": input}, time.Second)
	if !ok {
		t.Fatal("turn/start with a FIFO did not return within one second")
	}
	if reply.Error == nil || reply.Error.Code != codeInvalidParams || reply.Error.Message != "image path must be a regular file: "+fifo {
		t.Fatalf("turn/start with a FIFO returned %+v, want -32602 image path must be a regular file", reply.Error)
	}
}

func callWithin(c *wireClient, method string, params any, limit time.Duration) (message, bool) {
	replies := make(chan message, 1)
	go func() { replies <- c.call(method, params) }()
	select {
	case reply := <-replies:
		return reply, true
	case <-time.After(limit):
		return message{}, false
	}
}

func TestImageCountRefusedBeforeReading(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("a FIFO needs mkfifo, which Windows lacks")
	}
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	fifo := filepath.Join(t.TempDir(), "pipe.png")
	if out, err := exec.Command("mkfifo", fifo).CombinedOutput(); err != nil {
		t.Skipf("mkfifo: %v: %s", err, out)
	}
	var input []map[string]string
	for index := range event.MaxImages {
		input = append(input, map[string]string{"type": "localImage", "path": writeImage(t, fmt.Sprintf("shot%02d.png", index), pngSample)})
	}
	input = append(input, map[string]string{"type": "localImage", "path": fifo})
	reply, ok := callWithin(c, "turn/start", map[string]any{"threadId": threadID, "input": input}, 5*time.Second)
	if !ok {
		t.Fatal("turn/start with an 11th image did not return: the server opened the file before it refused the count")
	}
	if reply.Error == nil || reply.Error.Code != codeInvalidParams || reply.Error.Message != "a request takes at most 10 images, got 11" {
		t.Fatalf("turn/start with 11 images returned %+v, want -32602 a request takes at most 10 images, got 11", reply.Error)
	}
}

func TestImageTotalRefusedBeforeReading(t *testing.T) {
	if runtime.GOOS == "windows" || os.Geteuid() == 0 {
		t.Skip("the unreadable file needs a non-root Unix user")
	}
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	big := pngSample + strings.Repeat("a", 4<<20)
	first := writeImage(t, "first.png", big)
	second := writeImage(t, "second.png", big)
	third := writeImage(t, "third.png", big)
	if err := os.Chmod(third, 0); err != nil {
		t.Fatal(err)
	}
	input := []map[string]string{{"type": "localImage", "path": first}, {"type": "localImage", "path": second}, {"type": "localImage", "path": third}}
	c.fails("turn/start", map[string]any{"threadId": threadID, "input": input}, codeInvalidParams, fmt.Sprintf("images total %d bytes; the limit is 10485760 bytes (10 MiB) per request", 3*len(big)))
}

func TestImageSizeNamesItsIndex(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	small := writeImage(t, "small.png", pngSample)
	large := writeImage(t, "large.png", "")
	if err := os.Truncate(large, 5<<20+1); err != nil {
		t.Fatal(err)
	}
	input := []map[string]string{{"type": "localImage", "path": small}, {"type": "localImage", "path": large}}
	c.fails("turn/start", map[string]any{"threadId": threadID, "input": input}, codeInvalidParams, "image 2 is 5242881 bytes; the limit is 5242880 bytes (5 MiB)")
}
