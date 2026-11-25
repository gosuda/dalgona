package main

import (
	"context"
	"encoding/json/jsontext"
	"io"
	"os"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/rpcserver"
	"github.com/gosuda/dalgona/internal/dalgona/report"
)

const (
	methodReportingRead    = "dalgona/reporting/read"
	methodReportingEnable  = "dalgona/reporting/enable"
	methodReportingDisable = "dalgona/reporting/disable"
	reportingConfirmation  = "yes"
	stageConsent           = "consent"
	stageConfig            = "config"
)

var reportingDisabled = jsontext.Value(`{"enabled":false,"queuedReportsDeleted":true,"publishedIssuesDeleted":false}`)

type reportingView struct {
	method     string
	enabled    bool
	available  bool
	inspection report.Inspection
}

type reportingConsent struct {
	inspection report.Inspection
}

type enableRequest struct {
	method       string
	textHash     string
	confirmation string
}

func reportingExtensions() map[string]rpcserver.Extension {
	return map[string]rpcserver.Extension{methodReportingRead: readReporting}
}

func reportingOperations(reloadReporter func()) map[string]rpcserver.Extension {
	var mu sync.Mutex
	serialize := func(change rpcserver.Extension) rpcserver.Extension {
		return func(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
			mu.Lock()
			defer mu.Unlock()
			return change(ctx, params)
		}
	}
	return map[string]rpcserver.Extension{
		methodReportingEnable: serialize(func(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
			result, err := enableReporting(ctx, params)
			if err == nil {
				reloadReporter()
			}
			return result, err
		}),
		methodReportingDisable: serialize(func(context.Context, jsontext.Value) (jsontext.Value, error) {
			return disableReporting(reloadReporter)
		}),
	}
}

func liveReporterOpener(notice io.Writer) func(context.Context, string) (*report.Reporter, error) {
	return func(ctx context.Context, home string) (*report.Reporter, error) {
		cfg, err := config.Load(config.ProductDalgona, "", config.Flags{})
		if err != nil {
			return nil, err
		}
		return report.OpenConsented(ctx, report.OpenParams{
			Home: home, Enabled: cfg.FailureReports.Enabled, Method: cfg.FailureReports.Auth,
			Version: config.Version(), HomeOverridden: os.Getenv("DALGONA_HOME") != "", Notice: notice,
		})
	}
}

func reportingConfig() (config.Config, error) {
	cfg, err := config.Load(config.ProductDalgona, "", config.Flags{})
	if err != nil {
		return config.Config{}, rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorUnavailable, Stage: stageConfig})
	}
	return cfg, nil
}

func readReporting(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	cfg, err := reportingConfig()
	if err != nil {
		return nil, err
	}
	method := cfg.FailureReports.Auth
	err = decodeMembers(params, func(name string, d *jsontext.Decoder) error {
		if name != "method" {
			return d.SkipValue()
		}
		value, readErr := rpc.ReadString(d)
		method = value
		return readErr
	})
	if err != nil || !reportingMethod(method) {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	inspection := report.Inspect(cfg.Home, method, time.Now())
	return rpc.Marshal(reportingView{
		method:     method,
		enabled:    cfg.FailureReports.Enabled && cfg.FailureReports.Auth == method && report.ConsentValid(cfg.Home, method),
		available:  os.Getenv("DALGONA_HOME") == "" && inspection.Authorization != report.AuthorizationUnavailable,
		inspection: inspection,
	})
}

func enableReporting(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	request, err := decodeEnable(params)
	if err != nil {
		return nil, err
	}
	if request.confirmation != reportingConfirmation {
		return nil, rpc.NativeKind(rpc.ErrorPermissionDenied)
	}
	if current := report.ConsentTextHash(request.method); request.textHash != current {
		return nil, rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorStaleRevision, CurrentRevision: current})
	}
	cfg, err := reportingConfig()
	if err != nil {
		return nil, err
	}
	if err := report.WriteConsent(cfg.Home, request.method, time.Now()); err != nil {
		return nil, rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorUnavailable, Stage: stageConsent})
	}
	edits := []config.SettingEdit{{Key: "failure_reports.auth", Value: request.method}, {Key: "failure_reports.enabled", Value: true}}
	if err := config.WriteSettings(cfg.Home, edits...); err != nil {
		return nil, rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorUnavailable, Stage: stageConfig})
	}
	return rpc.Marshal(reportingConsent{inspection: report.Inspect(cfg.Home, request.method, time.Now())})
}

func decodeEnable(params jsontext.Value) (enableRequest, error) {
	var request enableRequest
	err := decodeMembers(params, func(name string, d *jsontext.Decoder) error {
		var readErr error
		switch name {
		case "method":
			request.method, readErr = rpc.ReadString(d)
		case "textHash":
			request.textHash, readErr = rpc.ReadString(d)
		case "confirmation":
			request.confirmation, readErr = rpc.ReadString(d)
		default:
			readErr = d.SkipValue()
		}
		return readErr
	})
	if err != nil || !reportingMethod(request.method) || request.textHash == "" || request.confirmation == "" {
		return request, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return request, nil
}

func disableReporting(reloadReporter func()) (jsontext.Value, error) {
	cfg, err := reportingConfig()
	if err != nil {
		return nil, err
	}
	if err := config.WriteSettings(cfg.Home, config.SettingEdit{Key: "failure_reports.enabled", Value: false}); err != nil {
		return nil, rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorUnavailable, Stage: stageConfig})
	}
	reloadReporter()
	if err := report.Disable(cfg.Home); err != nil {
		return nil, rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorUnavailable, Stage: stageConsent})
	}
	return reportingDisabled, nil
}

func reportingMethod(method string) bool { return method == "app" || method == "gh" }

func (v reportingView) Encode(o *rpc.Writer) {
	o.Open()
	o.Str("method", v.method)
	o.Bool("enabled", v.enabled)
	o.Bool("available", v.available)
	o.Str("consentText", report.ConsentText(v.method))
	o.Str("textHash", report.ConsentTextHash(v.method))
	encodeConsentReceipt(o, v.inspection.Consent)
	o.Str("authorization", v.inspection.Authorization)
	encodeRestriction(o, v.inspection.Restriction)
	o.Int("queueCount", int64(v.inspection.QueueCount))
	o.Int("uncertainCount", int64(v.inspection.UncertainCount))
	o.Close()
}

func (c reportingConsent) Encode(o *rpc.Writer) {
	o.Open()
	encodeConsentReceipt(o, c.inspection.Consent)
	o.Str("authorization", c.inspection.Authorization)
	o.Close()
}

func encodeConsentReceipt(o *rpc.Writer, receipt *report.ConsentReceipt) {
	o.Key("consentReceipt")
	if receipt == nil {
		o.Null()
		return
	}
	o.Open()
	o.Int("schemaVersion", int64(receipt.SchemaVersion))
	o.Str("time", rpc.FormatTimestamp(receipt.Time))
	o.Str("method", receipt.Method)
	o.Str("textHash", receipt.TextHash)
	o.Close()
}

func encodeRestriction(o *rpc.Writer, restriction *report.Restriction) {
	o.Key("restriction")
	if restriction == nil {
		o.Null()
		return
	}
	o.Open()
	o.Str("kind", restriction.Kind)
	if !restriction.Until.IsZero() {
		o.Str("until", rpc.FormatTimestamp(restriction.Until))
	}
	o.Str("message", restriction.Message)
	o.Close()
}
