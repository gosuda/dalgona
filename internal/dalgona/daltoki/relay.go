package daltoki

import (
	"bytes"
	"context"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"
)

const (
	relayPullLimit   = 100
	relayPollPeriod  = 10 * time.Second
	relayPollTimeout = 15 * time.Second
)

type RelayPage struct {
	Items []Envelope `json:"items"`
	Next  string     `json:"next"`
}

type RelayClient struct {
	BaseURL string
	Token   string
	HTTP    *http.Client
}

func NewRelayClient(baseURL, token string, transport *http.Client) (*RelayClient, error) {
	if baseURL == "" || token == "" {
		return nil, errors.New("daltoki: relay configuration is incomplete")
	}
	if transport == nil {
		transport = &http.Client{Timeout: relayPollTimeout}
	}
	return &RelayClient{BaseURL: strings.TrimRight(baseURL, "/"), Token: token, HTTP: transport}, nil
}

func (c *RelayClient) call(ctx context.Context, path string, payload any) ([]byte, int, error) {
	body, err := json.Marshal(payload)
	if err != nil {
		return nil, 0, err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, c.BaseURL+path, bytes.NewReader(body))
	if err != nil {
		return nil, 0, err
	}
	req.Header.Set("Authorization", "Bearer "+c.Token)
	req.Header.Set("Content-Type", "application/json")
	resp, err := c.HTTP.Do(req)
	if err != nil {
		return nil, 0, fmt.Errorf("daltoki: relay %s: %w", path, err)
	}
	defer func() { _ = resp.Body.Close() }()
	data, err := io.ReadAll(io.LimitReader(resp.Body, maxWebhookBody+1))
	if err != nil {
		return nil, resp.StatusCode, fmt.Errorf("daltoki: relay %s: read response: %w", path, err)
	}
	if int64(len(data)) > maxWebhookBody {
		return nil, resp.StatusCode, errors.New("daltoki: relay response too large")
	}
	return data, resp.StatusCode, nil
}

func (c *RelayClient) Pull(ctx context.Context, after string, limit int) (RelayPage, error) {
	var page RelayPage
	if limit <= 0 || limit > relayPullLimit {
		limit = relayPullLimit
	}
	data, status, err := c.call(ctx, "/daltoki/relay/pull", map[string]any{"after": after, "limit": limit})
	if err != nil {
		return page, err
	}
	if status != http.StatusOK {
		return page, fmt.Errorf("daltoki: relay pull: status %d", status)
	}
	if err := json.Unmarshal(data, &page); err != nil {
		return page, fmt.Errorf("daltoki: relay pull: decode response: %w", err)
	}
	return page, nil
}

func (c *RelayClient) Ack(ctx context.Context, through string) error {
	_, status, err := c.call(ctx, "/daltoki/relay/ack", map[string]string{"through": through})
	if err != nil {
		return err
	}
	if status != http.StatusNoContent && status != http.StatusOK {
		return fmt.Errorf("daltoki: relay ack: status %d", status)
	}
	return nil
}

type IntakeStore interface {
	Admit(ctx context.Context, envelope Envelope) (bool, error)
}

type RelayIntake struct {
	Client *RelayClient
	Secret string
	Store  IntakeStore
	Clock  func() time.Time
	Limit  int
}

func NewRelayIntake(client *RelayClient, secret string, store IntakeStore, clock func() time.Time) *RelayIntake {
	if clock == nil {
		clock = time.Now
	}
	return &RelayIntake{Client: client, Secret: secret, Store: store, Clock: clock, Limit: relayPullLimit}
}

func (r *RelayIntake) PollOnce(ctx context.Context, after string) (string, error) {
	page, err := r.Client.Pull(ctx, after, r.Limit)
	if err != nil {
		return after, err
	}
	now := r.Clock()
	handled := after
	for _, envelope := range page.Items {
		advanced, err := r.handleOne(ctx, envelope, now)
		if err != nil {
			if handled != after {
				ackErr := r.Client.Ack(ctx, handled)
				return handled, errors.Join(err, ackErr)
			}
			return handled, err
		}
		handled = advanced
	}
	if handled != "" && handled != after {
		if err := r.Client.Ack(ctx, handled); err != nil {
			return handled, err
		}
	}
	if page.Next != "" {
		return page.Next, nil
	}
	return handled, nil
}

func (r *RelayIntake) handleOne(ctx context.Context, envelope Envelope, now time.Time) (string, error) {
	if err := envelope.Verify(r.Secret, now); err != nil {
		return envelope.DeliveryID, nil
	}
	if _, err := r.Store.Admit(ctx, envelope); err != nil {
		return "", err
	}
	return envelope.DeliveryID, nil
}

func (r *RelayIntake) Run(ctx context.Context, after string) error {
	cursor := after
	ticker := time.NewTicker(relayPollPeriod)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-ticker.C:
			next, err := r.PollOnce(ctx, cursor)
			if next != "" {
				cursor = next
			}
			if err != nil {
				return err
			}
		}
	}
}
