package tui

import (
	"bytes"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"fmt"

	"github.com/gosuda/dalgona/internal/rpc"
)

func decodeParams[T any](params jsontext.Value) (T, error) {
	var out T
	if err := json.Unmarshal(params, &out); err != nil {
		return out, fmt.Errorf("decode params: %w", err)
	}
	return out, nil
}

type deltaDecoder struct {
	reader   bytes.Reader
	decoder  jsontext.Decoder
	threadID string
	itemID   string
	text     []byte
}

func (d *deltaDecoder) decode(params jsontext.Value) error {
	d.reader.Reset(params)
	d.decoder.Reset(&d.reader)
	d.threadID, d.itemID, d.text = "", "", d.text[:0]
	if err := d.expect('{'); err != nil {
		return err
	}
	for d.decoder.PeekKind() == '"' {
		token, err := d.decoder.ReadToken()
		if err != nil {
			return fmt.Errorf("decode delta field: %w", err)
		}
		field := token.String()
		switch field {
		case "threadId":
			d.threadID, err = d.readString()
		case "itemId":
			d.itemID, err = d.readString()
		case "delta":
			err = d.readDelta()
		default:
			err = d.decoder.SkipValue()
		}
		if err != nil {
			return fmt.Errorf("decode delta %s: %w", field, err)
		}
	}
	return d.expect('}')
}

func (d *deltaDecoder) expect(kind jsontext.Kind) error {
	token, err := d.decoder.ReadToken()
	if err != nil {
		return fmt.Errorf("decode delta: %w", err)
	}
	if token.Kind() != kind {
		return fmt.Errorf("decode delta: expected %s, got %s", kind, token.Kind())
	}
	return nil
}

func (d *deltaDecoder) readString() (string, error) {
	token, err := d.decoder.ReadToken()
	if err != nil {
		return "", err
	}
	if token.Kind() != '"' {
		return "", fmt.Errorf("expected string, got %s", token.Kind())
	}
	return token.String(), nil
}

func (d *deltaDecoder) readDelta() error {
	value, err := d.decoder.ReadValue()
	if err != nil {
		return err
	}
	if value.Kind() != '"' {
		return fmt.Errorf("expected string, got %s", value.Kind())
	}
	d.text, err = jsontext.AppendUnquote(d.text, value)
	return err
}

func decodeApproval(method string, params jsontext.Value) (approval, error) {
	switch method {
	case "item/commandExecution/requestApproval":
		request, err := decodeParams[rpc.CommandExecutionRequestApprovalParams](params)
		return approval{
			method: method, threadID: sanitizeText(request.ThreadID), itemID: request.ItemID,
			command: sanitizeText(request.Command), cwd: sanitizeText(request.Cwd), reason: sanitizeText(request.Reason),
		}, err
	case "item/fileChange/requestApproval":
		request, err := decodeParams[rpc.FileChangeRequestApprovalParams](params)
		return approval{method: method, threadID: sanitizeText(request.ThreadID), itemID: request.ItemID, reason: sanitizeText(request.Reason)}, err
	default:
		return approval{}, fmt.Errorf("unsupported server request %s", method)
	}
}
