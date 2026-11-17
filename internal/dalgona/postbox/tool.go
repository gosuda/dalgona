package postbox

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"strconv"
	"strings"
	"unicode/utf8"

	json "encoding/json/v2"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/tool"
)

const postDescription = "Send untrusted peer text to another agent. Mail arrives automatically at a safe boundary. A queued receipt confirms durable storage, not that the recipient has read it."

const postSchemaJSON = `{
"type": "object",
"properties": {
  "to": {
    "type": "string",
    "minLength": 1,
    "maxLength": 192
  },
  "text": {
    "type": "string",
    "minLength": 1,
    "maxLength": 16384
  },
  "reply_to": {
    "type": "string",
    "pattern": "^mail-[0-9a-f]{64}$"
  }
},
"required": ["to", "text"],
"additionalProperties": false
}`

type postTool struct {
	service *Service
}

func (t *postTool) Spec() tool.Spec {
	return tool.Spec{
		Name:        "post",
		Description: postDescription,
		Schema:      jsontext.Value(postSchemaJSON),
		Keep:        tool.KeepHead,
	}
}

func (t *postTool) Class() tool.Class { return tool.ClassAgent }

type postArgs struct {
	to      string
	text    string
	replyTo string
}

func parsePostArgs(raw jsontext.Value) (postArgs, error) {
	decoder := jsontext.NewDecoder(strings.NewReader(string(raw)))
	start, err := decoder.ReadToken()
	if err != nil || start.Kind() != '{' {
		return postArgs{}, errors.New("post: invalid arguments")
	}
	var out postArgs
	seen := map[string]bool{}
	for decoder.PeekKind() != '}' {
		name, field, err := readPostField(decoder, seen)
		if err != nil {
			return postArgs{}, err
		}
		if !field.known {
			return postArgs{}, fmt.Errorf("post: unknown field %q", name)
		}
		assignPostArg(&out, name, field.text)
	}
	return finishPostArgs(decoder, out, seen)
}

type postField struct {
	text  string
	known bool
}

func readPostField(decoder *jsontext.Decoder, seen map[string]bool) (string, postField, error) {
	nameToken, err := decoder.ReadToken()
	if err != nil {
		return "", postField{}, errors.New("post: invalid arguments")
	}
	name := nameToken.String()
	if seen[name] {
		return "", postField{}, errors.New("post: invalid arguments")
	}
	seen[name] = true
	switch name {
	case "to", "text", "reply_to":
		text, err := readPostString(decoder)
		if err != nil {
			return "", postField{}, err
		}
		return name, postField{text: text, known: true}, nil
	default:
		return name, postField{}, nil
	}
}

func readPostString(decoder *jsontext.Decoder) (string, error) {
	if decoder.PeekKind() != '"' {
		return "", errors.New("post: invalid arguments")
	}
	valueToken, err := decoder.ReadToken()
	if err != nil {
		return "", errors.New("post: invalid arguments")
	}
	text := valueToken.String()
	if !utf8.ValidString(text) {
		return "", errors.New("post: invalid arguments")
	}
	return text, nil
}

func assignPostArg(out *postArgs, name, text string) {
	switch name {
	case "to":
		out.to = text
	case "text":
		out.text = text
	case "reply_to":
		out.replyTo = text
	}
}

func finishPostArgs(decoder *jsontext.Decoder, out postArgs, seen map[string]bool) (postArgs, error) {
	if _, err := decoder.ReadToken(); err != nil {
		return postArgs{}, errors.New("post: invalid arguments")
	}
	if !seen["to"] || !seen["text"] {
		return postArgs{}, errors.New("post: invalid arguments")
	}
	if err := checkPostLengths(out); err != nil {
		return postArgs{}, err
	}
	if seen["reply_to"] && !validReplyTo(out.replyTo) {
		return postArgs{}, errors.New("post: invalid arguments")
	}
	return out, nil
}

func checkPostLengths(out postArgs) error {
	if len(out.to) < 1 || len(out.to) > 192 {
		return errors.New("post: invalid arguments")
	}
	if len(out.text) < 1 || len(out.text) > 16384 {
		return errors.New("post: invalid arguments")
	}
	return nil
}

func validReplyTo(value string) bool {
	if len(value) != 69 || !strings.HasPrefix(value, "mail-") {
		return false
	}
	for i := 5; i < len(value); i++ {
		c := value[i]
		if (c < '0' || c > '9') && (c < 'a' || c > 'f') {
			return false
		}
	}
	return true
}

type postReceiptJSON struct {
	MessageID string `json:"message_id"`
	PairSeq   string `json:"pair_seq"`
	Delivery  string `json:"delivery"`
}

func (t *postTool) Run(ctx context.Context, call tool.Call, _ tool.Env) (tool.Result, error) {
	args, err := parsePostArgs(call.Args)
	if err != nil {
		return tool.Result{OK: false, Output: []byte(err.Error())}, nil
	}
	thread, turnID, ok := agent.MailBinding(ctx)
	if !ok {
		return tool.Result{OK: false, Output: []byte("post: no mail turn bound")}, nil
	}
	_, state, err := thread.Records()
	if err != nil {
		return tool.Result{OK: false, Output: []byte("post: no mail turn bound")}, nil
	}
	sender, err := t.service.Bind(ctx, state.SessionID, turnID)
	if err != nil {
		return tool.Result{OK: false, Output: []byte(err.Error())}, nil
	}
	receipt, err := sender.Post(ctx, Request{
		OperationID: call.ID,
		To:          args.to,
		Text:        args.text,
		ReplyTo:     args.replyTo,
	})
	if err != nil {
		return tool.Result{OK: false, Output: []byte(err.Error())}, nil
	}
	out, err := json.Marshal(postReceiptJSON{
		MessageID: receipt.MessageID,
		PairSeq:   strconv.FormatUint(receipt.PairSeq, 10),
		Delivery:  receipt.Delivery,
	})
	if err != nil {
		return tool.Result{OK: false, Output: []byte("post: invalid arguments")}, nil
	}
	return tool.Result{OK: true, Output: out}, nil
}

func Tools(ctx context.Context, service *Service) []tool.Tool {
	if service == nil {
		return nil
	}
	thread, ok := agent.ThreadFromContext(ctx)
	if !ok {
		return nil
	}
	_, state, err := thread.Records()
	if err != nil || state.Advisor != "" {
		return nil
	}
	return []tool.Tool{&postTool{service: service}}
}
