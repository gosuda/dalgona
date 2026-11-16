package llmstub

import (
	"context"
	"net/http/httptest"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/model/openai"
)

const liveLoginKey = "sk-live-test"

func liveModelIDs(t *testing.T, serverURL, key string) ([]string, error) {
	t.Helper()
	provider := openai.New(openai.Config{
		BaseURL: serverURL + "/v1",
		APIKey:  func(context.Context) (string, error) { return key, nil },
	})
	models, err := provider.Models(t.Context())
	ids := make([]string, 0, len(models))
	for _, item := range models {
		ids = append(ids, item.ID)
	}
	return ids, err
}

func TestLiveModelListAnswersTheOpenAIKeyCheck(t *testing.T) {
	t.Parallel()
	server := httptest.NewServer(NewHandler(loadLiveScript(t), Options{Key: liveLoginKey}))
	t.Cleanup(server.Close)
	ids, err := liveModelIDs(t, server.URL, liveLoginKey)
	if err != nil {
		t.Fatal(err)
	}
	if want := []string{"qwen-test", "qwen-small"}; !slices.Equal(ids, want) {
		t.Fatalf("model ids = %v, want %v", ids, want)
	}
	if _, err = liveModelIDs(t, server.URL, "sk-other"); err == nil || !strings.Contains(err.Error(), "http 401") {
		t.Fatalf("a key other than the server key gave error %v, want http 401", err)
	}
}
