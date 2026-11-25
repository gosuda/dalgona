package rpcserver

import (
	"bytes"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"strconv"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/config"
)

type imageReadResult struct {
	MimeType   string `json:"mimeType"`
	Digest     string `json:"digest"`
	Total      string `json:"total"`
	Offset     string `json:"offset"`
	Next       string `json:"next"`
	DataBase64 string `json:"dataBase64"`
}

type imageItem struct {
	threadID string
	turnID   string
	itemID   string
}

func sendImageTurn(t *testing.T, r *rig, c *wireClient, threadID string, payload []byte) imageItem {
	t.Helper()
	path := writeImage(t, "shot.png", string(payload))
	input := []map[string]string{{"type": "text", "text": "look"}, {"type": "localImage", "path": path}}
	c.ok("turn/start", map[string]any{"threadId": threadID, "input": input}, nil)
	c.collect()
	messages := userMessages(t, r, threadID)
	last := messages[len(messages)-1]
	return imageItem{threadID: threadID, turnID: last.TurnID, itemID: last.ItemID}
}

func (it imageItem) params(extra map[string]any) map[string]any {
	params := map[string]any{"threadId": it.threadID, "turnId": it.turnID, "itemId": it.itemID, "imageIndex": 0}
	for key, value := range extra {
		params[key] = value
	}
	return params
}

func readImageChunk(t *testing.T, c *wireClient, params map[string]any) ([]byte, imageReadResult) {
	t.Helper()
	var result imageReadResult
	c.ok("dalgona/item/image/read", params, &result)
	data, err := base64.StdEncoding.DecodeString(result.DataBase64)
	if err != nil {
		t.Fatalf("decode image chunk: %v", err)
	}
	return data, result
}

func pngPayload(size int, fill byte) []byte {
	return append([]byte(pngSample), bytes.Repeat([]byte{fill}, size-len(pngSample))...)
}

func TestItemImageReadRefusesAnotherThreadsItem(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	mine := sendImageTurn(t, r, c, startThread(t, r, c, nil), pngPayload(64, 'a'))
	theirs := sendImageTurn(t, r, c, startThread(t, r, c, nil), pngPayload(64, 'b'))

	data, result := readImageChunk(t, c, mine.params(nil))
	sum := sha256.Sum256(pngPayload(64, 'a'))
	if !bytes.Equal(data, pngPayload(64, 'a')) || result.Digest != hex.EncodeToString(sum[:]) || result.MimeType != "image/png" {
		t.Fatalf("own image read = %+v, data %q", result, data)
	}
	if result.Total != "64" || result.Offset != "0" || result.Next != "64" {
		t.Fatalf("own image range = %+v, want total 64 from 0 to 64", result)
	}

	foreign := []map[string]any{
		{"threadId": mine.threadID, "turnId": theirs.turnID, "itemId": theirs.itemID, "imageIndex": 0},
		{"threadId": mine.threadID, "turnId": mine.turnID, "itemId": theirs.itemID, "imageIndex": 0},
		{"threadId": mine.threadID, "turnId": theirs.turnID, "itemId": mine.itemID, "imageIndex": 0},
		mine.params(map[string]any{"imageIndex": 1}),
		{"threadId": "no-such-thread", "turnId": mine.turnID, "itemId": mine.itemID, "imageIndex": 0},
	}
	for _, params := range foreign {
		c.fails("dalgona/item/image/read", params, -32020, "not_found")
	}
}

func TestItemImageReadBoundsEachPage(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	const size = 1<<20 + 300<<10
	payload := pngPayload(size, 'z')
	item := sendImageTurn(t, r, c, startThread(t, r, c, nil), payload)
	pages := readImagePages(t, c, item, size)
	if !bytes.Equal(bytes.Join(pages, nil), payload) {
		t.Fatalf("pages do not rebuild the image of %d bytes", size)
	}
	checkRefusedImageReads(t, c, item, size)
}

func readImagePages(t *testing.T, c *wireClient, item imageItem, size int) [][]byte {
	t.Helper()
	first, result := readImageChunk(t, c, item.params(nil))
	if len(first) != 256<<10 || result.Next != strconv.Itoa(256<<10) || result.Total != strconv.Itoa(size) {
		t.Fatalf("default page = %d bytes, %+v; want 262144 bytes of %d", len(first), result, size)
	}
	rest, result := readImageChunk(t, c, item.params(map[string]any{"offset": result.Next, "limit": 1 << 20}))
	if len(rest) != 1<<20 || result.Offset != strconv.Itoa(256<<10) || result.Next != strconv.Itoa(256<<10+1<<20) {
		t.Fatalf("maximum page = %d bytes, %+v; want 1048576 bytes", len(rest), result)
	}
	tail, result := readImageChunk(t, c, item.params(map[string]any{"offset": result.Next, "limit": 1 << 20}))
	if result.Next != result.Total {
		t.Fatalf("tail %d bytes does not reach the total %+v", len(tail), result)
	}
	return [][]byte{first, rest, tail}
}

func checkRefusedImageReads(t *testing.T, c *wireClient, item imageItem, size int) {
	t.Helper()
	refused := []map[string]any{
		item.params(map[string]any{"limit": 1<<20 + 1}),
		item.params(map[string]any{"limit": 0}),
		item.params(map[string]any{"offset": strconv.Itoa(size + 1)}),
		item.params(map[string]any{"offset": "01"}),
		{"threadId": item.threadID, "turnId": item.turnID, "itemId": item.itemID},
	}
	for _, params := range refused {
		c.fails("dalgona/item/image/read", params, -32602, "invalid_params")
	}
}
