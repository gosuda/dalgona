package daltoki

import (
	"fmt"
	"net/url"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dalgona/github"
)

type Config struct {
	Enabled                 bool
	AppID                   int64
	PrivateKeyFile          string
	Repositories            []github.Repository
	PollInterval            time.Duration
	WebhookSecret           string
	RelayURL                string
	RelayToken              string
	Concurrency             int
	JobsPerDay              int
	Model                   string
	Profile                 string
	JobTokenLimit           int
	DailyTokenLimit         int
	JobCostLimitUSDMicros   int64
	DailyCostLimitUSDMicros int64
}

func ParseConfig(raw config.DaltokiConfig, unmask func(string) (string, error)) (Config, error) {
	cfg := Config{Enabled: raw.Enabled}
	if !raw.Enabled {
		return cfg, nil
	}
	repositories, err := validateIdentity(raw)
	if err != nil {
		return Config{}, err
	}
	if limitErr := validateLimits(raw); limitErr != nil {
		return Config{}, limitErr
	}
	webhookSecret, err := resolveSecretRef("webhook_secret_ref", raw.WebhookSecretRef, unmask)
	if err != nil {
		return Config{}, err
	}
	relayURL, relayToken, err := resolveRelay(raw.RelayURL, raw.RelayTokenRef, unmask)
	if err != nil {
		return Config{}, err
	}
	return Config{
		Enabled:                 true,
		AppID:                   raw.AppID,
		PrivateKeyFile:          raw.PrivateKeyFile,
		Repositories:            repositories,
		PollInterval:            time.Duration(raw.PollIntervalSeconds) * time.Second,
		WebhookSecret:           webhookSecret,
		RelayURL:                relayURL,
		RelayToken:              relayToken,
		Concurrency:             raw.Concurrency,
		JobsPerDay:              raw.JobsPerDay,
		Model:                   raw.Model,
		Profile:                 raw.Profile,
		JobTokenLimit:           raw.JobTokenLimit,
		DailyTokenLimit:         raw.DailyTokenLimit,
		JobCostLimitUSDMicros:   raw.JobCostLimitUSDMicros,
		DailyCostLimitUSDMicros: raw.DailyCostLimitUSDMicros,
	}, nil
}

func validateIdentity(raw config.DaltokiConfig) ([]github.Repository, error) {
	if raw.AppID <= 0 {
		return nil, fmt.Errorf("daltoki: invalid app_id")
	}
	if raw.PrivateKeyFile == "" {
		return nil, fmt.Errorf("daltoki: invalid private_key_file")
	}
	repositories, err := parseRepositories(raw.Repositories)
	if err != nil {
		return nil, err
	}
	if raw.Model == "" {
		return nil, fmt.Errorf("daltoki: invalid model")
	}
	if raw.Profile == "" {
		return nil, fmt.Errorf("daltoki: invalid profile")
	}
	return repositories, nil
}

func validateLimits(raw config.DaltokiConfig) error {
	if err := validatePolling(raw); err != nil {
		return err
	}
	if err := validateTokenBudgets(raw); err != nil {
		return err
	}
	return validateCostBudgets(raw)
}

func validatePolling(raw config.DaltokiConfig) error {
	if raw.PollIntervalSeconds < 60 || raw.PollIntervalSeconds > 3600 {
		return fmt.Errorf("daltoki: invalid poll_interval_seconds")
	}
	if raw.Concurrency < 1 || raw.Concurrency > 4 {
		return fmt.Errorf("daltoki: invalid concurrency")
	}
	if raw.JobsPerDay < 1 || raw.JobsPerDay > 100 {
		return fmt.Errorf("daltoki: invalid jobs_per_day")
	}
	return nil
}

func validateTokenBudgets(raw config.DaltokiConfig) error {
	if raw.JobTokenLimit < 1000 || raw.JobTokenLimit > 1000000 {
		return fmt.Errorf("daltoki: invalid job_token_limit")
	}
	if raw.DailyTokenLimit < raw.JobTokenLimit || raw.DailyTokenLimit > 10000000 {
		return fmt.Errorf("daltoki: invalid daily_token_limit")
	}
	return nil
}

func validateCostBudgets(raw config.DaltokiConfig) error {
	if raw.JobCostLimitUSDMicros <= 0 {
		return fmt.Errorf("daltoki: invalid job_cost_limit_usd_micros")
	}
	if raw.DailyCostLimitUSDMicros < raw.JobCostLimitUSDMicros {
		return fmt.Errorf("daltoki: invalid daily_cost_limit_usd_micros")
	}
	return nil
}

func parseRepositories(raw []string) ([]github.Repository, error) {
	if len(raw) < 1 || len(raw) > 100 {
		return nil, fmt.Errorf("daltoki: invalid repositories")
	}
	repositories := make([]github.Repository, 0, len(raw))
	seen := make(map[string]bool, len(raw))
	for _, entry := range raw {
		owner, name, ok := strings.Cut(entry, "/")
		if !ok {
			return nil, fmt.Errorf("daltoki: invalid repositories")
		}
		owner = strings.ToLower(owner)
		name = strings.ToLower(name)
		if !validRepositoryPart(owner) || !validRepositoryPart(name) {
			return nil, fmt.Errorf("daltoki: invalid repositories")
		}
		key := owner + "/" + name
		if seen[key] {
			return nil, fmt.Errorf("daltoki: invalid repositories")
		}
		seen[key] = true
		repositories = append(repositories, github.Repository{Owner: owner, Name: name})
	}
	return repositories, nil
}

const repositoryPartChars = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_.-"

func validRepositoryPart(part string) bool {
	if part == "" || part == "." || part == ".." {
		return false
	}
	return strings.Trim(part, repositoryPartChars) == ""
}

func resolveSecretRef(key, value string, unmask func(string) (string, error)) (string, error) {
	if value == "" {
		return "", nil
	}
	if !isSinglePlaceholder(value) {
		return "", fmt.Errorf("daltoki: invalid %s", key)
	}
	if unmask == nil {
		return "", fmt.Errorf("daltoki: invalid %s", key)
	}
	resolved, err := unmask(value)
	if err != nil || resolved == "" || resolved == value {
		return "", fmt.Errorf("daltoki: invalid %s", key)
	}
	return resolved, nil
}

func isSinglePlaceholder(value string) bool {
	if !strings.HasPrefix(value, "{{secret:") || !strings.HasSuffix(value, "}}") {
		return false
	}
	return strings.Count(value, "{{secret:") == 1 && strings.Count(value, "}}") == 1
}

func resolveRelay(rawURL, rawToken string, unmask func(string) (string, error)) (string, string, error) {
	if rawURL == "" && rawToken == "" {
		return "", "", nil
	}
	if rawURL == "" || rawToken == "" {
		return "", "", fmt.Errorf("daltoki: relay configuration is incomplete")
	}
	if err := validateRelayURL(rawURL); err != nil {
		return "", "", err
	}
	token, err := resolveSecretRef("relay_token_ref", rawToken, unmask)
	if err != nil {
		return "", "", err
	}
	return rawURL, token, nil
}

func validateRelayURL(rawURL string) error {
	parsed, err := url.Parse(rawURL)
	if err != nil {
		return fmt.Errorf("daltoki: invalid relay_url")
	}
	if parsed.Scheme != "https" || parsed.Host == "" {
		return fmt.Errorf("daltoki: invalid relay_url")
	}
	if parsed.User != nil || parsed.RawQuery != "" || parsed.ForceQuery || parsed.Fragment != "" {
		return fmt.Errorf("daltoki: invalid relay_url")
	}
	return nil
}
