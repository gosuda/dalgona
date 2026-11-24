package rpc

import (
	"bufio"
	"encoding/json/jsontext"
	"errors"
	"net"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

func startCheckedCall(t *testing.T, check ResponseCheck) (net.Conn, <-chan error, *string) {
	t.Helper()
	left, right := net.Pipe()
	c := NewConn(t.Context(), left, nil)
	closeRPCPair(t, c, right)
	done := make(chan error, 1)
	var got struct {
		Answer string `json:"answer"`
	}
	go func() {
		done <- c.CallIDChecked(t.Context(), "question-1", CheckedCall{Method: "item/tool/requestUserInput", Params: map[string]string{}, Check: check}, &got)
	}()
	if err := right.SetDeadline(time.Now().Add(5 * time.Second)); err != nil {
		t.Fatal(err)
	}
	if _, err := bufio.NewReader(right).ReadBytes('\n'); err != nil {
		t.Fatal(err)
	}
	return right, done, &got.Answer
}

func acceptOnlyGood(rejected *atomic.Int32) ResponseCheck {
	return func(result jsontext.Value) error {
		if !strings.Contains(string(result), "good") {
			rejected.Add(1)
			return errors.New("bad answer")
		}
		return nil
	}
}

func send(t *testing.T, right net.Conn, lines ...string) {
	t.Helper()
	if _, err := right.Write([]byte(strings.Join(lines, "\n") + "\n")); err != nil {
		t.Fatal(err)
	}
}

func awaitCall(t *testing.T, done <-chan error) error {
	t.Helper()
	select {
	case err := <-done:
		return err
	case <-time.After(5 * time.Second):
		t.Fatal("the call never ended")
		return nil
	}
}

func TestCheckedCallKeepsItsWaiterUntilTheCheckPasses(t *testing.T) {
	var rejected atomic.Int32
	right, done, answer := startCheckedCall(t, acceptOnlyGood(&rejected))
	send(t, right, `{"id":"question-1","result":{"answer":"bad"}}`)
	select {
	case err := <-done:
		t.Fatalf("the call ended on a rejected response: %v", err)
	case <-time.After(100 * time.Millisecond):
	}
	send(t, right, `{"id":"question-1","result":{"answer":"good"}}`, `{"id":"question-1","result":{"answer":"late"}}`)
	if err := awaitCall(t, done); err != nil || *answer != "good" || rejected.Load() != 1 {
		t.Fatalf("call = %v, answer %q, %d rejections; want the corrected answer after one rejection", err, *answer, rejected.Load())
	}
}

func TestCheckedCallPassesErrorResponsesThrough(t *testing.T) {
	var rejected atomic.Int32
	right, done, _ := startCheckedCall(t, acceptOnlyGood(&rejected))
	send(t, right, `{"id":"question-1","error":{"code":-32603,"message":"bad window"}}`)
	err := awaitCall(t, done)
	if rpcErr, ok := errors.AsType[*RPCError](err); !ok || rpcErr.Code != -32603 || rejected.Load() != 0 {
		t.Fatalf("call = %v, %d rejections; want the client error unchecked", err, rejected.Load())
	}
}
