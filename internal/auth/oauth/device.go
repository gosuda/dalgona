package oauth

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"strings"
	"time"
)

type deviceGrant struct {
	deviceCode string
	userCode   string
	verifyURL  string
	interval   time.Duration
}

type deviceAuthorizationCode struct {
	code     string
	verifier string
	pending  bool
}

func DeviceCode(ctx context.Context, provider Provider, show func(string, string)) (Token, error) {
	if provider.DeviceURL == "" {
		return Token{}, errors.New("provider has no device flow")
	}
	flowContext, cancel := context.WithTimeout(ctx, 15*time.Minute)
	defer cancel()
	if provider.ID == "chatgpt" {
		return chatGPTDeviceCode(flowContext, provider, show)
	}
	return rfcDeviceCode(flowContext, provider, show)
}

func rfcDeviceCode(ctx context.Context, provider Provider, show func(string, string)) (Token, error) {
	response, reqErr := postForm(ctx, provider.DeviceURL, url.Values{
		"client_id": {provider.ClientID}, "scope": {strings.Join(provider.Scopes, " ")},
	})
	grant, err := requestDeviceGrant(provider, response, reqErr, "device_code")
	if err != nil {
		return Token{}, err
	}
	if show != nil {
		show(grant.userCode, grant.verifyURL)
	}
	values := url.Values{
		"client_id": {provider.ClientID}, "device_code": {grant.deviceCode},
		"grant_type": {"urn:ietf:params:oauth:grant-type:device_code"},
	}
	for {
		if waitErr := wait(ctx, grant.interval); waitErr != nil {
			return Token{}, waitErr
		}
		token, code, pollErr := pollRFCDevice(ctx, provider, values)
		if pollErr != nil {
			return Token{}, pollErr
		}
		if code == "" {
			return token, nil
		}
		interval, intervalErr := nextDeviceInterval(code, grant.interval)
		if intervalErr != nil {
			return Token{}, intervalErr
		}
		grant.interval = interval
	}
}

func nextDeviceInterval(code string, current time.Duration) (time.Duration, error) {
	switch code {
	case "authorization_pending":
		return current, nil
	case "slow_down":
		return current + 5*time.Second, nil
	default:
		return 0, fmt.Errorf("device authorization failed: %s", code)
	}
}

func pollRFCDevice(ctx context.Context, provider Provider, values url.Values) (Token, string, error) {
	response, err := postForm(ctx, provider.TokenURL, values)
	if err != nil {
		return Token{}, "", fmt.Errorf("oauth device poll: %w", err)
	}
	decoded, err := response.object()
	if err != nil {
		return Token{}, "", err
	}
	code, err := decoded.string("error")
	if err != nil {
		return Token{}, "", fmt.Errorf("decode oauth device error: %w", err)
	}
	if code != "" {
		return Token{}, code, nil
	}
	if statusErr := response.requireSuccess(); statusErr != nil {
		return Token{}, "", statusErr
	}
	token, err := tokenFromObject(decoded, "")
	return token, "", err
}

func chatGPTDeviceCode(ctx context.Context, provider Provider, show func(string, string)) (Token, error) {
	response, err := postJSON(ctx, provider.DeviceURL, url.Values{"client_id": {provider.ClientID}})
	grant, err := requestDeviceGrant(provider, response, err, "device_auth_id")
	if err != nil {
		return Token{}, err
	}
	if show != nil {
		show(grant.userCode, grant.verifyURL)
	}
	values := url.Values{"device_auth_id": {grant.deviceCode}, "user_code": {grant.userCode}}
	for {
		if waitErr := wait(ctx, grant.interval); waitErr != nil {
			return Token{}, waitErr
		}
		polled, pollErr := pollChatGPTDevice(ctx, provider, values)
		if pollErr != nil {
			return Token{}, pollErr
		}
		if polled.pending {
			continue
		}
		return exchange(ctx, provider, authorization{code: polled.code}, polled.verifier, provider.DeviceRedirectURL)
	}
}

func pollChatGPTDevice(ctx context.Context, provider Provider, values url.Values) (deviceAuthorizationCode, error) {
	response, err := postJSON(ctx, provider.DeviceTokenURL, values)
	if err != nil {
		return deviceAuthorizationCode{}, fmt.Errorf("oauth ChatGPT device poll: %w", err)
	}
	if response.status == http.StatusForbidden || response.status == http.StatusNotFound {
		return deviceAuthorizationCode{pending: true}, nil
	}
	if statusErr := response.requireSuccess(); statusErr != nil {
		return deviceAuthorizationCode{}, statusErr
	}
	decoded, err := response.object()
	if err != nil {
		return deviceAuthorizationCode{}, err
	}
	code, err := decoded.string("authorization_code")
	if err != nil {
		return deviceAuthorizationCode{}, fmt.Errorf("decode oauth device code: %w", err)
	}
	verifier, err := decoded.string("code_verifier")
	if err != nil {
		return deviceAuthorizationCode{}, fmt.Errorf("decode oauth device verifier: %w", err)
	}
	return deviceAuthorizationCode{code: code, verifier: verifier}, nil
}

func requestDeviceGrant(provider Provider, response wireResponse, requestErr error, idField string) (deviceGrant, error) {
	if requestErr != nil {
		return deviceGrant{}, fmt.Errorf("oauth device authorization: %w", requestErr)
	}
	if statusErr := response.requireSuccess(); statusErr != nil {
		return deviceGrant{}, statusErr
	}
	decoded, err := response.object()
	if err != nil {
		return deviceGrant{}, err
	}
	return readDeviceGrant(decoded, provider, idField)
}

func readDeviceGrant(decoded object, provider Provider, idField string) (deviceGrant, error) {
	deviceCode, err := decoded.string(idField)
	if err != nil {
		return deviceGrant{}, fmt.Errorf("decode oauth device identifier: %w", err)
	}
	userCode, err := decoded.firstString("user_code", "usercode")
	if err != nil {
		return deviceGrant{}, fmt.Errorf("decode oauth user code: %w", err)
	}
	verifyURL, err := decoded.firstString("verification_uri", "verification_url", "verification_uri_complete")
	if err != nil {
		return deviceGrant{}, fmt.Errorf("decode oauth verification URL: %w", err)
	}
	seconds, err := decoded.integer("interval")
	if err != nil {
		return deviceGrant{}, fmt.Errorf("decode oauth device interval: %w", err)
	}
	if deviceCode == "" || userCode == "" {
		return deviceGrant{}, errors.New("oauth device response is missing its codes")
	}
	interval := time.Duration(seconds) * time.Second
	if _, present := decoded["interval"]; !present && provider.ID != "chatgpt" {
		interval = 5 * time.Second
	}
	return deviceGrant{
		deviceCode: deviceCode, userCode: userCode, verifyURL: cmp.Or(verifyURL, provider.DeviceVerificationURL),
		interval: interval,
	}, nil
}

func wait(ctx context.Context, delay time.Duration) error {
	timer := time.NewTimer(delay)
	defer timer.Stop()
	select {
	case <-timer.C:
		return nil
	case <-ctx.Done():
		return fmt.Errorf("oauth polling: %w", ctx.Err())
	}
}
