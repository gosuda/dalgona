package oauth_test

import (
	"crypto/rand"
	"net/http"
	"net/http/httptest"
	"testing"
)

func scopedServer(t *testing.T, handler http.Handler) *httptest.Server {
	t.Helper()
	prefix := "/" + rand.Text()
	scoped := http.StripPrefix(prefix, handler)
	mux := http.NewServeMux()
	mux.Handle(prefix, scoped)
	mux.Handle(prefix+"/", scoped)
	server := httptest.NewServer(mux)
	server.URL += prefix
	return server
}
