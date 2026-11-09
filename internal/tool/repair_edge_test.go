package tool

import (
	"encoding/json/jsontext"
	"strings"
	"testing"
)

func TestRepairDecodesNumberAndDropsMany(t *testing.T) {
	schema := jsontext.Value(`{"type":"object","properties":{"count":{"type":"integer"}}}`)
	got, repairs, err := Repair(schema, jsontext.Value(`{"count":"42"}`))
	if err != nil {
		t.Fatal(err)
	}
	if repairs != 1 || strings.TrimSpace(string(got)) != `{"count":42}` {
		t.Fatalf("got %s repairs %d", got, repairs)
	}
}

func TestRepairLimit(t *testing.T) {
	schema := jsontext.Value(`{"type":"object","properties":{"keep":{"type":"string"}}}`)
	raw := `{"keep":"x"`
	for index := range 17 {
		raw += `,"unknown` + string(rune('a'+index%26)) + `":1`
	}
	raw += `}`
	_, _, err := Repair(schema, jsontext.Value(raw))
	if err == nil || err.Error() != "too many argument repairs" {
		t.Fatalf("err = %v", err)
	}
}

func TestRepairPreservesValues(t *testing.T) {
	schema := jsontext.Value(`{"type":"object","properties":{"name":{"type":"string"},"flag":{"type":"boolean"},"list":{"type":"array"}}}`)
	args := jsontext.Value(`{"name":"{}","flag":"true","list":"[1,2]","gone":false}`)
	got, repairs, err := Repair(schema, args)
	if err != nil {
		t.Fatal(err)
	}
	if repairs != 3 {
		t.Fatalf("repairs = %d", repairs)
	}
	want := `{"name":"{}","flag":true,"list":[1,2]}`
	if strings.TrimSpace(string(got)) != want {
		t.Fatalf("got %s want %s", got, want)
	}
}
