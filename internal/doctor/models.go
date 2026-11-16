package doctor

import (
	"cmp"
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/pem"
	"fmt"
	"net/http"
	"os"
	"time"

	"github.com/gosuda/dalgona/internal/dal/auth/oauth"
	"github.com/gosuda/dalgona/internal/dal/model"
	"github.com/gosuda/dalgona/internal/dal/model/anthropic"
	"github.com/gosuda/dalgona/internal/dal/model/antigravity"
	"github.com/gosuda/dalgona/internal/dal/model/chatcompat"
	"github.com/gosuda/dalgona/internal/dal/model/chatgpt"
	"github.com/gosuda/dalgona/internal/dal/model/cursor"
	"github.com/gosuda/dalgona/internal/dal/model/devin"
	"github.com/gosuda/dalgona/internal/dal/model/google"
	"github.com/gosuda/dalgona/internal/dal/model/openai"
)

const modelsTimeout = 5 * time.Second

func (in *inspector) checkModels(ctx context.Context) []row {
	client := in.httpClient()
	rows := make([]row, 0, len(in.configured))
	for _, item := range in.configured {
		rows = append(rows, in.listModels(ctx, item, client))
	}
	return rows
}

func (in *inspector) listModels(ctx context.Context, item configured, client *http.Client) row {
	provider, found := in.newProvider(item, client)
	if !found {
		return warnRow("provider-models", item.id+": not configured; no request made")
	}
	callCtx, cancel := context.WithTimeout(ctx, in.timeout)
	defer cancel()
	models, err := provider.Models(callCtx)
	if err != nil {
		return failRow("provider-models", item.id+": "+err.Error())
	}
	return okRow("provider-models", fmt.Sprintf("%s: %d models", item.id, len(models)))
}

func (in *inspector) newProvider(item configured, client *http.Client) (model.Provider, bool) {
	if spec, custom := in.cfg.Providers[item.id]; custom {
		return chatcompat.New(chatcompat.Config{
			BaseURL: spec.BaseURL, APIKey: apiKeyOf(item), APIKeyEnv: spec.APIKeyEnv, Client: client,
		}), true
	}
	build, found := builtin(item, client)[item.id]
	if !found {
		return nil, false
	}
	return build(), true
}

func apiKeyOf(item configured) func(context.Context) (string, error) {
	return func(context.Context) (string, error) { return cmp.Or(item.cred.Key, item.cred.Access), nil }
}

func builtin(item configured, client *http.Client) map[string]func() model.Provider {
	key := apiKeyOf(item)
	access := func(context.Context) (string, error) { return item.cred.Access, nil }
	token := func(context.Context) (oauth.Token, error) {
		return oauth.Token{Access: cmp.Or(item.cred.Access, item.cred.Key), Account: item.cred.Account}, nil
	}
	return map[string]func() model.Provider{
		"openai":      func() model.Provider { return openai.New(openai.Config{APIKey: key, Client: client}) },
		"anthropic":   func() model.Provider { return anthropic.New(anthropic.Config{APIKey: key, Client: client}) },
		"google":      func() model.Provider { return google.New(google.Config{APIKey: key, Client: client}) },
		"chatgpt":     func() model.Provider { return chatgpt.New(chatgpt.Config{Token: token, Client: client}) },
		"antigravity": func() model.Provider { return antigravity.New(antigravity.Config{Token: access, Client: client}) },
		"cursor":      func() model.Provider { return cursor.New(cursor.Config{Token: access, Client: client}) },
		"devin":       func() model.Provider { return devin.New(devin.Config{APIKey: key, Client: client}) },
	}
}

func (in *inspector) httpClient() *http.Client {
	if in.cfg.TLSCAFile == "" {
		return nil
	}
	roots, err := loadRoots(in.cfg.TLSCAFile)
	if err != nil {
		return nil
	}
	base, ok := http.DefaultTransport.(*http.Transport)
	if !ok {
		return nil
	}
	transport := base.Clone()
	if transport.TLSClientConfig == nil {
		transport.TLSClientConfig = &tls.Config{}
	}
	transport.TLSClientConfig.RootCAs = roots
	return &http.Client{Transport: transport}
}

func loadRoots(path string) (*x509.CertPool, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("tls_ca_file: %w", err)
	}
	roots, err := x509.SystemCertPool()
	if err != nil {
		return nil, fmt.Errorf("tls_ca_file: system cert pool: %w", err)
	}
	roots.AppendCertsFromPEM(data)
	return roots, nil
}

func certificateCount(path string) (int, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return 0, fmt.Errorf("tls_ca_file: %w", err)
	}
	count := 0
	for block, rest := pem.Decode(data); block != nil; block, rest = pem.Decode(rest) {
		if block.Type != "CERTIFICATE" {
			continue
		}
		if _, parseErr := x509.ParseCertificate(block.Bytes); parseErr == nil {
			count++
		}
	}
	if count == 0 {
		return 0, fmt.Errorf("tls_ca_file %s: holds no PEM certificate", path)
	}
	return count, nil
}
