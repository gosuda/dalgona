package rpc

import (
	"bufio"
	"context"
	"encoding/hex"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"net"
	"os"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

type textCorpus struct {
	Vectors []struct {
		ID   string `json:"id"`
		Text string `json:"text"`
	} `json:"vectors"`
	InvalidEncodings []struct {
		ID       string `json:"id"`
		Encoding string `json:"encoding"`
		Input    string `json:"input"`
	} `json:"invalidEncodings"`
}

func loadTextCorpus(t *testing.T) textCorpus {
	t.Helper()
	data, err := os.ReadFile("../../../docs/dal/spec/input-text-cases.json")
	if err != nil {
		t.Fatal(err)
	}
	var c textCorpus
	if err := json.Unmarshal(data, &c); err != nil {
		t.Fatal(err)
	}
	return c
}

func malformedFrames(t *testing.T, c textCorpus) map[string][]byte {
	t.Helper()
	frames := map[string][]byte{"leading_bom": []byte("\xef\xbb\xbf" + `{"id":1,"method":"probe","params":{"text":"a"}}`)}
	for _, e := range c.InvalidEncodings {
		var text []byte
		switch e.Encoding {
		case "utf8_hex":
			raw, err := hex.DecodeString(strings.ReplaceAll(e.Input, " ", ""))
			if err != nil {
				t.Fatal(err)
			}
			text = append(append([]byte(`"`), raw...), '"')
		case "json_text":
			text = []byte(e.Input)
		default:
			continue
		}
		frame := append([]byte(`{"id":1,"method":"probe","params":{"text":`), text...)
		frames[e.ID] = append(frame, "}}"...)
	}
	return frames
}

func TestMalformedTextFailsAsParseErrorBeforeDispatch(t *testing.T) {
	frames := malformedFrames(t, loadTextCorpus(t))
	if len(frames) < 8 {
		t.Fatalf("built %d malformed frames, want every UTF-8 and JSON vector", len(frames))
	}
	for id, frame := range frames {
		var dispatched atomic.Bool
		left, right := net.Pipe()
		c := NewConn(t.Context(), left, func(context.Context, string, jsontext.Value) (jsontext.Value, error) {
			dispatched.Store(true)
			return jsontext.Value("{}"), nil
		})
		closeRPCPair(t, c, right)
		if _, err := right.Write(append(frame, '\n')); err != nil {
			t.Fatal(err)
		}
		var got struct {
			Error *wireError `json:"error"`
		}
		if err := json.Unmarshal(readReply(t, right), &got); err != nil {
			t.Fatal(err)
		}
		if got.Error == nil || got.Error.Code != codeParseError || dispatched.Load() {
			t.Errorf("%s: reply %+v, dispatched %v; want a parse error and no dispatch", id, got.Error, dispatched.Load())
		}
	}
}

func TestTextSplitAcrossReadsArrivesOnce(t *testing.T) {
	for _, v := range loadTextCorpus(t).Vectors {
		got := make(chan string, 1)
		left, right := net.Pipe()
		c := NewConn(t.Context(), left, func(_ context.Context, _ string, params jsontext.Value) (jsontext.Value, error) {
			var p struct {
				Text string `json:"text"`
			}
			err := json.Unmarshal(params, &p)
			got <- p.Text
			return jsontext.Value("{}"), err
		})
		closeRPCPair(t, c, right)
		frame, err := json.Marshal(map[string]any{"id": 1, "method": "probe", "params": map[string]string{"text": v.Text}})
		if err != nil {
			t.Fatal(err)
		}
		go writeByteByByte(right, append(frame, '\n'))
		go func() { _, _ = bufio.NewReader(right).ReadBytes('\n') }()
		select {
		case text := <-got:
			if text != v.Text {
				t.Errorf("%s: handler got %q, want %q", v.ID, text, v.Text)
			}
		case <-time.After(5 * time.Second):
			t.Fatalf("%s: no dispatch", v.ID)
		}
	}
}

func writeByteByByte(w net.Conn, frame []byte) {
	for i := range frame {
		if _, err := w.Write(frame[i : i+1]); err != nil {
			return
		}
	}
}
