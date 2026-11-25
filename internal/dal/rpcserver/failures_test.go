package rpcserver

import (
	"bytes"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/failure"
)

type plainMasker struct{}

func (plainMasker) MaskWithError(text string) (string, error) { return text, nil }

type failureSummary struct {
	ID          string `json:"id"`
	Time        string `json:"time"`
	Product     string `json:"product"`
	Class       string `json:"class"`
	Tool        string `json:"tool"`
	Fingerprint string `json:"fingerprint"`
	Reportable  bool   `json:"reportable"`
}

type failureList struct {
	Records    []failureSummary `json:"records"`
	NextCursor string           `json:"nextCursor"`
}

func captureFailure(t *testing.T, r *rig, n int, at time.Time) failure.Record {
	t.Helper()
	store, err := failure.Open(t.Context(), failure.Options{Home: r.home, Product: "dal", Version: "1.0", Masker: plainMasker{}})
	if err != nil {
		t.Fatal(err)
	}
	record, err := store.Capture(t.Context(), failure.Input{
		ID: fmt.Sprintf("%032x", n), Time: at, Product: "dal", Version: "1.0", Class: failure.ClassTool, Tool: "exec",
		Err: errors.New("exec tool failed"), Args: `{"command":"ls"}`, Output: "partial", Workspace: r.cwd, Origin: failure.OriginMain,
		Correlation: failure.Correlation{Session: "sidecar-session-7f3a", Turn: "sidecar-turn", CallID: "sidecar-call"},
	})
	if err != nil {
		t.Fatal(err)
	}
	return record
}

func listedIDs(list failureList) []string {
	ids := make([]string, len(list.Records))
	for i, summary := range list.Records {
		ids[i] = summary.ID
	}
	return ids
}

func failureIDs(numbers ...int) []string {
	ids := make([]string, len(numbers))
	for i, n := range numbers {
		ids[i] = fmt.Sprintf("%032x", n)
	}
	return ids
}

func TestFailuresListPagesNewestFirst(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	base := time.Now().UTC().Truncate(time.Second).Add(-time.Hour)
	for n := 1; n <= 5; n++ {
		captureFailure(t, r, n, base.Add(time.Duration(n)*time.Minute))
	}
	c := r.connect()
	cursor := checkFirstPage(t, c, base)
	checkMiddlePages(t, c, r, base, cursor)
	checkDefaultPage(t, c)
	checkBadListParams(t, c, cursor)
}

func checkFirstPage(t *testing.T, c *wireClient, base time.Time) string {
	t.Helper()
	var first failureList
	c.ok("dalgona/failures/list", map[string]any{"limit": 2}, &first)
	if !slices.Equal(listedIDs(first), failureIDs(5, 4)) || first.NextCursor == "" {
		t.Fatalf("first page = %v cursor %q, want records 5 and 4 with a cursor", listedIDs(first), first.NextCursor)
	}
	summary := first.Records[0]
	if summary.Time != rpc.FormatTimestamp(base.Add(5*time.Minute)) || summary.Product != "dal" || summary.Class != "tool" || summary.Tool != "exec" || summary.Fingerprint == "" {
		t.Fatalf("summary = %+v", summary)
	}
	return first.NextCursor
}

func checkMiddlePages(t *testing.T, c *wireClient, r *rig, base time.Time, cursor string) {
	t.Helper()
	captureFailure(t, r, 6, base.Add(10*time.Minute))
	var second failureList
	c.ok("dalgona/failures/list", map[string]any{"limit": 2, "cursor": cursor}, &second)
	if !slices.Equal(listedIDs(second), failureIDs(3, 2)) || second.NextCursor == "" {
		t.Fatalf("second page = %v cursor %q, want records 3 and 2 with a cursor", listedIDs(second), second.NextCursor)
	}
	var last failureList
	c.ok("dalgona/failures/list", map[string]any{"limit": 2, "cursor": second.NextCursor}, &last)
	if !slices.Equal(listedIDs(last), failureIDs(1)) || last.NextCursor != "" {
		t.Fatalf("last page = %v cursor %q, want record 1 without a cursor", listedIDs(last), last.NextCursor)
	}
}

func checkDefaultPage(t *testing.T, c *wireClient) {
	t.Helper()
	var all failureList
	c.ok("dalgona/failures/list", map[string]any{}, &all)
	if !slices.Equal(listedIDs(all), failureIDs(6, 5, 4, 3, 2, 1)) || all.NextCursor != "" {
		t.Fatalf("default page = %v cursor %q", listedIDs(all), all.NextCursor)
	}
}

func checkBadListParams(t *testing.T, c *wireClient, cursor string) {
	t.Helper()
	for _, params := range []map[string]any{{"limit": 101}, {"limit": 0}, {"cursor": "not a cursor"}, {"cursor": cursor + "x"}} {
		c.fails("dalgona/failures/list", params, -32602, "invalid_params")
	}
}

func TestFailuresReadReturnsThePublicRecordOnly(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	captured := captureFailure(t, r, 7, time.Now().UTC().Add(-time.Minute))
	c := r.connect()
	m := c.call("dalgona/failures/read", map[string]any{"id": captured.ID})
	if m.Error != nil {
		t.Fatalf("read failed: %+v", m.Error)
	}
	checkPublicRecord(t, m.Result, captured)
	checkBadReadIDs(t, c)
}

func recordsMatch(got, captured failure.Record) bool {
	return got.ID == captured.ID && got.Error == captured.Error && got.Args == captured.Args && got.Output == captured.Output &&
		got.Fingerprint == captured.Fingerprint && got.Time.Equal(captured.Time) && got.SchemaVersion == 1
}

func checkPublicRecord(t *testing.T, result jsontext.Value, captured failure.Record) {
	t.Helper()
	var got failure.Record
	if err := json.Unmarshal(result, &got); err != nil {
		t.Fatal(err)
	}
	if !recordsMatch(got, captured) {
		t.Fatalf("read record = %+v, want %+v", got, captured)
	}
	if bytes.Contains(result, []byte("sidecar-")) {
		t.Fatalf("read record exposes the local correlation: %s", result)
	}
}

func checkBadReadIDs(t *testing.T, c *wireClient) {
	t.Helper()
	c.fails("dalgona/failures/read", map[string]any{"id": strings.Repeat("e", 32)}, -32020, "not_found")
	for _, id := range []string{"", "../failure-x", strings.Repeat("E", 32), strings.Repeat("e", 33)} {
		c.fails("dalgona/failures/read", map[string]any{"id": id}, -32602, "invalid_params")
	}
}

func TestFailureCursorExpiresAfterFiveMinutes(t *testing.T) {
	issued := time.Date(2026, 11, 24, 9, 0, 0, 0, time.UTC)
	position := failure.Position{Time: issued.Add(-time.Hour), ID: strings.Repeat("a", 32)}
	cursor := encodeFailureCursor(position, issued)
	got, err := decodeFailureCursor(cursor, issued.Add(5*time.Minute))
	if err != nil || !got.Time.Equal(position.Time) || got.ID != position.ID {
		t.Fatalf("cursor within five minutes = %+v, %v", got, err)
	}
	_, err = decodeFailureCursor(cursor, issued.Add(5*time.Minute+time.Millisecond))
	if data, ok := rpc.ErrorDataOf(err); !ok || data.Kind != rpc.ErrorExpired {
		t.Fatalf("cursor after five minutes: %v", err)
	}
}
