package tool_test

import (
	"fmt"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/tool"
)

func TestRepair_dropsUnknownProperty(t *testing.T) {
	t.Parallel()
	schema := `{"type":"object","properties":{"path":{"type":"string"}}}`
	got, repairs, err := tool.Repair([]byte(schema), []byte(`{"path":"a","bogus":1}`))
	if err != nil {
		t.Fatal(err)
	}
	if repairs != 1 || string(got) != `{"path":"a"}` {
		t.Fatalf("got %s with %d repairs", got, repairs)
	}
}

func TestRepair_decodesDeclaredArray(t *testing.T) {
	t.Parallel()
	schema := `{"type":"object","properties":{"items":{"type":"array"}}}`
	raw := `{"items":"[{\"text\":\"x\"}]"}`
	got, repairs, err := tool.Repair([]byte(schema), []byte(raw))
	if err != nil {
		t.Fatal(err)
	}
	if repairs != 1 || string(got) != `{"items":[{"text":"x"}]}` {
		t.Fatalf("got %s with %d repairs", got, repairs)
	}
}

func TestRepair_preservesStringProperty(t *testing.T) {
	t.Parallel()
	schema := `{"type":"object","properties":{"text":{"type":"string"}}}`
	got, repairs, err := tool.Repair([]byte(schema), []byte(`{"text":"{}"}`))
	if err != nil {
		t.Fatal(err)
	}
	if repairs != 0 || string(got) != `{"text":"{}"}` {
		t.Fatalf("got %s with %d repairs", got, repairs)
	}
}

func TestRepair_declaredTypes(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name   string
		schema string
		raw    string
		want   string
		count  int
	}{
		{name: "union with string", schema: `{"type":["string","object"]}`, raw: `"{}"`, want: `"{}"`, count: 0},
		{name: "object", schema: `{"type":"object"}`, raw: `"{\"x\":1}"`, want: `{"x":1}`, count: 1},
		{name: "number", schema: `{"type":"number"}`, raw: `"12.5"`, want: `12.5`, count: 1},
		{name: "integer", schema: `{"type":"integer"}`, raw: `"12"`, want: `12`, count: 1},
		{name: "fraction is not integer", schema: `{"type":"integer"}`, raw: `"12.5"`, want: `"12.5"`, count: 0},
		{name: "boolean", schema: `{"type":"boolean"}`, raw: `"true"`, want: `true`, count: 1},
		{name: "null is not repaired", schema: `{"type":"null"}`, raw: `"null"`, want: `"null"`, count: 0},
		{name: "wrong decoded kind", schema: `{"type":"boolean"}`, raw: `"{}"`, want: `"{}"`, count: 0},
		{name: "invalid json string", schema: `{"type":"object"}`, raw: `"not json"`, want: `"not json"`, count: 0},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			schema := `{"type":"object","properties":{"value":` + test.schema + `}}`
			raw := `{"value":` + test.raw + `}`
			got, repairs, err := tool.Repair([]byte(schema), []byte(raw))
			if err != nil {
				t.Fatal(err)
			}
			want := `{"value":` + test.want + `}`
			if repairs != test.count || string(got) != want {
				t.Fatalf("got %s with %d repairs, want %s with %d", got, repairs, want, test.count)
			}
		})
	}
}

func TestRepair_allowsSixteenRepairs(t *testing.T) {
	t.Parallel()
	got, repairs, err := tool.Repair([]byte(`{"type":"object","properties":{}}`), unknownObject(16))
	if err != nil {
		t.Fatal(err)
	}
	if repairs != 16 || string(got) != `{}` {
		t.Fatalf("got %s with %d repairs", got, repairs)
	}
}

func TestRepair_rejectsSeventeenRepairs(t *testing.T) {
	t.Parallel()
	got, repairs, err := tool.Repair([]byte(`{"type":"object","properties":{}}`), unknownObject(17))
	if err == nil || err.Error() != "too many argument repairs" || repairs != 17 {
		t.Fatalf("got %s, %d, %v", got, repairs, err)
	}
}

func unknownObject(count int) []byte {
	fields := make([]string, count)
	for index := range fields {
		fields[index] = fmt.Sprintf(`"unknown%d":0`, index)
	}
	return []byte("{" + strings.Join(fields, ",") + "}")
}
