package antigravity

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"slices"
	"strconv"
	"time"

	json "encoding/json/v2"

	"github.com/gosuda/dalgona/internal/model"
)

const (
	freeTier          = "free-tier"
	onboardTimeout    = 30 * time.Second
	onboardPollPeriod = time.Second
)

type projectCache struct {
	turn chan struct{}
	id   string
}

func newProjectCache(id string) *projectCache {
	return &projectCache{turn: make(chan struct{}, 1), id: id}
}

type tier struct {
	ID string `json:"id"`
}

type ineligibleTier struct {
	TierID        string `json:"tierId"`
	Reason        string `json:"reasonMessage"`
	ValidationURL string `json:"validationUrl"`
}

type loadResponse struct {
	CurrentTier     *tier            `json:"currentTier"`
	PaidTier        *tier            `json:"paidTier"`
	AllowedTiers    []tier           `json:"allowedTiers"`
	IneligibleTiers []ineligibleTier `json:"ineligibleTiers"`
	Project         string           `json:"cloudaicompanionProject"`
}

type operationError struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
}

type operationResponse struct {
	Project string `json:"cloudaicompanionProject"`
}

type operation struct {
	Name     string             `json:"name"`
	Done     bool               `json:"done"`
	Error    *operationError    `json:"error"`
	Response *operationResponse `json:"response"`
}

func (p *Provider) Project(ctx context.Context) (string, error) {
	select {
	case p.project.turn <- struct{}{}:
	case <-ctx.Done():
		return "", fmt.Errorf("antigravity: project: %w", ctx.Err())
	}
	defer func() { <-p.project.turn }()
	if p.project.id != "" {
		return p.project.id, nil
	}
	token, err := p.token(ctx)
	if err != nil {
		return "", err
	}
	id, err := p.discoverProject(ctx, token)
	if err != nil {
		return "", err
	}
	p.project.id = id
	return id, nil
}

func (p *Provider) discoverProject(ctx context.Context, token string) (string, error) {
	initial, err := p.loadCodeAssist(ctx, token)
	if err != nil {
		return "", err
	}
	if eligibility := freeTierEligible(initial); eligibility != nil {
		return "", eligibility
	}
	if initial.CurrentTier == nil {
		if onboardErr := p.onboard(ctx, token); onboardErr != nil {
			return "", onboardErr
		}
	}
	refreshed, err := p.loadCodeAssist(ctx, token)
	if err != nil {
		return "", err
	}
	if refreshed.Project == "" {
		return "", errors.New("antigravity: loadCodeAssist did not return a cloudaicompanionProject")
	}
	return refreshed.Project, nil
}

func freeTierEligible(payload loadResponse) error {
	if slices.ContainsFunc(payload.AllowedTiers, func(item tier) bool { return item.ID == freeTier }) {
		return nil
	}
	index := slices.IndexFunc(payload.IneligibleTiers, func(item ineligibleTier) bool { return item.TierID == freeTier })
	if index < 0 || payload.IneligibleTiers[index].Reason == "" {
		return nil
	}
	ineligible := payload.IneligibleTiers[index]
	message := "antigravity: " + ineligible.Reason
	if ineligible.ValidationURL != "" {
		message += "\n" + ineligible.ValidationURL
	}
	return errors.New(message)
}

func (p *Provider) loadCodeAssist(ctx context.Context, token string) (loadResponse, error) {
	payload, err := p.postLoad(ctx, token, "")
	if err != nil || payload.PaidTier != nil || payload.Project == "" {
		return payload, err
	}
	return p.postLoad(ctx, token, payload.Project)
}

func (p *Provider) postLoad(ctx context.Context, token, project string) (loadResponse, error) {
	var buffer bytes.Buffer
	encoder := model.NewJSONEncoder(&buffer)
	encoder.BeginObject()
	if project != "" {
		encoder.StringKey("cloudaicompanionProject", project)
	}
	encodeMetadata(encoder)
	encoder.EndObject()
	if err := encoder.Err(); err != nil {
		return loadResponse{}, fmt.Errorf("antigravity: encode loadCodeAssist: %w", err)
	}
	var payload loadResponse
	err := p.control(ctx, controlCall{token: token, method: http.MethodPost, path: "/v1internal:loadCodeAssist", body: buffer.Bytes()}, &payload)
	return payload, err
}

func encodeMetadata(encoder *model.JSONEncoder) {
	encoder.Key("metadata")
	encoder.BeginObject()
	encoder.StringKey("ideType", "ANTIGRAVITY")
	encoder.EndObject()
}

func (p *Provider) onboard(ctx context.Context, token string) error {
	ctx, cancel := context.WithTimeoutCause(ctx, onboardTimeout, errors.New("antigravity: onboardUser timed out after 30s"))
	defer cancel()
	var buffer bytes.Buffer
	encoder := model.NewJSONEncoder(&buffer)
	encoder.BeginObject()
	encoder.StringKey("tierId", freeTier)
	encodeMetadata(encoder)
	encoder.EndObject()
	if err := encoder.Err(); err != nil {
		return fmt.Errorf("antigravity: encode onboardUser: %w", err)
	}
	var current operation
	if err := p.control(ctx, controlCall{token: token, method: http.MethodPost, path: "/v1internal:onboardUser", body: buffer.Bytes()}, &current); err != nil {
		return err
	}
	for !current.Done {
		next, err := p.pollOperation(ctx, token, current.Name)
		if err != nil {
			return err
		}
		current = next
	}
	return operationResult(current)
}

func (p *Provider) pollOperation(ctx context.Context, token, name string) (operation, error) {
	if name == "" {
		return operation{}, errors.New("antigravity: onboardUser returned an operation without a name")
	}
	timer := time.NewTimer(onboardPollPeriod)
	defer timer.Stop()
	select {
	case <-timer.C:
	case <-ctx.Done():
		return operation{}, context.Cause(ctx)
	}
	var next operation
	err := p.control(ctx, controlCall{token: token, method: http.MethodGet, path: "/v1internal/" + name}, &next)
	return next, err
}

func operationResult(done operation) error {
	if done.Error != nil {
		return errors.New("antigravity: onboardUser operation failed: " + strconv.Itoa(done.Error.Code) + ": " + done.Error.Message)
	}
	if done.Response == nil {
		return errors.New("antigravity: onboardUser operation returned no response")
	}
	return nil
}

type controlCall struct {
	token, method, path string
	body                []byte
}

type controlError struct {
	status int
	err    error
}

func (e *controlError) Error() string { return e.err.Error() }

func controlStatus(err error) int {
	if failure, ok := errors.AsType[*controlError](err); ok {
		return failure.status
	}
	return 0
}

func (p *Provider) control(ctx context.Context, call controlCall, out any) error {
	httpReq, err := p.newRequest(ctx, call.method, call.path, call.body, call.token)
	if err != nil {
		return err
	}
	resp, err := p.client.Do(httpReq)
	if err != nil {
		return fmt.Errorf("antigravity: %s: %w", call.path, err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		raw, _ := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
		return &controlError{
			status: resp.StatusCode,
			err:    fmt.Errorf("antigravity: %s: http %d: %s", call.path, resp.StatusCode, bytes.TrimSpace(raw)),
		}
	}
	if err := json.UnmarshalRead(resp.Body, out); err != nil {
		return fmt.Errorf("antigravity: decode %s: %w", call.path, err)
	}
	return nil
}
