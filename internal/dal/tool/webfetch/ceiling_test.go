package webfetch

import (
	"encoding/json/jsontext"
	"encoding/json/v2"
	"strconv"
	"strings"
	"testing"
)

func TestMaxBytesRejectsValuesOverTheCeiling(t *testing.T) {
	cases := []struct {
		raw       string
		wantLimit int64
		wantErr   bool
	}{
		{raw: "0", wantErr: true},
		{raw: "1", wantLimit: 1, wantErr: false},
		{raw: "1048576", wantLimit: 1048576, wantErr: false},
		{raw: "16777215", wantLimit: 16777215, wantErr: false},
		{raw: "16777216", wantLimit: 16777216, wantErr: false},
		{raw: "16777217", wantErr: true},
		{raw: "9223372036854775807", wantErr: true},
	}
	for _, tc := range cases {
		limit, err := positiveBytes(tc.raw)
		if tc.wantErr {
			if err == nil {
				t.Errorf("positiveBytes(%s) accepted %d", tc.raw, limit)
			}
			if limit != 0 {
				t.Errorf("positiveBytes(%s) returned non-zero limit %d on error", tc.raw, limit)
			}
			continue
		}
		if err != nil {
			t.Errorf("positiveBytes(%s) failed: %v", tc.raw, err)
			continue
		}
		if limit != tc.wantLimit {
			t.Errorf("positiveBytes(%s) = %d, want %d", tc.raw, limit, tc.wantLimit)
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
			Minimum *int64 `json:"minimum"`
			Maximum *int64 `json:"maximum"`
		} `json:"properties"`
	}
	if err := json.Unmarshal([]byte(tool.Spec().Schema), &schema); err != nil {
		t.Fatalf("decode schema: %v", err)
	}
	property, ok := schema.Properties["max_bytes"]
	if !ok || property.Maximum == nil || property.Minimum == nil {
		t.Fatal("schema lacks max_bytes minimum or maximum")
	}
	if *property.Maximum != 16<<20 {
		t.Fatalf("schema maximum %d, want %d", *property.Maximum, 16<<20)
	}
	if *property.Minimum != 1 {
		t.Fatalf("schema minimum %d, want 1", *property.Minimum)
	}
	requireSchemaByteBounds(t, *property.Minimum, *property.Maximum)
}

func requireSchemaByteBounds(t *testing.T, minimum, maximum int64) {
	t.Helper()
	maxLimit, err := positiveBytes(strconv.FormatInt(maximum, 10))
	if err != nil || maxLimit != maximum {
		t.Fatalf("schema maximum %d rejected by positiveBytes: %v", maximum, err)
	}
	if _, aboveErr := positiveBytes(strconv.FormatInt(maximum+1, 10)); aboveErr == nil {
		t.Fatalf("schema maximum+1 (%d) accepted by positiveBytes", maximum+1)
	}
	minLimit, err := positiveBytes(strconv.FormatInt(minimum, 10))
	if err != nil || minLimit != minimum {
		t.Fatalf("schema minimum %d rejected by positiveBytes: %v", minimum, err)
	}
	if _, belowErr := positiveBytes(strconv.FormatInt(minimum-1, 10)); belowErr == nil {
		t.Fatalf("schema minimum-1 (%d) accepted by positiveBytes", minimum-1)
	}
}
