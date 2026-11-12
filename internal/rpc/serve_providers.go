package rpc

import (
	"context"
	"encoding/json/jsontext"
	"slices"

	"github.com/gosuda/dalgona/internal/auth"
)

type AccountProvidersResponse struct {
	Providers []AccountProvider `json:"providers"`
}

type AccountProvider struct {
	ID       string `json:"id"`
	Login    string `json:"login"`
	SignedIn bool   `json:"signedIn"`
}

func (s *server) accountProviders(context.Context, jsontext.Value) (jsontext.Value, error) {
	signedIn := map[string]bool{}
	if store := s.authStore(); store != nil {
		for _, entry := range store.Entries() {
			signedIn[entry.ID] = true
		}
	}
	ids := slices.Sorted(slices.Values(slices.Concat(auth.Providers(), auth.RunnerServices)))
	response := AccountProvidersResponse{Providers: make([]AccountProvider, 0, len(ids))}
	for _, id := range ids {
		response.Providers = append(response.Providers, AccountProvider{ID: id, Login: loginMode(id), SignedIn: signedIn[id]})
	}
	return encode(response)
}

func loginMode(id string) string {
	switch {
	case id == "chatgpt":
		return "browser"
	case slices.Contains(auth.OAuthProviders, id):
		return "terminal"
	}
	return "apiKey"
}

func (r AccountProvidersResponse) write(o *out) {
	o.open()
	o.key("providers")
	o.openArray()
	for _, provider := range r.Providers {
		provider.write(o)
	}
	o.closeArray()
	o.close()
}

func (p AccountProvider) write(o *out) {
	o.open()
	o.str("id", p.ID)
	o.str("login", p.Login)
	o.boolean("signedIn", p.SignedIn)
	o.close()
}
