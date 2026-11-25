package report

import (
	"crypto/sha256"
	"encoding/hex"
	json "encoding/json/v2"
	"os"
	"path/filepath"
	"runtime"
	"testing"
	"time"
)

func writeReporterFile(t *testing.T, home, name string, value any) {
	t.Helper()
	dir := filepath.Join(home, reportDirName)
	if err := ensureReporterDir(dir); err != nil {
		t.Fatal(err)
	}
	data, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	if err := atomicWriteFile(dir, name, data); err != nil {
		t.Fatal(err)
	}
}

func TestInspectFreshHome(t *testing.T) {
	got := Inspect(t.TempDir(), "app", time.Now())
	if got.Consent != nil || got.Restriction != nil || got.QueueCount != 0 || got.UncertainCount != 0 {
		t.Fatalf("fresh home inspection = %+v", got)
	}
	if got.Authorization != AuthorizationUnavailable {
		t.Fatalf("app authorization without a client id = %q, want unavailable", got.Authorization)
	}
}

func TestInspectReturnsStoredConsentReceipt(t *testing.T) {
	home := t.TempDir()
	at := time.Date(2026, 11, 20, 8, 30, 0, 0, time.FixedZone("KST", 9*60*60))
	if err := WriteConsent(home, "gh", at); err != nil {
		t.Fatal(err)
	}
	got := Inspect(home, "app", at)
	sum := sha256.Sum256([]byte(ConsentText("gh")))
	want := ConsentReceipt{SchemaVersion: 1, Time: at.UTC(), Method: "gh", TextHash: hex.EncodeToString(sum[:])}
	if got.Consent == nil || *got.Consent != want {
		t.Fatalf("consent receipt = %+v, want %+v", got.Consent, want)
	}
}

func TestInspectCountsQueueAndUncertainReceipts(t *testing.T) {
	home := t.TempDir()
	writeReporterFile(t, home, queueFileName, queueFile{Entries: []QueueEntry{{Fingerprint: "a"}, {Fingerprint: "b"}}})
	writeReporterFile(t, home, stateFileName, reporterState{Receipts: []Receipt{
		{Fingerprint: "a", State: receiptUncertain}, {Fingerprint: "c", State: receiptFiled}, {Fingerprint: "d", State: receiptUncertain},
	}})
	got := Inspect(home, "app", time.Now())
	if got.QueueCount != 2 || got.UncertainCount != 2 {
		t.Fatalf("queue %d uncertain %d, want 2 and 2", got.QueueCount, got.UncertainCount)
	}
}

func TestInspectRestrictions(t *testing.T) {
	now := time.Date(2026, 11, 24, 10, 0, 0, 0, time.UTC)
	retry := now.Add(90 * time.Second)
	cases := []struct {
		name  string
		state reporterState
		want  *Restriction
	}{
		{"stop latch", reporterState{StoppedStatus: 403, CoreRetryAfter: retry}, &Restriction{
			Kind: RestrictionStopped, Message: "failure reporting stopped after HTTP 403; review access before enabling again",
		}},
		{"repository latch", reporterState{StoppedStatus: 404}, &Restriction{
			Kind: RestrictionStopped, Message: "failure reporting stopped after HTTP 404; check repository access and issue policy",
		}},
		{"core retry after", reporterState{CoreRetryAfter: retry}, &Restriction{
			Kind: RestrictionRateLimited, Until: retry, Message: rateLimitedMessage,
		}},
		{"search bucket empty", reporterState{SearchReset: retry}, &Restriction{
			Kind: RestrictionRateLimited, Until: retry.Add(time.Second), Message: rateLimitedMessage,
		}},
		{"daily rest cap", reporterState{RestsDay: "2026-11-24", Rests: 20}, &Restriction{
			Kind: RestrictionLocalLimit, Until: time.Date(2026, 11, 25, 0, 0, 0, 0, time.UTC), Message: "failure reporting paused: local limit reached",
		}},
		{"daily create cap", reporterState{CreatesDay: "2026-11-24", Creates: 3}, &Restriction{
			Kind: RestrictionLocalLimit, Until: time.Date(2026, 11, 25, 0, 0, 0, 0, time.UTC), Message: "failure reporting paused: local limit reached",
		}},
		{"elapsed retry after", reporterState{CoreRetryAfter: now.Add(-time.Second), CoreRemaining: 5}, nil},
		{"cap of an earlier day", reporterState{RestsDay: "2026-11-23", Rests: 20}, nil},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			home := t.TempDir()
			writeReporterFile(t, home, stateFileName, tc.state)
			got := Inspect(home, "app", now)
			if (got.Restriction == nil) != (tc.want == nil) || got.Restriction != nil && *got.Restriction != *tc.want {
				t.Fatalf("restriction = %+v, want %+v", got.Restriction, tc.want)
			}
		})
	}
}

func TestInspectReportsDamagedStateAsStopped(t *testing.T) {
	home := t.TempDir()
	writeReporterFile(t, home, queueFileName, queueFile{Entries: []QueueEntry{{Fingerprint: "a"}}})
	if err := atomicWriteFile(filepath.Join(home, reportDirName), stateFileName, []byte("{")); err != nil {
		t.Fatal(err)
	}
	if err := WriteConsent(home, "app", time.Now()); err != nil {
		t.Fatal(err)
	}
	got := Inspect(home, "app", time.Now())
	want := Restriction{Kind: RestrictionStopped, Message: "failure reporting stopped: state is invalid"}
	if got.Restriction == nil || *got.Restriction != want || got.Consent == nil {
		t.Fatalf("damaged state inspection = %+v, want restriction %+v and the stored consent", got, want)
	}
}

func TestAppAuthorizationFollowsStoredCredential(t *testing.T) {
	now := time.Date(2026, 11, 24, 10, 0, 0, 0, time.UTC)
	home := t.TempDir()
	if got := appAuthorization(home, now); got != AuthorizationMissing {
		t.Fatalf("no credential = %q, want missing", got)
	}
	if err := writeCredential(home, "token", now.Add(-time.Minute)); err != nil {
		t.Fatal(err)
	}
	if got := appAuthorization(home, now); got != AuthorizationExpired {
		t.Fatalf("expired credential = %q, want expired", got)
	}
	if err := writeCredential(home, "token", now.Add(time.Hour)); err != nil {
		t.Fatal(err)
	}
	if got := appAuthorization(home, now); got != AuthorizationReady {
		t.Fatalf("live credential = %q, want ready", got)
	}
}

func TestGHAuthorizationNeedsTheExecutable(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("the fake gh executable is a unix file")
	}
	bin := t.TempDir()
	t.Setenv("PATH", bin)
	got := Inspect(t.TempDir(), "gh", time.Now())
	if got.Authorization != AuthorizationUnavailable {
		t.Fatalf("gh without executable = %q, want unavailable", got.Authorization)
	}
	if err := os.WriteFile(filepath.Join(bin, "gh"), []byte("#!/bin/sh\ntouch \"$0.ran\"\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	got = Inspect(t.TempDir(), "gh", time.Now())
	if got.Authorization != AuthorizationReady {
		t.Fatalf("gh with executable = %q, want ready", got.Authorization)
	}
	if _, err := os.Stat(filepath.Join(bin, "gh.ran")); err == nil {
		t.Fatal("inspection ran gh")
	}
}
