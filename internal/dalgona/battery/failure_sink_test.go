package battery

import (
	"context"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/app"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/failure"
	"github.com/gosuda/dalgona/internal/dalgona/report"
)

func TestLoadedFailureSinkQueuesConsentedReports(t *testing.T) {
	home := t.TempDir()
	t.Setenv("DALGONA_HOME", home)
	t.Setenv("HOME", t.TempDir())
	cfg, err := config.Load(config.ProductDalgona, t.TempDir(), config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	if consentErr := report.WriteConsent(home, "app", time.Now()); consentErr != nil {
		t.Fatal(consentErr)
	}
	d := &Daemon{ReporterOpener: func(ctx context.Context, home string) (*report.Reporter, error) {
		return report.Open(ctx, report.Options{Home: home, ConsentMethod: "app", Eligibility: report.Eligibility{Origin: failure.OriginMain}})
	}}
	extra, err := d.Load(t.Context(), app.Base{Config: cfg})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = extra.Close() })
	if extra.FailureSink == nil {
		t.Fatal("dalgona batteries pass no failure sink")
	}
	extra.FailureSink(reportableFailure(t))
	if queued := report.Inspect(home, "app", time.Now()).QueueCount; queued != 1 {
		t.Fatalf("queued reports = %d, want 1", queued)
	}
}
