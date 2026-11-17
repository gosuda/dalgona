package report

import (
	"context"
	"fmt"
	"io"
	"time"

	"github.com/gosuda/dalgona/internal/dal/failure"
)

type OpenParams struct {
	Home           string
	Method         string
	Version        string
	Enabled        bool
	HomeOverridden bool
	Test           bool
	Notice         io.Writer
	Now            func() time.Time
	NewGHSource    func() CredentialSource
	NewAppSource   func(home string, now func() time.Time) (CredentialSource, error)
	NewClient      func(version string, source CredentialSource) (GitHub, error)
}

func OpenConsented(ctx context.Context, params OpenParams) (*Reporter, error) {
	notice := params.Notice
	if notice == nil {
		notice = io.Discard
	}
	if params.Test || params.HomeOverridden {
		fmt.Fprintln(notice, "automatic failure reporting is off for an overridden home")
		return nil, nil
	}
	if !params.Enabled || !consentGateOpen(params, notice) {
		return nil, nil
	}
	source, err := buildSource(params, notice)
	if err != nil || source == nil {
		return nil, err
	}
	return openWithSource(ctx, params, source, notice)
}

func openWithSource(ctx context.Context, params OpenParams, source CredentialSource, notice io.Writer) (*Reporter, error) {
	newClient := params.NewClient
	if newClient == nil {
		newClient = func(version string, source CredentialSource) (GitHub, error) {
			return NewReporterClient("https://api.github.com", nil, version, source)
		}
	}
	client, err := newClient(params.Version, source)
	if err != nil {
		return nil, err
	}
	return Open(ctx, Options{
		Home:          params.Home,
		Client:        client,
		Credentials:   source,
		Eligibility:   Eligibility{Origin: failure.OriginMain, Test: params.Test, HomeOverridden: params.HomeOverridden},
		Now:           params.Now,
		Notice:        notice,
		ConsentMethod: params.Method,
	})
}

func consentGateOpen(params OpenParams, notice io.Writer) bool {
	switch {
	case !ConsentStored(params.Home):
		fmt.Fprintln(notice, "automatic failure reporting is off: consent is required")
	case !ConsentValid(params.Home, params.Method):
		fmt.Fprintln(notice, "automatic failure reporting is off: authentication changed; run dalgona failures reporting on")
	default:
		return true
	}
	return false
}

func buildSource(params OpenParams, notice io.Writer) (CredentialSource, error) {
	if params.Method == consentMethodGH {
		if params.NewGHSource != nil {
			return params.NewGHSource(), nil
		}
		return NewGHSource(), nil
	}
	if reportAppClientID == "" {
		fmt.Fprintln(notice, "failure reporting is unavailable: github app client id is missing")
		return nil, nil
	}
	if params.NewAppSource != nil {
		return params.NewAppSource(params.Home, params.Now)
	}
	return NewAppSource(params.Home, params.Now)
}
