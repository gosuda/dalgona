package rpcserver

import (
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	json "encoding/json/v2"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
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
	var response rpc.TurnStartResponse
	c.ok("turn/start", map[string]any{"threadId": threadID, "input": input}, &response)
	c.collect()
	messages := userMessages(t, r, threadID)
	if len(messages) != 1 || len(messages[0].Images) != 1 {
		t.Fatalf("user messages %+v, want one message with one image", messages)
	}
	image := messages[0].Images[0]
	sum := sha256.Sum256([]byte(pngSample))
	if want := hex.EncodeToString(sum[:]); image.Blob.Blob != want {
		t.Fatalf("image blob = %q, want %q", image.Blob.Blob, want)
	}
	if image.Mime != "image/png" || image.Blob.Bytes != int64(len(pngSample)) || image.Path != path {
		t.Fatalf("image %+v, want image/png, %d bytes, path %s", image, len(pngSample), path)
	}
}

func TestTurnStartImageDataURL(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	input := []map[string]string{{"type": "image", "url": "data:image/png;base64,iVBORw0KGgo=", "detail": "high"}}
	var response rpc.TurnStartResponse
	c.ok("turn/start", map[string]any{"threadId": threadID, "input": input}, &response)
	c.collect()
	messages := userMessages(t, r, threadID)
	if len(messages) != 1 || len(messages[0].Images) != 1 {
		t.Fatalf("user messages %+v, want one message with one image", messages)
	}
	payload, err := base64.StdEncoding.DecodeString("iVBORw0KGgo=")
	if err != nil {
		t.Fatal(err)
	}
	image := messages[0].Images[0]
	sum := sha256.Sum256(payload)
	if want := hex.EncodeToString(sum[:]); image.Blob.Blob != want {
		t.Fatalf("image blob = %q, want %q", image.Blob.Blob, want)
	}
	if image.Blob.Bytes != int64(len(payload)) {
		t.Fatalf("image bytes = %d, want %d", image.Blob.Bytes, len(payload))
	}
}

func requirePreservedTextEntry(t *testing.T, content []map[string]any) {
	t.Helper()
	if len(content) != 3 || fmt.Sprint(content[0]["text"]) != "what is this" {
		t.Fatalf("user content = %+v, want text plus two images", content)
	}
}

func requireJournalNameAndAlt(t *testing.T, messages []event.UserMessage, path string) {
	t.Helper()
	if len(messages) != 1 || len(messages[0].Images) != 2 {
		t.Fatalf("user messages %+v, want one message with two images", messages)
	}
	if got := messages[0].Images[0]; got.Name != "shot.png" || got.AltText != "Map of the work area" || got.Path != path {
		t.Fatalf("journal image 0 = %+v, want name shot.png and alt Map of the work area", got)
	}
	if got := messages[0].Images[1]; got.Name != "pasted.png" || got.AltText != "Pasted diagram" {
		t.Fatalf("journal image 1 = %+v, want name pasted.png and alt Pasted diagram", got)
	}
}

func requireWireNameAndAlt(t *testing.T, content []map[string]any) {
	t.Helper()
	first, second := content[1], content[2]
	if fmt.Sprint(first["name"]) != "shot.png" || fmt.Sprint(first["altText"]) != "Map of the work area" {
		t.Fatalf("wire image 0 = %+v, want name and altText to survive to wire", first)
	}
	if fmt.Sprint(second["name"]) != "pasted.png" || fmt.Sprint(second["altText"]) != "Pasted diagram" {
		t.Fatalf("wire image 1 = %+v, want name and alt alias to survive to wire", second)
	}
}

func requireResumedNameAndAlt(t *testing.T, contents [][]map[string]any) {
	t.Helper()
	if len(contents) != 1 || len(contents[0]) != 3 {
		t.Fatalf("resumed contents = %+v, want one item with text plus two images", contents)
	}
	resumed := contents[0][1]
	if fmt.Sprint(resumed["name"]) != "shot.png" || fmt.Sprint(resumed["altText"]) != "Map of the work area" {
		t.Fatalf("resumed image = %+v, want name and altText after recovery", resumed)
	}
}

func TestTurnStartImagePreservesNameAndAlt(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	path := writeImage(t, "shot.png", pngSample)
	input := []map[string]string{
		{"type": "text", "text": "what is this"},
		{"type": "localImage", "path": path, "name": "shot.png", "altText": "Map of the work area"},
		{"type": "image", "url": "data:image/png;base64," + dataURLPNG, "name": "pasted.png", "alt": "Pasted diagram"},
	}
	var response rpc.TurnStartResponse
	c.ok("turn/start", map[string]any{"threadId": threadID, "input": input}, &response)
	content := userItemContent(t, c.collect())
	requirePreservedTextEntry(t, content)
	requireJournalNameAndAlt(t, userMessages(t, r, threadID), path)
	requireWireNameAndAlt(t, content)
	contents, _ := resumedUserContents(t, r, threadID)
	requireResumedNameAndAlt(t, contents)
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
		{"long path", map[string]string{"type": "localImage", "path": filepath.Join(dir, strings.Repeat("a", 4096))}, "image path is longer than 4096 bytes"},
	} {
		c.fails("turn/start", map[string]any{"threadId": threadID, "input": []map[string]string{tc.item}}, rpc.CodeInvalidParams, tc.text)
		if messages := userMessages(t, r, threadID); len(messages) != 0 {
			t.Fatalf("%s appended %d user messages, want 0", tc.name, len(messages))
		}
	}
}

func TestImageReadFailure(t *testing.T) {
	if runtime.GOOS == "windows" || os.Geteuid() == 0 {
		t.Skip("the unreadable file needs a non-root Unix user")
	}
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	path := writeImage(t, "shot.png", pngSample)
	if err := os.Chmod(path, 0); err != nil {
		t.Fatal(err)
	}
	input := []map[string]string{{"type": "localImage", "path": path}}
	reply := c.call("turn/start", map[string]any{"threadId": threadID, "input": input})
	prefix := "cannot read image " + path + ": "
	if reply.Error == nil || reply.Error.Code != rpc.CodeInternal || !strings.HasPrefix(reply.Error.Message, prefix) {
		t.Fatalf("turn/start with an unreadable image returned %+v, want -32603 %q", reply.Error, prefix)
	}
	if messages := userMessages(t, r, threadID); len(messages) != 0 {
		t.Fatalf("rejected turn/start appended %d user messages, want 0", len(messages))
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
	var steer rpc.TurnSteerResponse
	input := []map[string]string{{"type": "localImage", "path": path}}
	c.ok("turn/steer", map[string]any{"threadId": threadID, "expectedTurnId": turnID, "input": input}, &steer)
	close(gate)
	c.collect()
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
	if reply.Error == nil || reply.Error.Code != rpc.CodeInvalidParams || reply.Error.Message != "image path must be a regular file: "+fifo {
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
	if reply.Error == nil || reply.Error.Code != rpc.CodeInvalidParams || reply.Error.Message != "a request takes at most 10 images, got 11" {
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
	c.fails("turn/start", map[string]any{"threadId": threadID, "input": input}, rpc.CodeInvalidParams, fmt.Sprintf("images total %d bytes; the limit is 10485760 bytes (10 MiB) per request", 3*len(big)))
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
	c.fails("turn/start", map[string]any{"threadId": threadID, "input": input}, rpc.CodeInvalidParams, "image 2 is 5242881 bytes; the limit is 5242880 bytes (5 MiB)")
}

func userItemContent(t *testing.T, notes []message) []map[string]any {
	t.Helper()
	for _, m := range notes {
		if m.Method != "item/completed" || fieldOf(t, m.Params, "item", "type") != rpc.KindUser {
			continue
		}
		var envelope struct {
			Item struct {
				Content []map[string]any `json:"content"`
			} `json:"item"`
		}
		if err := json.Unmarshal(m.Params, &envelope); err != nil {
			t.Fatal(err)
		}
		return envelope.Item.Content
	}
	t.Fatal("no userMessage item/completed")
	return nil
}

func userItemImage(t *testing.T, content []map[string]any) map[string]any {
	t.Helper()
	for _, entry := range content {
		if fmt.Sprint(entry["type"]) == "image" {
			return entry
		}
	}
	t.Fatalf("user content %+v carries no image", content)
	return nil
}

const dataURLPNG = "iVBORw0KGgo="

func dataURLPayload(t *testing.T) []byte {
	t.Helper()
	payload, err := base64.StdEncoding.DecodeString(dataURLPNG)
	if err != nil {
		t.Fatal(err)
	}
	return payload
}

func requireBlobImage(t *testing.T, image map[string]any, payload []byte, path string) {
	t.Helper()
	digest := sha256.Sum256(payload)
	want := map[string]string{"type": "image", "url": "dalgona-blob:" + hex.EncodeToString(digest[:]), "mime": "image/png"}
	if path != "" {
		want["path"] = path
	}
	for key, value := range want {
		if got, ok := image[key].(string); !ok || got != value {
			t.Fatalf("image %s = %v, want %s", key, image[key], value)
		}
	}
	if size, ok := image["bytes"].(float64); !ok || size != float64(len(payload)) {
		t.Fatalf("image bytes = %v, want %d", image["bytes"], len(payload))
	}
	if _, has := image["path"]; has != (path != "") || len(image) != len(want)+1 {
		t.Fatalf("image = %+v, want exactly the members %v and bytes", image, want)
	}
}

func resumedUserContents(t *testing.T, r *rig, threadID string) ([][]map[string]any, int) {
	t.Helper()
	reply := r.connect().call("thread/resume", map[string]any{"threadId": threadID})
	if reply.Error != nil {
		t.Fatalf("thread/resume failed: %d %s", reply.Error.Code, reply.Error.Message)
	}
	var resumed struct {
		Thread struct {
			Turns []struct {
				Items []struct {
					Type    string           `json:"type"`
					Content []map[string]any `json:"content"`
				} `json:"items"`
			} `json:"turns"`
		} `json:"thread"`
	}
	if err := json.Unmarshal(reply.Result, &resumed); err != nil {
		t.Fatalf("decode thread/resume result: %v", err)
	}
	var contents [][]map[string]any
	for _, turn := range resumed.Thread.Turns {
		for _, item := range turn.Items {
			if item.Type == rpc.KindUser {
				contents = append(contents, item.Content)
			}
		}
	}
	return contents, len(reply.Result)
}

func TestUserMessageItemEchoesImage(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	path := writeImage(t, "shot.png", pngSample)
	requireLocalImageEchoed(t, c, threadID, path)
	requireDataURLEchoedWithoutPath(t, c, threadID)
	requireTextOnlyEchoed(t, c, threadID)
	requireResumeEchoesSameItems(t, r, threadID, path)
}

func TestResumedUserItemCarriesNoImageBytes(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	payload := []byte(pngSample + strings.Repeat("a", 1<<20))
	path := writeImage(t, "large.png", string(payload))
	var response rpc.TurnStartResponse
	c.ok("turn/start", map[string]any{"threadId": threadID, "input": []map[string]any{{"type": "localImage", "path": path}}}, &response)
	c.collect()
	contents, size := resumedUserContents(t, r, threadID)
	if len(contents) != 1 {
		t.Fatalf("resumed user items = %d, want 1", len(contents))
	}
	requireBlobImage(t, userItemImage(t, contents[0]), payload, path)
	if size > 64<<10 {
		t.Fatalf("thread/resume result is %d bytes for a %d-byte image, want a metadata-only echo under 64 KiB", size, len(payload))
	}
}

func requireLocalImageEchoed(t *testing.T, c *wireClient, threadID, path string) {
	t.Helper()
	input := []map[string]any{{"type": "text", "text": "what is this"}, {"type": "localImage", "path": path}}
	var response rpc.TurnStartResponse
	c.ok("turn/start", map[string]any{"threadId": threadID, "input": input}, &response)
	content := userItemContent(t, c.collect())
	if len(content) != 2 || fmt.Sprint(content[0]["text"]) != "what is this" {
		t.Fatalf("user content = %+v, want the text and one image", content)
	}
	requireBlobImage(t, content[1], []byte(pngSample), path)
}

func requireDataURLEchoedWithoutPath(t *testing.T, c *wireClient, threadID string) {
	t.Helper()
	var response rpc.TurnStartResponse
	c.ok("turn/start", map[string]any{"threadId": threadID, "input": []map[string]any{{"type": "image", "url": "data:image/png;base64," + dataURLPNG}}}, &response)
	requireBlobImage(t, userItemImage(t, userItemContent(t, c.collect())), dataURLPayload(t), "")
}

func requireTextOnlyEchoed(t *testing.T, c *wireClient, threadID string) {
	t.Helper()
	var response rpc.TurnStartResponse
	c.ok("turn/start", map[string]any{"threadId": threadID, "input": []map[string]any{{"type": "text", "text": "plain"}}}, &response)
	content := userItemContent(t, c.collect())
	if len(content) != 1 || fmt.Sprint(content[0]["text"]) != "plain" {
		t.Fatalf("text-only user content = %+v, want one text entry", content)
	}
}

func requireResumeEchoesSameItems(t *testing.T, r *rig, threadID, path string) {
	t.Helper()
	contents, _ := resumedUserContents(t, r, threadID)
	if len(contents) != 3 || len(contents[0]) != 2 || len(contents[2]) != 1 {
		t.Fatalf("resumed user contents = %+v, want a text and image, an image item, and a text-only item", contents)
	}
	if fmt.Sprint(contents[0][0]["text"]) != "what is this" || fmt.Sprint(contents[2][0]["text"]) != "plain" {
		t.Fatalf("resumed user texts = %+v, want the submitted texts", contents)
	}
	requireBlobImage(t, contents[0][1], []byte(pngSample), path)
	requireBlobImage(t, userItemImage(t, contents[1]), dataURLPayload(t), "")
}
