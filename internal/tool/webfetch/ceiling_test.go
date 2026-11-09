package webfetch

import (
	"encoding/json/jsontext"
	"encoding/json/v2"
	"strings"
	"testing"
)

func TestMaxBytesRejectsValuesOverTheCeiling(t *testing.T) {
	cases := []struct {
		raw     string
		wantErr bool
	}{
		{"1", false},
		{"1048576", false},
		{"16777216", false},
		{"16777217", true},
		{"9223372036854775807", true},
	}
	for _, tc := range cases {
		limit, err := positiveBytes(tc.raw)
		if tc.wantErr && err == nil {
			t.Errorf("positiveBytes(%s) accepted %d", tc.raw, limit)
		}
		if !tc.wantErr && err != nil {
			t.Errorf("positiveBytes(%s) failed: %v", tc.raw, err)
		}
	}
}

func TestParseArgsRejectsOversizedMaxBytes(t *testing.T) {
	_, err := parseArgs(jsontext.Value(`{"url":"https://example.com","max_bytes":1073741824}`), publicAddrs)
	if err == nil {
		t.Fatal("parseArgs accepted max_bytes over the ceiling")
	}
	if !strings.Contains(err.Error(), "16777216") {
		t.Fatalf("error %v does not name the ceiling", err)
	}
}

func TestFetchSchemaCapsMaxBytes(t *testing.T) {
	tool := New().(*Tool)
	var schema struct {
		Properties map[string]struct {
			Maximum *int64 `json:"maximum"`
		} `json:"properties"`
	}
	if err := json.Unmarshal([]byte(tool.Spec().Schema), &schema); err != nil {
		t.Fatalf("decode schema: %v", err)
	}
	property, ok := schema.Properties["max_bytes"]
	if !ok || property.Maximum == nil {
		t.Fatal("schema lacks max_bytes maximum")
	}
	if *property.Maximum != 16<<20 {
		t.Fatalf("schema maximum %d, want %d", *property.Maximum, 16<<20)
	}
}
