package rpc

import (
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"net"
	"os"
	"path/filepath"
	"sync/atomic"
	"testing"
)

type jsonTextVector struct {
	ID           string `json:"id"`
	Encoding     string `json:"encoding"`
	Input        string `json:"input"`
	RPCErrorCode int    `json:"rpcErrorCode"`
}

func jsonTextVectors(t *testing.T) []jsonTextVector {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join("..", "..", "..", "docs", "dal", "spec", "input-text-cases.json"))
	if err != nil {
		t.Fatal(err)
	}
	var corpus struct {
		InvalidEncodings []jsonTextVector `json:"invalidEncodings"`
	}
	if err := json.Unmarshal(raw, &corpus); err != nil {
		t.Fatal(err)
	}
	var vectors []jsonTextVector
	for _, v := range corpus.InvalidEncodings {
		if v.Encoding == "json_text" && v.RPCErrorCode == -32700 {
			vectors = append(vectors, v)
		}
	}
	if len(vectors) != 2 {
		t.Fatalf("corpus holds %d json_text vectors, want 2", len(vectors))
	}
	return vectors
}

func TestMalformedTextGetsParseErrorBeforeDispatch(t *testing.T) {
	lines := map[string]string{"byte-order mark before the value": "\ufeff" + `{"jsonrpc":"2.0","id":8,"method":"turn/start","params":{}}`}
	for _, v := range jsonTextVectors(t) {
		lines[v.ID] = v.Input
		lines[v.ID+" inside a request"] = `{"jsonrpc":"2.0","id":7,"method":"turn/start","params":{"input":[{"type":"text","text":` + v.Input + `}]}}`
	}
	for name, line := range lines {
		t.Run(name, func(t *testing.T) {
			var dispatched atomic.Int32
			left, right := net.Pipe()
			c := NewConn(t.Context(), left, func(context.Context, string, jsontext.Value) (jsontext.Value, error) {
				dispatched.Add(1)
				return jsontext.Value(`{}`), nil
			})
			defer c.Close()
			defer right.Close()
			if _, err := right.Write([]byte(line + "\n")); err != nil {
				t.Fatal(err)
			}
			reply := readReply(t, right)
			var got struct {
				ID    jsontext.Value `json:"id"`
				Error *struct {
					Code int `json:"code"`
				} `json:"error"`
			}
			if err := json.Unmarshal(reply, &got); err != nil {
				t.Fatal(err)
			}
			if got.Error == nil || got.Error.Code != -32700 || string(got.ID) != "null" || dispatched.Load() != 0 {
				t.Fatalf("reply %s after %d dispatches, want a -32700 error with id null and no dispatch", reply, dispatched.Load())
			}
		})
	}
}
