package wire_test

import (
	"bytes"
	"encoding/json/jsontext"
	"math"
	"math/rand/v2"
	"reflect"
	"strconv"
	"testing"

	json "encoding/json/v2"

	"github.com/gosuda/dalgona/internal/model/cursor/wire"
	"google.golang.org/protobuf/encoding/protowire"
)

func randomJSON(r *rand.Rand, depth int) any {
	kind := r.IntN(6)
	if depth > 3 {
		kind = r.IntN(4)
	}
	switch kind {
	case 0:
		return nil
	case 1:
		return r.IntN(2) == 0
	case 2:
		return float64(r.IntN(2000)-1000) / 8
	case 3:
		return "s" + strconv.Itoa(r.IntN(100)) + "\u00e9\"\\"
	case 4:
		return randomJSONArray(r, depth)
	default:
		return randomJSONObject(r, depth)
	}
}

func randomJSONArray(r *rand.Rand, depth int) []any {
	items := make([]any, r.IntN(4))
	for index := range items {
		items[index] = randomJSON(r, depth+1)
	}
	return items
}

func randomJSONObject(r *rand.Rand, depth int) map[string]any {
	object := make(map[string]any)
	for index := range r.IntN(4) {
		object["k"+strconv.Itoa(index)] = randomJSON(r, depth+1)
	}
	return object
}

func TestValueJSONRoundTrip(t *testing.T) {
	t.Parallel()
	r := rand.New(rand.NewPCG(7, 11))
	for range 256 {
		want := randomJSON(r, 0)
		raw, err := json.Marshal(want)
		if err != nil {
			t.Fatal(err)
		}
		encoded, err := wire.AppendValueFromJSON([]byte{0xff}, raw)
		if err != nil {
			t.Fatalf("encode %s: %v", raw, err)
		}
		back, err := wire.ValueJSON(encoded[1:])
		if err != nil {
			t.Fatalf("decode %s: %v", raw, err)
		}
		var got any
		if err := json.Unmarshal(back, &got); err != nil {
			t.Fatalf("decoded JSON %s: %v", back, err)
		}
		if !reflect.DeepEqual(got, want) {
			t.Fatalf("round trip %s -> %s", raw, back)
		}
	}
}

func TestHandAssembledValue(t *testing.T) {
	t.Parallel()
	number := protowire.AppendTag(nil, 2, protowire.Fixed64Type)
	number = protowire.AppendFixed64(number, math.Float64bits(1.5))
	list := appendField(1, number)
	listValue := appendField(6, list)
	entry := protowire.AppendTag(nil, 1, protowire.BytesType)
	entry = protowire.AppendString(entry, "a")
	entry = append(entry, appendField(2, listValue)...)
	want := appendField(5, appendField(1, entry))
	got, err := wire.AppendValueFromJSON(nil, jsontext.Value(`{"a":[1.5]}`))
	if err != nil || !bytes.Equal(got, want) {
		t.Fatalf("value = %x, want %x, err = %v", got, want, err)
	}
}

func TestValueRejectsTrailingAndDeepJSON(t *testing.T) {
	t.Parallel()
	if _, err := wire.AppendValueFromJSON(nil, jsontext.Value(`{} {}`)); err == nil {
		t.Fatal("trailing JSON accepted")
	}
	deep := bytes.Repeat([]byte("["), 80)
	deep = append(deep, bytes.Repeat([]byte("]"), 80)...)
	if _, err := wire.AppendValueFromJSON(nil, deep); err == nil {
		t.Fatal("80-level JSON accepted")
	}
}

func TestControlAndShellStreamRoundTrips(t *testing.T) {
	t.Parallel()
	checkRoundTrip(t, "exec throw", func(r *rand.Rand) wire.ExecClientControl {
		return wire.ExecClientControl{Kind: wire.ExecThrow, ID: r.Uint32() | 1, ErrorView: randomBytes(r)}
	}, wire.AppendExecClientControl, wire.ConsumeExecClientControl)
	checkRoundTrip(t, "exec heartbeat", func(r *rand.Rand) wire.ExecClientControl {
		return wire.ExecClientControl{Kind: wire.ExecHeartbeat, ID: r.Uint32() | 1}
	}, wire.AppendExecClientControl, wire.ConsumeExecClientControl)
	checkRoundTrip(t, "shell stdout", func(r *rand.Rand) wire.ShellStreamEvent {
		return wire.ShellStreamEvent{Kind: wire.ShellStreamStdout, DataView: randomBytes(r)}
	}, wire.AppendShellStreamEvent, wire.ConsumeShellStreamEvent)
	checkRoundTrip(t, "shell exit", func(r *rand.Rand) wire.ShellStreamEvent {
		return wire.ShellStreamEvent{Kind: wire.ShellStreamExit, Exit: &wire.ShellExit{Code: r.Uint32() | 1, CwdView: randomBytes(r), Aborted: true}}
	}, wire.AppendShellStreamEvent, wire.ConsumeShellStreamEvent)
}

func TestHandAssembledStreamClose(t *testing.T) {
	t.Parallel()
	id := protowire.AppendTag(nil, 1, protowire.VarintType)
	id = protowire.AppendVarint(id, 9)
	want := appendField(1, id)
	if got := wire.AppendExecClientControl(nil, &wire.ExecClientControl{Kind: wire.ExecStreamClose, ID: 9}); !bytes.Equal(got, want) {
		t.Fatalf("frame = %x, want %x", got, want)
	}
}
