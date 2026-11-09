package decide_test

import (
	"bytes"
	"encoding/json/jsontext"
	"io"
	"net/http"
	"testing"

	"github.com/gosuda/dalgona/internal/decide"
)

type capturedRequest struct {
	header http.Header
	body   []byte
}

func captureRequest(r *http.Request) (capturedRequest, error) {
	body, err := io.ReadAll(r.Body)
	if err != nil {
		return capturedRequest{}, err
	}
	return capturedRequest{header: r.Header.Clone(), body: body}, nil
}

func (c capturedRequest) bodyReader() io.Reader {
	return bytes.NewReader(c.body)
}

func testQuestions() map[string]decide.Question {
	return map[string]decide.Question{
		"team": {
			Type:         decide.TypeChoice,
			Instructions: "Which team owns this ticket?",
			Criteria: map[string]string{
				"backend":  "API and storage issues.",
				"frontend": "Browser issues.",
			},
		},
		"is_bug": {
			Type:         decide.TypeNoul,
			Instructions: "Does this describe a defect?",
			Criteria: map[string]string{
				"false": "A question or feature request.",
				"true":  "Broken behavior.",
			},
		},
		"urgency": {
			Type:         decide.TypeScore,
			Instructions: "How urgent is the fix?",
			Criteria:     []string{"Later", "Soon", "Now"},
		},
	}
}

func decodeObject(t *testing.T, reader io.Reader) map[string]jsontext.Value {
	t.Helper()
	decoder := jsontext.NewDecoder(reader)
	token, err := decoder.ReadToken()
	if err != nil || token.Kind() != jsontext.KindBeginObject {
		t.Fatalf("object start = %v, %v", token, err)
	}
	values := make(map[string]jsontext.Value)
	for decoder.PeekKind() != jsontext.KindEndObject {
		name, err := decoder.ReadToken()
		if err != nil {
			t.Fatal(err)
		}
		key := name.String()
		value, err := decoder.ReadValue()
		if err != nil {
			t.Fatal(err)
		}
		values[key] = value.Clone()
	}
	if _, err := decoder.ReadToken(); err != nil {
		t.Fatal(err)
	}
	return values
}

func decodeArray(t *testing.T, raw jsontext.Value) []jsontext.Value {
	t.Helper()
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	token, err := decoder.ReadToken()
	if err != nil || token.Kind() != jsontext.KindBeginArray {
		t.Fatalf("array start = %v, %v", token, err)
	}
	var values []jsontext.Value
	for decoder.PeekKind() != jsontext.KindEndArray {
		value, err := decoder.ReadValue()
		if err != nil {
			t.Fatal(err)
		}
		values = append(values, value.Clone())
	}
	if _, err := decoder.ReadToken(); err != nil {
		t.Fatal(err)
	}
	return values
}

func decodeString(t *testing.T, raw jsontext.Value) string {
	t.Helper()
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	token, err := decoder.ReadToken()
	if err != nil || token.Kind() != jsontext.KindString {
		t.Fatalf("string = %v, %v", token, err)
	}
	return token.String()
}

func nestedObject(t *testing.T, raw jsontext.Value) map[string]jsontext.Value {
	t.Helper()
	return decodeObject(t, bytes.NewReader(raw))
}

func checkStateAndQuestions(t *testing.T, raw jsontext.Value) {
	t.Helper()
	envelope := nestedObject(t, raw)
	state := nestedObject(t, envelope["state"])
	if got := decodeString(t, state["ticket"]); got != "Checkout fails" {
		t.Fatalf("ticket = %q", got)
	}
	questions := nestedObject(t, envelope["questions"])
	team := nestedObject(t, questions["team"])
	if got := decodeString(t, team["type"]); got != "choice" {
		t.Fatalf("team type = %q", got)
	}
	criteria := nestedObject(t, team["criteria"])
	if got := decodeString(t, criteria["backend"]); got != "API and storage issues." {
		t.Fatalf("backend criterion = %q", got)
	}
	urgency := nestedObject(t, questions["urgency"])
	if got := len(decodeArray(t, urgency["criteria"])); got != 3 {
		t.Fatalf("urgency criteria count = %d", got)
	}
}

func checkNoul(t *testing.T, answer decide.Answer, want float64) {
	t.Helper()
	if answer.Noul == nil {
		t.Fatal("noul is nil")
	}
	if *answer.Noul != want {
		t.Errorf("noul = %v, want %v", *answer.Noul, want)
	}
}

func checkScore(t *testing.T, answer decide.Answer, want float64) {
	t.Helper()
	if answer.Score == nil {
		t.Fatal("score is nil")
	}
	if *answer.Score != want {
		t.Errorf("score = %v, want %v", *answer.Score, want)
	}
}
