package oauth_test

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/auth/oauth"
)

func TestDeviceCodePendingThenSuccess(t *testing.T) {
	var polls atomic.Int32
	server := scopedServer(t, pendingDeviceHandler(t, &polls))
	defer server.Close()
	provider := oauth.Provider{ID: "test", DeviceURL: server.URL + "/device", TokenURL: server.URL + "/token", ClientID: "client"}
	var shownCode, shownURL string
	token, err := oauth.DeviceCode(context.Background(), provider, func(userCode, verifyURL string) {
		shownCode, shownURL = userCode, verifyURL
	})
	if err != nil {
		t.Fatalf("device flow: %v", err)
	}
	if polls.Load() != 2 {
		t.Fatalf("polls = %d", polls.Load())
	}
	if shownCode != "user" || shownURL != "https://example.test/verify" {
		t.Fatalf("show arguments = %q, %q", shownCode, shownURL)
	}
	if token.Access != "device-access" || token.Refresh != "device-refresh" {
		t.Fatalf("token = %+v", token)
	}
}

func pendingDeviceHandler(t *testing.T, polls *atomic.Int32) http.HandlerFunc {
	t.Helper()
	return func(writer http.ResponseWriter, request *http.Request) {
		writer.Header().Set("Content-Type", "application/json")
		switch request.URL.Path {
		case "/device":
			checkDeviceClient(t, request)
			_, _ = io.WriteString(writer, `{"device_code":"device","user_code":"user","verification_uri":"https://example.test/verify","interval":0}`)
		case "/token":
			if polls.Add(1) == 1 {
				writer.WriteHeader(http.StatusBadRequest)
				_, _ = io.WriteString(writer, `{"error":"authorization_pending"}`)
				return
			}
			_, _ = io.WriteString(writer, `{"access_token":"device-access","refresh_token":"device-refresh"}`)
		default:
			writer.WriteHeader(http.StatusNotFound)
		}
	}
}

func checkDeviceClient(t *testing.T, request *http.Request) {
	t.Helper()
	if err := request.ParseForm(); err != nil {
		t.Errorf("parse device form: %v", err)
	}
	if request.Form.Get("client_id") != "client" {
		t.Errorf("client_id = %q", request.Form.Get("client_id"))
	}
}

func TestDeviceCodeSlowDownRaisesInterval(t *testing.T) {
	var polls atomic.Int32
	server := scopedServer(t, http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		writer.Header().Set("Content-Type", "application/json")
		if request.URL.Path == "/device" {
			_, _ = io.WriteString(writer, `{"device_code":"device","user_code":"user","interval":0}`)
			return
		}
		if polls.Add(1) == 1 {
			writer.WriteHeader(http.StatusBadRequest)
			_, _ = io.WriteString(writer, `{"error":"slow_down"}`)
			return
		}
		_, _ = io.WriteString(writer, `{"access_token":"after-slow-down"}`)
	}))
	defer server.Close()
	provider := oauth.Provider{ID: "test", DeviceURL: server.URL + "/device", TokenURL: server.URL + "/token"}
	started := time.Now()
	token, err := oauth.DeviceCode(context.Background(), provider, nil)
	elapsed := time.Since(started)
	if err != nil {
		t.Fatalf("slow_down flow: %v", err)
	}
	if elapsed < 4500*time.Millisecond {
		t.Fatalf("slow_down did not add five seconds: %v", elapsed)
	}
	if token.Access != "after-slow-down" || polls.Load() != 2 {
		t.Fatalf("token = %+v, polls = %d", token, polls.Load())
	}
}

func TestChatGPTDeviceCodeUsesDeviceEndpoints(t *testing.T) {
	var devicePolls atomic.Int32
	server := scopedServer(t, chatGPTDeviceHandler(t, &devicePolls))
	defer server.Close()
	provider := oauth.Provider{
		ID: "chatgpt", ClientID: "client", DeviceURL: server.URL + "/usercode",
		DeviceTokenURL: server.URL + "/device-token", DeviceVerificationURL: "https://example.test/verify",
		DeviceRedirectURL: "https://example.test/device-callback", TokenURL: server.URL + "/oauth-token",
	}
	var verifyURL string
	token, err := oauth.DeviceCode(context.Background(), provider, func(_, verification string) {
		verifyURL = verification
	})
	if err != nil {
		t.Fatalf("ChatGPT device flow: %v", err)
	}
	if token.Access != "chatgpt-device-access" || devicePolls.Load() != 2 {
		t.Fatalf("token = %+v, polls = %d", token, devicePolls.Load())
	}
	if verifyURL != "https://example.test/verify" {
		t.Fatalf("verification URL = %q", verifyURL)
	}
}

func chatGPTDeviceHandler(t *testing.T, polls *atomic.Int32) http.HandlerFunc {
	t.Helper()
	return func(writer http.ResponseWriter, request *http.Request) {
		writer.Header().Set("Content-Type", "application/json")
		switch request.URL.Path {
		case "/usercode":
			chatGPTUserCode(t, writer, request)
		case "/device-token":
			chatGPTDevicePoll(t, writer, request, polls)
		case "/oauth-token":
			chatGPTOAuthToken(t, writer, request)
		default:
			writer.WriteHeader(http.StatusNotFound)
		}
	}
}

func chatGPTUserCode(t *testing.T, writer http.ResponseWriter, request *http.Request) {
	t.Helper()
	body, readErr := io.ReadAll(request.Body)
	if readErr != nil {
		t.Errorf("read usercode body: %v", readErr)
	}
	if !strings.Contains(string(body), `"client_id":"client"`) {
		t.Errorf("usercode body = %s", body)
	}
	_, _ = io.WriteString(writer, `{"device_auth_id":"device-id","user_code":"user-code","interval":"0"}`)
}

func chatGPTDevicePoll(t *testing.T, writer http.ResponseWriter, request *http.Request, polls *atomic.Int32) {
	t.Helper()
	body, readErr := io.ReadAll(request.Body)
	if readErr != nil {
		t.Errorf("read device poll body: %v", readErr)
	}
	text := string(body)
	if !strings.Contains(text, `"device_auth_id":"device-id"`) || !strings.Contains(text, `"user_code":"user-code"`) {
		t.Errorf("device poll body = %s", body)
	}
	if polls.Add(1) == 1 {
		writer.WriteHeader(http.StatusNotFound)
		return
	}
	_, _ = io.WriteString(writer, `{"authorization_code":"code","code_challenge":"challenge","code_verifier":"verifier"}`)
}

func chatGPTOAuthToken(t *testing.T, writer http.ResponseWriter, request *http.Request) {
	t.Helper()
	if err := request.ParseForm(); err != nil {
		t.Errorf("parse code exchange: %v", err)
	}
	if request.Form.Get("code") != "code" || request.Form.Get("code_verifier") != "verifier" {
		t.Errorf("code exchange = %v", request.Form)
	}
	if request.Form.Get("redirect_uri") != "https://example.test/device-callback" {
		t.Errorf("device redirect = %q", request.Form.Get("redirect_uri"))
	}
	_, _ = io.WriteString(writer, `{"access_token":"chatgpt-device-access"}`)
}

func TestDeviceCodeCancellationStopsPolling(t *testing.T) {
	var polls atomic.Int32
	server := scopedServer(t, http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		writer.Header().Set("Content-Type", "application/json")
		if request.URL.Path == "/device" {
			_, _ = io.WriteString(writer, `{"device_code":"device","user_code":"user","interval":10}`)
			return
		}
		polls.Add(1)
		_, _ = fmt.Fprint(writer, `{"access_token":"unexpected"}`)
	}))
	defer server.Close()
	ctx, cancel := context.WithCancel(context.Background())
	started := time.Now()
	_, err := oauth.DeviceCode(ctx, oauth.Provider{
		ID: "test", DeviceURL: server.URL + "/device", TokenURL: server.URL + "/token",
	}, func(string, string) { cancel() })
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("cancellation error = %v", err)
	}
	if time.Since(started) > time.Second {
		t.Fatalf("cancellation did not interrupt the poll wait: %v", time.Since(started))
	}
	if polls.Load() != 0 {
		t.Fatalf("canceled flow polled %d times", polls.Load())
	}
}
