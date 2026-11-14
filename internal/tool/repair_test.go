package tool_test

import (
	json "encoding/json/v2"
	"fmt"
	"maps"
	"reflect"
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

func TestRepair_keepsKnownPropertiesAroundUnknown(t *testing.T) {
	t.Parallel()
	schema := `{"type":"object","properties":{"name":{"type":"string"},"flag":{"type":"boolean"},"list":{"type":"array"}}}`
	raw := `{"name":"{}","gone":false,"flag":"true","list":"[1,2]"}`
	got, repairs, err := tool.Repair([]byte(schema), []byte(raw))
	if err != nil {
		t.Fatal(err)
	}
	if repairs != 3 {
		t.Fatalf("repairs = %d, want 3", repairs)
	}

	type keptProperties struct {
		Name string `json:"name"`
		Flag bool   `json:"flag"`
		List []int  `json:"list"`
	}
	var gotProps keptProperties
	if err := json.Unmarshal(got, &gotProps, json.RejectUnknownMembers(true)); err != nil {
		t.Fatalf("unmarshal got %s: %v", got, err)
	}
	wantProps := keptProperties{
		Name: "{}",
		Flag: true,
		List: []int{1, 2},
	}
	if !reflect.DeepEqual(gotProps, wantProps) {
		t.Fatalf("got %+v, want %+v", gotProps, wantProps)
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
	tests := []struct {
		name   string
		schema []byte
		raw    []byte
		want   map[string]int
	}{
		{
			name:   "unknown properties",
			schema: []byte(`{"type":"object","properties":{}}`),
			raw:    unknownObject(16),
			want:   integerMap(0),
		},
		{
			name:   "changed integer properties",
			schema: integerSchema(16),
			raw:    integerRaw(16),
			want:   integerMap(16),
		},
		{
			name:   "mixed properties",
			schema: integerSchema(8),
			raw:    mixedRaw(8, 8),
			want:   integerMap(8),
		},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			checkSixteenRepairs(t, test.schema, test.raw, test.want)
		})
	}
}

func checkSixteenRepairs(t *testing.T, schema []byte, raw []byte, want map[string]int) {
	t.Helper()
	got, repairs, err := tool.Repair(schema, raw)
	if err != nil {
		t.Fatal(err)
	}
	if repairs != 16 {
		t.Fatalf("repairs = %d, want 16", repairs)
	}
	var values map[string]int
	if err := json.Unmarshal(got, &values); err != nil {
		t.Fatal(err)
	}
	if !maps.Equal(values, want) {
		t.Fatalf("got %v, want %v", values, want)
	}
}

func TestRepair_rejectsSeventeenRepairs(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name   string
		schema []byte
		raw    []byte
	}{
		{
			name:   "unknown properties",
			schema: []byte(`{"type":"object","properties":{}}`),
			raw:    unknownObject(17),
		},
		{
			name:   "changed integer properties",
			schema: integerSchema(17),
			raw:    integerRaw(17),
		},
		{
			name:   "mixed properties unknown limit",
			schema: integerSchema(8),
			raw:    mixedRaw(8, 9),
		},
		{
			name:   "mixed properties changed limit",
			schema: integerSchema(9),
			raw:    mixedRaw(9, 8),
		},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			got, repairs, err := tool.Repair(test.schema, test.raw)
			if err == nil || err.Error() != "too many argument repairs" || repairs != 17 {
				t.Fatalf("got %s, %d repairs, err = %v", got, repairs, err)
			}
		})
	}
}

func unknownObject(count int) []byte {
	fields := make([]string, count)
	for index := range fields {
		fields[index] = fmt.Sprintf(`"unknown%d":0`, index)
	}
	return []byte("{" + strings.Join(fields, ",") + "}")
}

func integerSchema(count int) []byte {
	props := make([]string, count)
	for index := range props {
		props[index] = fmt.Sprintf(`"int%d":{"type":"integer"}`, index)
	}
	return []byte(fmt.Sprintf(`{"type":"object","properties":{%s}}`, strings.Join(props, ",")))
}

func integerRaw(count int) []byte {
	fields := make([]string, count)
	for index := range fields {
		fields[index] = fmt.Sprintf(`"int%d":"1"`, index)
	}
	return []byte("{" + strings.Join(fields, ",") + "}")
}

func mixedRaw(knownCount int, unknownCount int) []byte {
	fields := make([]string, 0, knownCount+unknownCount)
	maxCount := max(knownCount, unknownCount)
	for index := range maxCount {
		if index < unknownCount {
			fields = append(fields, fmt.Sprintf(`"unknown%d":0`, index))
		}
		if index < knownCount {
			fields = append(fields, fmt.Sprintf(`"int%d":"1"`, index))
		}
	}
	return []byte("{" + strings.Join(fields, ",") + "}")
}

func integerMap(count int) map[string]int {
	values := make(map[string]int, count)
	for index := range count {
		values[fmt.Sprintf("int%d", index)] = 1
	}
	return values
}
