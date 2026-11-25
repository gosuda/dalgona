package rpcserver

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/config"
)

func sha256Hex(data string) string {
	sum := sha256.Sum256([]byte(data))
	return hex.EncodeToString(sum[:])
}

func pageItemHeader(t *testing.T, c *nativeClient, threadID, itemType string) rpc.ItemHeader {
	t.Helper()
	var snap rpc.HistorySnapshot
	c.ok("dalgona/history/open", map[string]string{"threadId": threadID}, &snap)
	var page rpc.HistoryPage
	c.ok("dalgona/history/page", map[string]string{"historyId": snap.HistoryID}, &page)
	for _, turn := range page.Turns {
		for _, header := range turn.Items {
			if header.Type == itemType {
				return header
			}
		}
	}
	t.Fatalf("history page has no %s item", itemType)
	return rpc.ItemHeader{}
}

func TestNativeHeadersKeepJournalBlobDescriptors(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connectNative(ServeOptions{})
	threadID := c.startThread(r)
	reply := strings.Repeat("blob text ", 8000)
	r.llm.push(llmReply{body: sseText(reply)})
	c.ok("turn/start", prompt(threadID, "hi"), nil)
	completed := c.awaitEvent("item/completed", func(event rpc.Event) bool { return eventItem(t, event).Type == rpc.KindAgent })
	live, ok := contentField(eventItem(t, completed), "text")
	if !ok || live.Total != strconv.Itoa(len(reply)) || live.Digest != sha256Hex(reply) {
		t.Fatalf("live text reference = %+v, want %d bytes with the reply digest", live, len(reply))
	}
	if got := c.readContent(live); string(got) != reply {
		t.Fatalf("live text read %d bytes, want the %d byte reply", len(got), len(reply))
	}
	c.awaitEvent("turn/completed", nil)
	if err := os.Remove(filepath.Join(r.home, "blobs", sha256Hex(reply))); err != nil {
		t.Fatal(err)
	}
	snapshot, ok := contentField(pageItemHeader(t, c, threadID, rpc.KindAgent), "text")
	if !ok || snapshot.Total != live.Total || snapshot.Digest != live.Digest {
		t.Fatalf("snapshot text reference = %+v after the blob vanished, want the journal descriptor %s/%s", snapshot, live.Total, live.Digest)
	}
	_ = c.failsWith("dalgona/history/content/read", map[string]any{"source": snapshot.Source, "contentId": snapshot.ContentID}, rpc.ErrorUnavailable)
}

func TestNativeOutputSpillIsOneCompleteReference(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("needs POSIX printf")
	}
	r := newRig(t, config.ApprovalYolo)
	c := r.connectNative(ServeOptions{})
	threadID := c.startThread(r)
	r.llm.push(llmReply{body: sseCall("call-1", "exec", `{"command":"printf '%070000d' 0"}`)}, llmReply{body: sseText("done")})
	c.ok("turn/start", prompt(threadID, "print it"), nil)
	completed := c.awaitEvent("item/completed", func(event rpc.Event) bool { return eventItem(t, event).ID == "call-1" })
	c.awaitEvent("turn/completed", nil)
	want := strings.Repeat("0", 70000)
	for source, header := range map[string]rpc.ItemHeader{
		"live":     eventItem(t, completed),
		"snapshot": pageItemHeader(t, c, threadID, rpc.KindCommand),
	} {
		if _, exposed := header.Metadata["outputSpill"]; exposed {
			t.Fatalf("%s header exposes the spill descriptor %s", source, header.Metadata["outputSpill"])
		}
		ref, ok := contentField(header, "outputSpill")
		if !ok || ref.Total != "70000" || ref.Encoding != encodingUTF8 {
			t.Fatalf("%s outputSpill reference = %+v, want the complete 70000 byte output", source, ref)
		}
		if got := c.readContent(ref); string(got) != want {
			t.Fatalf("%s spill read %d bytes, want 70000 zero characters", source, len(got))
		}
	}
}

func TestRunnerSpillDescribesTheMaskedBytesItServes(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("needs POSIX printf")
	}
	probe := "spl-probe-8Rw3Kn5Yq2"
	t.Setenv("SPILL_PROBE_TOKEN", probe)
	r := newRig(t, config.ApprovalYolo)
	c := r.connectNative(ServeOptions{})
	threadID := c.startThread(r)
	command := `printf '%070000d' 0; printf 'key=%s\n' "$SPILL_PROBE_TOKEN"`
	r.llm.push(llmReply{body: sseCall("call-1", "exec", `{"command":`+strconv.Quote(command)+`}`)}, llmReply{body: sseText("done")})
	c.ok("turn/start", prompt(threadID, "print it"), nil)
	completed := c.awaitEvent("item/completed", func(event rpc.Event) bool { return eventItem(t, event).ID == "call-1" })
	c.awaitEvent("turn/completed", nil)
	want := strings.Repeat("0", 70000) + "key={{secret:SPILL_PROBE_TOKEN}}\n"
	checkSpillReference(t, c, completed, want)
	checkRunnerOutputPages(t, r, threadID, want)
}

func checkSpillReference(t *testing.T, c *nativeClient, completed rpc.Event, want string) {
	t.Helper()
	ref, ok := contentField(eventItem(t, completed), "outputSpill")
	if !ok || ref.Total != strconv.Itoa(len(want)) || ref.Digest != sha256Hex(want) {
		t.Fatalf("outputSpill reference = %+v, want %d masked bytes with their digest", ref, len(want))
	}
	if got := c.readContent(ref); string(got) != want {
		t.Fatalf("spill read %q..., want the masked output", got[max(len(got)-40, 0):])
	}
}

func checkRunnerOutputPages(t *testing.T, r *rig, threadID, want string) {
	t.Helper()
	thread, err := r.host.Resume(t.Context(), threadID)
	if err != nil {
		t.Fatal(err)
	}
	var served strings.Builder
	for offset := int64(0); offset < int64(len(want)); {
		page, err := thread.ReadOutput(t.Context(), "call-1", offset)
		if err != nil || page.Total != int64(len(want)) {
			t.Fatalf("ReadOutput at %d = total %d, %v; want total %d", offset, page.Total, err, len(want))
		}
		served.WriteString(page.Text)
		offset = page.Next
	}
	if served.String() != want {
		t.Fatal("ReadOutput pages differ from the bytes the spill reference describes")
	}
}

func TestOversizedEventParamsSpillAsMetadataAndNestedReferences(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	diff := strings.Repeat("d", 300<<10)
	var threadID string
	emit := func(ctx context.Context, _ jsontext.Value) (jsontext.Value, error) {
		params, err := json.Marshal(map[string]string{"threadId": threadID, "label": "small", "diff": diff})
		if err != nil {
			return nil, err
		}
		return jsontext.Value(`{}`), Notify(ctx, "test/large", params)
	}
	c := r.connectNative(ServeOptions{Extensions: map[string]Extension{"test/emit": emit}})
	threadID = c.startThread(r)
	c.ok("test/emit", map[string]string{}, nil)
	event := c.awaitEvent("test/large", nil)
	body := checkSpilledParamsBody(t, c, event, threadID)
	checkNestedDiffRef(t, c, body, diff)
}

type spilledParamsBody struct {
	Metadata map[string]string      `json:"metadata"`
	Contents []rpc.ContentReference `json:"contents"`
}

func checkSpilledParamsBody(t *testing.T, c *nativeClient, event rpc.Event, threadID string) spilledParamsBody {
	t.Helper()
	if event.ParamsContent == nil || len(event.Params) != 0 {
		t.Fatalf("oversized event = params %d bytes, paramsContent %+v", len(event.Params), event.ParamsContent)
	}
	var body spilledParamsBody
	if err := json.Unmarshal(c.readContent(*event.ParamsContent), &body); err != nil {
		t.Fatalf("decode spilled params body: %v", err)
	}
	if body.Metadata["threadId"] != threadID || body.Metadata["label"] != "small" || len(body.Metadata) != 2 {
		t.Fatalf("spilled metadata = %v, want only threadId and label", body.Metadata)
	}
	return body
}

func checkNestedDiffRef(t *testing.T, c *nativeClient, body spilledParamsBody, diff string) {
	t.Helper()
	if len(body.Contents) != 1 || body.Contents[0].Field != "diff" || body.Contents[0].Digest != sha256Hex(diff) {
		t.Fatalf("spilled contents = %+v, want one diff reference", body.Contents)
	}
	if got := c.readContent(body.Contents[0]); string(got) != diff {
		t.Fatalf("nested diff read %d bytes, want %d", len(got), len(diff))
	}
}

func TestLiveReferencesBindTheirEventOrdinal(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	s, c := manualNativeServer(t, r)
	threadID := c.startThread(r)
	r.llm.push(llmReply{body: sseText("ordinal bound")})
	c.ok("turn/start", prompt(threadID, "hi"), nil)
	completed := c.awaitEvent("item/completed", func(event rpc.Event) bool { return eventItem(t, event).Type == rpc.KindAgent })
	ref, ok := contentField(eventItem(t, completed), "text")
	if !ok {
		t.Fatal("agent item has no text reference")
	}
	c.awaitEvent("turn/completed", nil)
	got, err := s.native.Load().contents.lookup(ref.ContentID, time.Now())
	if err != nil || got.ordinal != completed.First {
		t.Fatalf("live reference ordinal = %d (%v), want the event ordinal %d", got.ordinal, err, completed.First)
	}
}
