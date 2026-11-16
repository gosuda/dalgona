package app

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"testing"
)

func TestTurnBookRefusesQuestion(t *testing.T) {
	t.Parallel()
	var stderr bytes.Buffer
	book := newTurnBook(&stderr)
	params := jsontext.Value(`{"threadId":"t1","turnId":"u1","itemId":"c1","isBlocking":true,"questions":[{"id":"q1","header":"Database","question":"Which database?","isOther":true,"isSecret":false,"multiSelect":false,"options":null}]}`)
	_, err := book.Request(context.Background(), "item/tool/requestUserInput", params)
	if err == nil || err.Error() != "no interactive client" {
		t.Fatalf("Request error = %v, want no interactive client", err)
	}
	if got, want := stderr.String(), "declined question Database: no interactive client\n"; got != want {
		t.Fatalf("stderr = %q, want %q", got, want)
	}
}
