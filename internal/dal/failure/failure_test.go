package failure

import (
	"context"
	"encoding/json/v2"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"
)

type mapMasker struct {
	replacements map[string]string
	fail         bool
}

func (m mapMasker) MaskWithError(text string) (string, error) {
	if m.fail {
		return "", errors.New("masker down")
	}
	out := text
	for from, to := range m.replacements {
		out = strings.ReplaceAll(out, from, to)
	}
	return out, nil
}

func testWorkspace() string {
	return filepath.ToSlash("/home/test/projx9")
}

func testInput() Input {
	return Input{
		Product:     "dalgona",
		Version:     "1.2.3",
		Class:       ClassTool,
		Tool:        "exec",
		Err:         errors.New("boom"),
		Args:        `{"command":"ls"}`,
		Output:      "ok",
		Workspace:   testWorkspace(),
		Origin:      OriginMain,
		Correlation: Correlation{Session: "sess-1", Turn: "turn-1", CallID: "call-1"},
	}
}

func buildOrFail(t *testing.T, input Input, masker Masker) Record {
	t.Helper()
	record, err := Build(context.Background(), input, masker)
	if err != nil {
		t.Fatalf("Build: %v", err)
	}
	return record
}

func TestBuildMasksDecodedArguments(t *testing.T) {
	workspace := testWorkspace()
	masker := mapMasker{replacements: map[string]string{"s3cr3t": "XX"}}
	input := testInput()
	input.Tool = "patch"
	input.Args = `{"path":"s3cr3t-key","mode":"edit","token":"s3cr3t","plain":"sk-ant-abcdef0123456789ABCD"}`
	input.Workspace = workspace
	record := buildOrFail(t, input, masker)
	var decoded map[string]any
	if err := json.Unmarshal([]byte(record.Args), &decoded); err != nil {
		t.Fatalf("record args are not JSON: %v", err)
	}
	encoded, _ := json.Marshal(decoded)
	text := string(encoded)
	for _, forbidden := range []string{"s3cr3t", "sk-ant-abcdef0123456789ABCD"} {
		if strings.Contains(text, forbidden) {
			t.Fatalf("secret %q survived in %q", forbidden, text)
		}
	}
	if !strings.Contains(text, "[redacted]") {
		t.Fatalf("expected scanner redaction in %q", text)
	}
}

func TestBuildContentFreeOnMaskError(t *testing.T) {
	input := testInput()
	input.FailingStep = &FailureStep{SchemaVersion: 1, Status: StatusReady}
	record, err := Build(context.Background(), input, mapMasker{fail: true})
	if err != nil {
		t.Fatalf("Build: %v", err)
	}
	assertContentFreeRecord(t, record)
	assertContentFreeAttachment(t, record)
	if !record.Reportable {
		t.Fatal("content-free dalgona main record stays reportable")
	}
	assertNilMaskerFallback(t, input)
}

func assertContentFreeRecord(t *testing.T, record Record) {
	t.Helper()
	if !record.ContentFree {
		t.Fatal("expected content-free record")
	}
	if record.Error != "failure details withheld: masking failed" {
		t.Fatalf("error = %q", record.Error)
	}
	if record.Args != "" || record.Output != "" || len(record.Stack) != 0 {
		t.Fatalf("content-free record leaks details: %+v", record)
	}
}

func assertContentFreeAttachment(t *testing.T, record Record) {
	t.Helper()
	step := record.FailingStep
	if step == nil {
		t.Fatalf("content-free attachment = %+v", record.FailingStep)
	}
	if step.Status != StatusUnavailable || step.Reason != "failure case unavailable: masking failed" {
		t.Fatalf("content-free attachment = %+v", record.FailingStep)
	}
	if step.Call != nil {
		t.Fatalf("content-free attachment = %+v", record.FailingStep)
	}
}

func assertNilMaskerFallback(t *testing.T, input Input) {
	t.Helper()
	nilRecord, err := Build(context.Background(), input, nil)
	if err != nil {
		t.Fatalf("Build nil masker: %v", err)
	}
	if !nilRecord.ContentFree {
		t.Fatal("nil masker must select content-free fallback")
	}
}

func TestBuildBoundsAndPaths(t *testing.T) {
	workspace := testWorkspace()
	masker := mapMasker{}
	input := testInput()
	input.Err = errors.New("denied: /etc/passwd and \"" + workspace + "/some dir/file.txt\" and C:\\secrets\\k and \\\\host\\share and https://user@example.com/viewed?x=1#frag and person@example.com and COLOR=blue and " + strings.Repeat("e", 5000))
	input.Output = "read " + workspace + "/a.txt then kept relative docs/guide.md and " + strings.Repeat("o", 5000)
	input.Args = `{"command":"ls docs/guide.md"}`
	record := buildOrFail(t, input, masker)
	if len(record.Error) > maxErrorBytes {
		t.Fatalf("error exceeds limit: %d", len(record.Error))
	}
	if !strings.Contains(record.Error, truncatedMarker) {
		t.Fatal("error must carry the truncation marker")
	}
	if len(record.Output) > maxOutputBytes {
		t.Fatalf("output exceeds limit: %d", len(record.Output))
	}
	text := record.Error + "\n" + record.Args
	for _, forbidden := range []string{"/etc/passwd", "C:\\secrets\\k", "host\\share", "user@example.com", "?x=1", "#frag", "COLOR=blue", workspace} {
		if strings.Contains(text, forbidden) {
			t.Fatalf("forbidden text %q survives in %q", forbidden, text)
		}
	}
	for _, required := range []string{"[path]", "docs/guide.md", "COLOR=[redacted]"} {
		if !strings.Contains(text, required) {
			t.Fatalf("expected %q in %q", required, text)
		}
	}
	if !strings.Contains(text, `"some dir/file.txt"`) {
		t.Fatalf("workspace path with spaces must relativize: %q", text)
	}
}

func TestBuildOriginPolicy(t *testing.T) {
	cases := []struct {
		product string
		origin  Origin
		want    bool
	}{
		{"dalgona", OriginMain, true},
		{"dalgona", OriginSide, false},
		{"dalgona", OriginDoctor, false},
		{"dalgona", OriginTest, false},
		{"dal", OriginMain, false},
		{"dal", OriginSide, false},
	}
	for _, tc := range cases {
		input := testInput()
		input.Product = tc.product
		input.Origin = tc.origin
		record := buildOrFail(t, input, mapMasker{})
		if record.Reportable != tc.want {
			t.Errorf("product %s origin %s reportable = %v, want %v", tc.product, tc.origin, record.Reportable, tc.want)
		}
	}
	for _, input := range []Input{
		func() Input { in := testInput(); in.Product = "other"; return in }(),
		func() Input { in := testInput(); in.Class = "nope"; return in }(),
		func() Input { in := testInput(); in.Origin = "nope"; return in }(),
		func() Input { in := testInput(); in.ID = "short"; return in }(),
	} {
		if _, err := Build(context.Background(), input, mapMasker{}); err == nil || err.Error() != "failure: invalid record" {
			t.Errorf("expected invalid record, got %v", err)
		}
	}
}

func TestFingerprintNormalization(t *testing.T) {
	frames := func(file string, line int) []Frame {
		return []Frame{{Function: "github.com/gosuda/dalgona/internal/agent/worker.runCall", File: file, Line: line}}
	}
	base := testInput()
	base.Frames = frames("/home/alice/checkout/internal/agent/tools.go", 10)
	baseRecord := buildOrFail(t, base, mapMasker{})
	other := testInput()
	other.Version = "9.9.9"
	other.Frames = frames("/home/bob/work/internal/agent/tools.go", 999)
	otherRecord := buildOrFail(t, other, mapMasker{})
	if baseRecord.Fingerprint != otherRecord.Fingerprint {
		t.Fatal("version, line and machine root must not change the fingerprint")
	}
	modded := testInput()
	modded.Frames = []Frame{{Function: "example.com/mod/agent@v1.2.3/worker.runCall", File: "/home/alice/go/pkg/mod/example.com/mod/agent@v1.2.3/tools.go", Line: 4}}
	modRecord := buildOrFail(t, modded, mapMasker{})
	_ = modRecord
	changed := testInput()
	changed.Frames = frames("/home/alice/checkout/internal/agent/other.go", 10)
	changedRecord := buildOrFail(t, changed, mapMasker{})
	if baseRecord.Fingerprint == changedRecord.Fingerprint {
		t.Fatal("application frame change must change the fingerprint")
	}
	for _, mutate := range []func(*Input){
		func(in *Input) { in.Product = "dal" },
		func(in *Input) { in.Class = ClassModel },
		func(in *Input) { in.Tool = "read" },
	} {
		alt := testInput()
		alt.Frames = frames("/home/alice/checkout/internal/agent/tools.go", 10)
		mutate(&alt)
		altRecord := buildOrFail(t, alt, mapMasker{})
		if baseRecord.Fingerprint == altRecord.Fingerprint {
			t.Fatal("product, class and tool enter the fingerprint")
		}
	}
	empty := testInput()
	emptyRecord := buildOrFail(t, empty, mapMasker{})
	if !strings.HasPrefix(emptyRecord.Fingerprint, "") || len(emptyRecord.Fingerprint) != 64 {
		t.Fatalf("fingerprint must be 64 lowercase hex, got %q", emptyRecord.Fingerprint)
	}
}

func TestRecordJSONMembers(t *testing.T) {
	record := buildOrFail(t, testInput(), mapMasker{})
	encoded, err := json.Marshal(record)
	if err != nil {
		t.Fatal(err)
	}
	var members map[string]any
	if err := json.Unmarshal(encoded, &members); err != nil {
		t.Fatal(err)
	}
	want := []string{"schemaVersion", "id", "time", "product", "version", "os", "arch", "class", "tool", "error", "stack", "args", "output", "fingerprint", "contentFree", "reportable", "failingStep"}
	if len(members) != len(want) {
		t.Fatalf("members = %v", members)
	}
	for _, key := range want {
		if _, ok := members[key]; !ok {
			t.Fatalf("missing member %s in %v", key, members)
		}
	}
}

func testStore(t *testing.T, product string, notice *strings.Builder) *Store {
	t.Helper()
	opts := Options{Home: t.TempDir(), Product: product, Version: "1.0", Masker: mapMasker{}}
	if notice != nil {
		opts.Notice = notice
	}
	store, err := Open(context.Background(), opts)
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	return store
}

func TestCaptureAtomicPublication(t *testing.T) {
	var notice strings.Builder
	store := testStore(t, "dalgona", &notice)
	input := testInput()
	input.ID = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	input.Time = time.Date(2026, 11, 1, 12, 0, 0, 0, time.UTC)
	record, err := store.Capture(context.Background(), input)
	if err != nil {
		t.Fatalf("Capture: %v", err)
	}
	assertRecordFilePrivate(t, store.dir, record.ID)
	assertNoStagingLeft(t, store.dir)
	assertCaptureIdempotent(t, store, input, record, &notice)
}

func assertRecordFilePrivate(t *testing.T, dir, id string) {
	t.Helper()
	info, err := os.Lstat(filepath.Join(dir, "failure-"+id+".json"))
	if err != nil {
		t.Fatal(err)
	}
	if runtime.GOOS != "windows" && info.Mode().Perm() != 0o600 {
		t.Fatalf("record mode = %v", info.Mode())
	}
	dirInfo, dirErr := os.Lstat(dir)
	if dirErr != nil {
		t.Fatal(dirErr)
	}
	if runtime.GOOS != "windows" && dirInfo.Mode().Perm() != 0o700 {
		t.Fatalf("dir mode = %v", dirInfo.Mode())
	}
}

func assertNoStagingLeft(t *testing.T, dir string) {
	t.Helper()
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	for _, entry := range entries {
		if strings.HasPrefix(entry.Name(), ".pending-") {
			t.Fatalf("staging file left behind: %s", entry.Name())
		}
	}
}

func assertCaptureIdempotent(t *testing.T, store *Store, input Input, record Record, notice *strings.Builder) {
	t.Helper()
	again, err := store.Capture(context.Background(), input)
	if err != nil {
		t.Fatalf("second Capture: %v", err)
	}
	if again.Fingerprint != record.Fingerprint {
		t.Fatal("idempotent capture must reuse the frozen record")
	}
	if !strings.Contains(notice.String(), "failure recorded; run dalgona failures") {
		t.Fatalf("notice = %q", notice.String())
	}
	listed, err := store.List(context.Background(), Query{})
	if err != nil {
		t.Fatal(err)
	}
	if len(listed) != 1 || listed[0].ID != record.ID {
		t.Fatalf("list = %+v", listed)
	}
}

func TestCaptureIDConflict(t *testing.T) {
	home := t.TempDir()
	firstStore, err := Open(context.Background(), Options{Home: home, Product: "dalgona", Masker: mapMasker{}})
	if err != nil {
		t.Fatal(err)
	}
	first := testInput()
	first.ID = "0123456789abcdef0123456789abcdef"
	first.Time = time.Date(2026, 11, 1, 12, 0, 0, 0, time.UTC)
	if _, captureErr := firstStore.Capture(context.Background(), first); captureErr != nil {
		t.Fatal(captureErr)
	}
	secondStore, err := Open(context.Background(), Options{Home: home, Product: "dalgona", Masker: mapMasker{}})
	if err != nil {
		t.Fatal(err)
	}
	second := first
	second.Err = errors.New("different failure")
	second.Time = first.Time.Add(time.Second)
	_, err = secondStore.Capture(context.Background(), second)
	if err == nil || err.Error() != "failure: occurrence id conflict" {
		t.Fatalf("err = %v", err)
	}
}

func writeRawRecord(t *testing.T, dir, id string, at time.Time) {
	t.Helper()
	record := Record{SchemaVersion: 1, ID: id, Time: at, Product: "dal", Version: "unknown", OS: "linux", Arch: "amd64", Class: ClassRuntime, Fingerprint: "abc"}
	encoded, err := json.Marshal(record)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "failure-"+id+".json"), encoded, 0o600); err != nil {
		t.Fatal(err)
	}
}

func TestFailureRetention(t *testing.T) {
	now := time.Now().UTC().Truncate(time.Second)
	home := t.TempDir()
	store, err := Open(context.Background(), Options{Home: home, Product: "dal", Masker: mapMasker{}, Now: func() time.Time { return now }})
	if err != nil {
		t.Fatal(err)
	}
	base := now.Add(-10 * 24 * time.Hour)
	seedRetentionHistory(t, store.dir, base)
	input := testInput()
	input.Product = "dal"
	input.Time = base.Add(261 * time.Hour)
	assertRetentionCap(t, store, input)
	assertStaleRecordRotated(t)
}

func seedRetentionHistory(t *testing.T, dir string, base time.Time) {
	t.Helper()
	for i := 0; i < 260; i++ {
		writeRawRecord(t, dir, fmt.Sprintf("%032x", i+1), base.Add(time.Duration(i)*time.Hour))
	}
}

func assertRetentionCap(t *testing.T, store *Store, input Input) {
	t.Helper()
	if _, err := store.Capture(context.Background(), input); err != nil {
		t.Fatal(err)
	}
	summary, err := store.Summary(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if summary.Count != 256 {
		t.Fatalf("count = %d, want 256", summary.Count)
	}
	if _, err := store.List(context.Background(), Query{ID: fmt.Sprintf("%032x", 1)}); err == nil || err.Error() != "failure: record not found" {
		t.Fatalf("err = %v", err)
	}
}

func assertStaleRecordRotated(t *testing.T) {
	t.Helper()
	stale := testStore(t, "dal", nil)
	writeRawRecord(t, stale.dir, strings.Repeat("1", 32), time.Now().Add(-15*24*time.Hour).UTC())
	fresh := testInput()
	fresh.Product = "dal"
	if _, err := stale.Capture(context.Background(), fresh); err != nil {
		t.Fatal(err)
	}
	if _, err := stale.List(context.Background(), Query{ID: strings.Repeat("1", 32)}); err == nil {
		t.Fatal("record older than 14 days should have been rotated away")
	}
}

func TestFailurePrivatePaths(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("unix permission checks")
	}
	assertRejectsWorldReadable(t)
	assertRejectsSymlinkHome(t)
	assertRequiresHome(t)
}

func assertRejectsWorldReadable(t *testing.T) {
	t.Helper()
	home := t.TempDir()
	if _, err := Open(context.Background(), Options{Home: home}); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(filepath.Join(home, "failures"), 0o755); err != nil { //nolint:gosec // test fixtures a non-private directory
		t.Fatal(err)
	}
	if _, err := Open(context.Background(), Options{Home: home}); err == nil || err.Error() != "failure: directory is not private" {
		t.Fatalf("err = %v", err)
	}
}

func assertRejectsSymlinkHome(t *testing.T) {
	t.Helper()
	linkHome := t.TempDir()
	target := filepath.Join(linkHome, "real")
	if err := os.MkdirAll(target, 0o700); err != nil {
		t.Fatal(err)
	}
	link := filepath.Join(linkHome, "link")
	if err := os.Symlink(target, link); err != nil {
		t.Fatal(err)
	}
	if _, err := Open(context.Background(), Options{Home: link}); err == nil || err.Error() != "failure: directory is not private" {
		t.Fatalf("err = %v", err)
	}
}

func assertRequiresHome(t *testing.T) {
	t.Helper()
	if _, err := Open(context.Background(), Options{}); err == nil || err.Error() != "failure: home is empty" {
		t.Fatalf("err = %v", err)
	}
}

func TestRecoverCompletesOwner(t *testing.T) {
	store := testStore(t, "dalgona", nil)
	run := func() (err error) {
		input := testInput()
		defer store.Recover(context.Background(), input, &err)
		panic("kaboom")
	}
	if err := func() (err error) {
		defer func() {
			_ = recover()
		}()
		_ = run()
		return errors.New("resumed")
	}(); err == nil || err.Error() != "resumed" {
		t.Fatalf("panic must not propagate: %v", err)
	}
	records, err := store.List(context.Background(), Query{Limit: 1})
	if err != nil {
		t.Fatal(err)
	}
	if len(records) != 1 || records[0].Class != ClassPanic {
		t.Fatalf("records = %+v", records)
	}
	calm := func() (err error) {
		defer store.Recover(context.Background(), testInput(), &err)
		return nil
	}
	if err := calm(); err != nil {
		t.Fatalf("no panic must mean no action: %v", err)
	}
}

func TestFailureWriteErrorHasNoRawFallback(t *testing.T) {
	var notice strings.Builder
	store := testStore(t, "dalgona", &notice)
	secret := "sk-ant-sekret9876543210ZYXW" //nolint:gosec // synthetic test-only secret
	input := testInput()
	input.Err = errors.New("leak " + secret)
	if err := os.RemoveAll(store.dir); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(store.dir, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	_, err := store.Capture(context.Background(), input)
	if err == nil || !strings.HasPrefix(err.Error(), "failure: write local record:") {
		t.Fatalf("err = %v", err)
	}
	if got := notice.String(); got != "failure log unavailable; details were not written\n" {
		t.Fatalf("notice = %q", got)
	}
	if strings.Contains(notice.String(), secret) {
		t.Fatal("raw secret reached the notice")
	}
}

func TestFailureLocalCorrelationNeverPublished(t *testing.T) {
	store := testStore(t, "dalgona", nil)
	input := testInput()
	record, err := store.Capture(context.Background(), input)
	if err != nil {
		t.Fatal(err)
	}
	assertCorrelationUnpublished(t, store.dir, record)
	assertLocalCorrelation(t, store, record)
	assertLocalFailures(t, store, record)
}

func assertCorrelationUnpublished(t *testing.T, dir string, record Record) {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join(dir, "failure-"+record.ID+".json"))
	if err != nil {
		t.Fatal(err)
	}
	for _, secret := range []string{"sess-1", "turn-1", "call-1"} {
		if strings.Contains(string(raw), secret) {
			t.Fatalf("correlation %q published in %s", secret, raw)
		}
	}
	if text := ReportText(record); strings.Contains(text, "sess-1") || strings.Contains(text, "turn-1") {
		t.Fatalf("correlation published in report text:\n%s", text)
	}
}

func assertLocalCorrelation(t *testing.T, store *Store, record Record) {
	t.Helper()
	local, err := store.Local(context.Background(), record.ID)
	if err != nil {
		t.Fatal(err)
	}
	if local.Correlation.Session != "sess-1" || local.Correlation.CallID != "call-1" {
		t.Fatalf("local = %+v", local)
	}
	if local.RepairedArgsSHA256 == "" || local.OutputSHA256 == "" {
		t.Fatalf("local digests missing: %+v", local)
	}
}

func assertLocalFailures(t *testing.T, store *Store, record Record) {
	t.Helper()
	if _, err := store.Local(context.Background(), "ffffffffffffffffffffffffffffffff"); err == nil || err.Error() != ReasonMissingCorrelation {
		t.Fatalf("err = %v", err)
	}
	if err := os.WriteFile(filepath.Join(store.dir, localFilename(record.ID)), []byte("{broken"), 0o600); err != nil {
		t.Fatal(err)
	}
	fresh, err := Open(context.Background(), Options{Home: filepath.Dir(store.dir), Product: "dalgona", Masker: mapMasker{}})
	if err != nil {
		t.Fatal(err)
	}
	if _, localErr := fresh.Local(context.Background(), record.ID); localErr == nil || localErr.Error() != "failure case unavailable: local metadata is invalid" {
		t.Fatalf("err = %v", localErr)
	}
}
