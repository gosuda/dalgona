package decide

import (
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
)

func readKind(decoder *jsontext.Decoder, kind jsontext.Kind) error {
	token, err := decoder.ReadToken()
	if err != nil {
		return err
	}
	if token.Kind() != kind {
		return fmt.Errorf("expected JSON token %q, got %q", kind, token.Kind())
	}
	return nil
}

func requireEOF(decoder *jsontext.Decoder) error {
	_, err := decoder.ReadToken()
	if errors.Is(err, io.EOF) {
		return nil
	}
	if err != nil {
		return err
	}
	return errors.New("unexpected trailing JSON value")
}
