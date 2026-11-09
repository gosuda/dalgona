package rpc

import (
	"bufio"
	json "encoding/json/v2"
	"net"
	"testing"
	"time"
)

func readReply(t *testing.T, right net.Conn) []byte {
	t.Helper()
	reply := make(chan []byte, 1)
	go func() {
		line, err := bufio.NewReader(right).ReadBytes('\n')
		if err == nil {
			reply <- line
		}
	}()
	select {
	case line := <-reply:
		return line
	case <-time.After(5 * time.Second):
		t.Fatal("no reply for malformed envelope")
		return nil
	}
}

func TestAuditWrongTypeGetsInvalidRequest(t *testing.T) {
	left, right := net.Pipe()
	c := NewConn(t.Context(), left, nil)
	defer c.Close()
	defer right.Close()
	if _, err := right.Write([]byte(`{"id":3,"method":53}` + "\n")); err != nil {
		t.Fatal(err)
	}
	line := readReply(t, right)
	var got struct {
		ID    int `json:"id"`
		Error *struct {
			Code int `json:"code"`
		} `json:"error"`
	}
	if err := json.Unmarshal(line, &got); err != nil {
		t.Fatal(err)
	}
	if got.ID != 3 || got.Error == nil || got.Error.Code != -32600 {
		t.Fatalf("reply=%s", line)
	}
}

func TestAuditInvalidJSONGetsParseError(t *testing.T) {
	left, right := net.Pipe()
	c := NewConn(t.Context(), left, nil)
	defer c.Close()
	defer right.Close()
	if _, err := right.Write([]byte(`{"id":3,"method":` + "\n")); err != nil {
		t.Fatal(err)
	}
	line := readReply(t, right)
	var got struct {
		ID    *int `json:"id"`
		Error *struct {
			Code int `json:"code"`
		} `json:"error"`
	}
	if err := json.Unmarshal(line, &got); err != nil {
		t.Fatal(err)
	}
	if got.Error == nil || got.Error.Code != -32700 {
		t.Fatalf("reply=%s", line)
	}
	if got.ID != nil {
		t.Fatalf("parse error id must be null, reply=%s", line)
	}
}
