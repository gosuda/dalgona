package rpcserver

import (
	"bufio"
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"io"
	"net"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func blockingMethod(started chan<- struct{}) method {
	return func(_ *server, ctx context.Context, _ jsontext.Value) (jsontext.Value, error) {
		started <- struct{}{}
		<-ctx.Done()
		return nil, ctx.Err()
	}
}

func TestDeadlineExpiryReturnsExactCancelledError(t *testing.T) {
	started := make(chan struct{}, 1)
	_, err := deadline(20*time.Millisecond, blockingMethod(started))(nil, t.Context(), nil)
	data, ok := rpc.ErrorDataOf(err)
	var coded *rpc.RPCError
	if !errors.As(err, &coded) {
		coded = nil
	}
	if !ok || coded == nil || coded.Code != -32019 || coded.Message != "cancelled" || data != (rpc.ErrorData{Kind: rpc.ErrorCancelled, Stage: deadlineStage}) {
		t.Fatalf("deadline error = %#v data %+v", err, data)
	}
	passthrough := deadline(20*time.Millisecond, func(*server, context.Context, jsontext.Value) (jsontext.Value, error) {
		time.Sleep(40 * time.Millisecond)
		return nil, rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorUnknownOutcome, OperationID: "op"})
	})
	if _, err := passthrough(nil, t.Context(), nil); err.Error() != "unknown_outcome" {
		t.Fatalf("native error after deadline became %v", err)
	}
}

func TestCancelRequestStopsRunningHandlerWithCancelledReply(t *testing.T) {
	started := make(chan struct{}, 1)
	slow := deadline(time.Minute, blockingMethod(started))
	left, right := net.Pipe()
	conn := rpc.NewConn(t.Context(), left, func(ctx context.Context, _ string, params jsontext.Value) (jsontext.Value, error) {
		return slow(nil, ctx, params)
	})
	conn.OpenStream("s1", nil)
	t.Cleanup(func() { _ = conn.Close(); _ = right.Close() })
	writeCancelLines(t, right, started)
	checkCancelReply(t, right)
}

func writeCancelLines(t *testing.T, right net.Conn, started <-chan struct{}) {
	t.Helper()
	write := func(line string) {
		if _, err := io.WriteString(right, line+"\n"); err != nil {
			t.Fatal(err)
		}
	}
	write(`{"jsonrpc":"2.0","id":"page-1","method":"dalgona/history/page"}`)
	<-started
	write(`{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":"page-1"}}`)
}

func checkCancelReply(t *testing.T, right net.Conn) {
	t.Helper()
	line, err := bufio.NewReader(right).ReadBytes('\n')
	if err != nil {
		t.Fatal(err)
	}
	var reply message
	if err := json.Unmarshal(line, &reply); err != nil {
		t.Fatal(err)
	}
	data, ok := rpc.ErrorDataOf(reply.Error)
	if string(reply.ID) != `"page-1"` || reply.Error == nil || reply.Error.Code != -32019 || reply.Error.Message != "cancelled" || !ok || data != (rpc.ErrorData{Kind: rpc.ErrorCancelled}) {
		t.Fatalf("cancelled reply = %s", line)
	}
}
