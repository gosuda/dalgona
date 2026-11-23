package rpcserver

import (
	"context"
	"encoding/json/jsontext"
	"slices"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/auth"
)

func (s *server) accountProviders(context.Context, jsontext.Value) (jsontext.Value, error) {
	signedIn := map[string]bool{}
	if store := s.authStore(); store != nil {
		for _, entry := range store.Entries() {
			signedIn[entry.ID] = true
		}
	}
	ids := slices.Sorted(slices.Values(slices.Concat(auth.Providers(), auth.RunnerServices())))
	response := rpc.AccountProvidersResponse{Providers: make([]rpc.AccountProvider, 0, len(ids))}
	for _, id := range ids {
		response.Providers = append(response.Providers, rpc.AccountProvider{ID: id, Login: loginMode(id), SignedIn: signedIn[id]})
	}
	return rpc.Marshal(response)
}

func loginMode(id string) string {
	switch {
	case id == "chatgpt":
		return "browser"
	case slices.Contains(auth.OAuthProviders(), id) && slices.Contains(auth.APIKeyProviders(), id):
		return "hybrid"
	case slices.Contains(auth.OAuthProviders(), id):
		return "terminal"
	}
	return "apiKey"
}
