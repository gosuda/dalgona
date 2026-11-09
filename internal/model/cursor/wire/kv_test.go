package wire_test

import (
	"bytes"
	"math/rand/v2"
	"testing"

	"github.com/gosuda/dalgona/internal/model/cursor/wire"
	"google.golang.org/protobuf/encoding/protowire"
)

func TestKvAndShellRoundTrips(t *testing.T) {
	t.Parallel()
	checkRoundTrip(t, "kv server get", func(r *rand.Rand) wire.KvServerMessage {
		return wire.KvServerMessage{ID: r.Uint32() | 1, Get: &wire.GetBlobArgs{BlobIDView: randomBytes(r)}}
	}, wire.AppendKvServerMessage, wire.ConsumeKvServerMessage)
	checkRoundTrip(t, "kv server set", func(r *rand.Rand) wire.KvServerMessage {
		return wire.KvServerMessage{ID: r.Uint32() | 1, Set: &wire.SetBlobArgs{BlobIDView: randomBytes(r), BlobDataView: randomBytes(r)}}
	}, wire.AppendKvServerMessage, wire.ConsumeKvServerMessage)
	checkRoundTrip(t, "kv client get", func(r *rand.Rand) wire.KvClientMessage {
		return wire.KvClientMessage{ID: r.Uint32() | 1, Get: &wire.GetBlobResult{BlobDataView: randomBytes(r)}}
	}, wire.AppendKvClientMessage, wire.ConsumeKvClientMessage)
	checkRoundTrip(t, "kv client set", func(r *rand.Rand) wire.KvClientMessage {
		return wire.KvClientMessage{ID: r.Uint32() | 1, Set: &wire.SetBlobResult{ErrorView: randomBytes(r)}}
	}, wire.AppendKvClientMessage, wire.ConsumeKvClientMessage)
	checkRoundTrip(t, "shell failure", func(r *rand.Rand) wire.ShellResult {
		return wire.ShellResult{Failure: &wire.ShellOutcome{
			CommandView: randomBytes(r), WorkingDirectoryView: randomBytes(r), ExitCode: r.Int32() - r.Int32(),
			SignalView: randomBytes(r), StdoutView: randomBytes(r), StderrView: randomBytes(r), ExecutionTime: r.Int32(),
		}}
	}, wire.AppendShellResult, wire.ConsumeShellResult)
}

func TestHandAssembledGetBlob(t *testing.T) {
	t.Parallel()
	args := protowire.AppendTag(nil, 1, protowire.BytesType)
	args = protowire.AppendBytes(args, []byte{0xde, 0xad})
	kv := protowire.AppendTag(nil, 1, protowire.VarintType)
	kv = protowire.AppendVarint(kv, 7)
	kv = append(kv, appendField(2, args)...)
	server := appendField(protowire.Number(wire.ServerKvNumber), kv)
	var message wire.AgentServerMessage
	if err := wire.ConsumeAgentServerMessage(server, &message); err != nil {
		t.Fatal(err)
	}
	if message.MessageNumber != wire.ServerKvNumber {
		t.Fatalf("server variant = %d", message.MessageNumber)
	}
	var request wire.KvServerMessage
	if err := wire.ConsumeKvServerMessage(message.MessageView, &request); err != nil {
		t.Fatal(err)
	}
	if request.ID != 7 || request.Get == nil || !bytes.Equal(request.Get.BlobIDView, []byte{0xde, 0xad}) || request.Set != nil {
		t.Fatalf("request = %#v", request)
	}
}

func TestEmptyBlobResultKeepsPresence(t *testing.T) {
	t.Parallel()
	got := wire.AppendKvClientMessage(nil, &wire.KvClientMessage{ID: 2, Get: &wire.GetBlobResult{BlobDataView: []byte{}}})
	want := protowire.AppendTag(nil, 1, protowire.VarintType)
	want = protowire.AppendVarint(want, 2)
	result := protowire.AppendTag(nil, 1, protowire.BytesType)
	result = protowire.AppendBytes(result, nil)
	want = append(want, appendField(2, result)...)
	if !bytes.Equal(got, want) {
		t.Fatalf("frame = %x, want %x", got, want)
	}
	missing := wire.AppendKvClientMessage(nil, &wire.KvClientMessage{ID: 2, Get: &wire.GetBlobResult{}})
	if bytes.Equal(missing, want) {
		t.Fatal("absent blob encodes like an empty blob")
	}
}

func TestHandAssembledShellSuccess(t *testing.T) {
	t.Parallel()
	got := wire.AppendShellResult(nil, &wire.ShellResult{Success: &wire.ShellOutcome{CommandView: []byte("echo hi"), StdoutView: []byte("hi\n"), ExitCode: 0}})
	outcome := protowire.AppendTag(nil, 1, protowire.BytesType)
	outcome = protowire.AppendString(outcome, "echo hi")
	outcome = protowire.AppendTag(outcome, 5, protowire.BytesType)
	outcome = protowire.AppendString(outcome, "hi\n")
	if want := appendField(1, outcome); !bytes.Equal(got, want) {
		t.Fatalf("frame = %x, want %x", got, want)
	}
}
