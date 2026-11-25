package battery

import (
	"context"
	"errors"
	"log/slog"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/failure"
	"github.com/gosuda/dalgona/internal/dalgona/report"
)

type plainMasker struct{}

func (plainMasker) MaskWithError(text string) (string, error) { return text, nil }

func reportableFailure(t *testing.T) failure.Record {
	t.Helper()
	record, err := failure.Build(t.Context(), failure.Input{
		Product: "dalgona", Version: "1.0", Class: failure.ClassTool, Tool: "exec", Err: errors.New("exec tool failed"), Origin: failure.OriginMain,
	}, plainMasker{})
	if err != nil {
		t.Fatal(err)
	}
	return record
}

func TestReloadReporterFollowsConsentWithoutRestart(t *testing.T) {
	home := t.TempDir()
	enabled := false
	d := &Daemon{ReporterOpener: func(ctx context.Context, home string) (*report.Reporter, error) {
		if !enabled || !report.ConsentValid(home, "app") {
			return nil, nil
		}
		return report.Open(ctx, report.Options{Home: home, ConsentMethod: "app", Eligibility: report.Eligibility{Origin: failure.OriginMain}})
	}}
	d.openReporter(t.Context(), home, slog.New(slog.DiscardHandler))
	t.Cleanup(func() { _ = d.closeReporter() })
	queued := func() int { return report.Inspect(home, "app", time.Now()).QueueCount }
	requireReportBeforeConsent(t, d, queued)
	if err := report.WriteConsent(home, "app", time.Now()); err != nil {
		t.Fatal(err)
	}
	enabled = true
	d.ReloadReporter()
	requireReportAfterEnabling(t, d, queued)
	enabled = false
	if err := report.Disable(home); err != nil {
		t.Fatal(err)
	}
	d.ReloadReporter()
	requireReportAfterDisabling(t, d, queued)
}

func requireReportBeforeConsent(t *testing.T, d *Daemon, queued func() int) {
	t.Helper()
	if err := d.ReportFailure(t.Context(), reportableFailure(t)); err != nil || queued() != 0 {
		t.Fatalf("report before consent: err %v, queued %d", err, queued())
	}
}

func requireReportAfterEnabling(t *testing.T, d *Daemon, queued func() int) {
	t.Helper()
	if err := d.ReportFailure(t.Context(), reportableFailure(t)); err != nil || queued() != 1 {
		t.Fatalf("report after enabling: err %v, queued %d", err, queued())
	}
}

func requireReportAfterDisabling(t *testing.T, d *Daemon, queued func() int) {
	t.Helper()
	if err := d.ReportFailure(t.Context(), reportableFailure(t)); err != nil || queued() != 0 {
		t.Fatalf("report after disabling: err %v, queued %d", err, queued())
	}
}

func TestReloadReporterJoinsAdmittedFailureBeforeQueueDeletion(t *testing.T) {
	home := t.TempDir()
	if err := report.WriteConsent(home, "app", time.Now()); err != nil {
		t.Fatal(err)
	}
	arrived, finish := make(chan struct{}), make(chan struct{})
	unblock := sync.OnceFunc(func() { close(finish) })
	pause := sync.OnceFunc(func() {
		close(arrived)
		<-finish
	})
	enabled := true
	d := &Daemon{ReporterOpener: func(ctx context.Context, home string) (*report.Reporter, error) {
		if !enabled {
			return nil, nil
		}
		return report.Open(ctx, report.Options{Home: home, ConsentMethod: "app", Now: func() time.Time {
			pause()
			return time.Now()
		}})
	}}
	d.openReporter(t.Context(), home, slog.New(slog.DiscardHandler))
	t.Cleanup(func() { _ = d.closeReporter() })
	defer unblock()
	record := reportableFailure(t)
	enqueued := make(chan error, 1)
	go func() { enqueued <- d.ReportFailure(t.Context(), record) }()
	<-arrived
	if d.reportMu.TryLock() {
		d.reportMu.Unlock()
		t.Error("reporter shutdown can pass the admission lock before this enqueue finishes")
	}
	enabled = false
	disabled := make(chan error, 1)
	go func() {
		d.ReloadReporter()
		disabled <- report.Disable(home)
	}()
	unblock()
	if err := errors.Join(<-enqueued, <-disabled); err != nil {
		t.Fatal(err)
	}
	if count := report.Inspect(home, "app", time.Now()).QueueCount; count != 0 {
		t.Fatalf("reports queued after disable = %d, want 0", count)
	}
}
