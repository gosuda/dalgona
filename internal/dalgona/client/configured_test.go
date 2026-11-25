package client

import (
	"crypto/tls"
	"log/slog"
	"net/http"
	"os"
	"path/filepath"
	"slices"
	"testing"
)

func configuredHome(t *testing.T, configBody string) string {
	t.Helper()
	home := t.TempDir()
	for name, body := range map[string]string{"config.toml": configBody, "work.token": "relay-token\n"} {
		if err := os.WriteFile(filepath.Join(home, name), []byte(body), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	t.Setenv("DALGONA_HOME", home)
	return home
}

func TestOpenConfiguredListsTheHomeAndEveryConfiguredRemote(t *testing.T) {
	home := configuredHome(t, "[remotes.work]\nkind = \"daemon\"\nurl = \"wss://work.example\"\ntoken_file = \"work.token\"\n\n[remotes.box1]\nservice = \"docker\"\n")
	set, remotes, err := OpenConfigured(Backend{}, newRecorder(), slog.New(slog.DiscardHandler))
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = set.Close() }()
	want := []EndpointStatus{
		{ID: EndpointID(home), Label: "local", Kind: EndpointLocal, Phase: PhaseIdle},
		{ID: "box1", Label: "box1", Kind: EndpointBox, Phase: PhaseIdle},
		{ID: "work", Label: "work", Kind: EndpointDaemon, Phase: PhaseIdle},
	}
	if got := set.Endpoints(); !slices.Equal(got, want) {
		t.Fatalf("endpoints = %+v, want %+v", got, want)
	}
	if spec, err := remotes.Spec("box1"); err != nil || spec.Service != "docker" {
		t.Fatalf("returned remotes resolve box1 to %+v, %v", spec, err)
	}
}

func TestOpenConfiguredReportsTheConfigurationError(t *testing.T) {
	configuredHome(t, "[remotes.work]\nkind = \"daemon\"\nurl = \"http://work.example\"\ntoken_file = \"work.token\"\n")
	_, _, err := OpenConfigured(Backend{}, newRecorder(), slog.New(slog.DiscardHandler))
	want := "config.toml:3: remotes.work.url must be an https:// or wss:// URL with a host and no user info, query, or fragment"
	if err == nil || err.Error() != want {
		t.Fatalf("open with an http daemon url = %v, want %q", err, want)
	}
}

func TestOpenConfiguredTrustsTheCAFileForServiceCalls(t *testing.T) {
	files := writeRelayFiles(t)
	configuredHome(t, "tls_ca_file = '"+files.cert+"'\n")
	original := http.DefaultTransport
	t.Cleanup(func() { http.DefaultTransport = original })
	srv, _ := startRelayServer(t, files, tls.VersionTLS13, func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(http.StatusNoContent) })
	set, _, err := OpenConfigured(Backend{}, newRecorder(), slog.New(slog.DiscardHandler))
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = set.Close() }()
	request, err := http.NewRequestWithContext(t.Context(), http.MethodGet, srv.URL, http.NoBody)
	if err != nil {
		t.Fatal(err)
	}
	resp, err := http.DefaultClient.Do(request)
	if err != nil {
		t.Fatalf("a service call to a server signed by tls_ca_file: %v", err)
	}
	_ = resp.Body.Close()
}
