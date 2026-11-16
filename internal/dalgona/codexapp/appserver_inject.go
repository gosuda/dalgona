package codexapp

import (
	"bytes"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
)

type paramInjector struct {
	defaults sessionDefaults
	reader   bytes.Reader
	decoder  *jsontext.Decoder
	encoder  *jsontext.Encoder
	out      bytes.Buffer
	name     []byte
}

func newParamInjector(defaults sessionDefaults) *paramInjector {
	injector := &paramInjector{defaults: defaults}
	injector.decoder = jsontext.NewDecoder(&injector.reader)
	injector.encoder = jsontext.NewEncoder(&injector.out)
	return injector
}

func (p *paramInjector) line(raw []byte) []byte {
	if rewritten, ok := p.rewrite(trimLineEnd(raw)); ok {
		return rewritten
	}
	return raw
}

func (p *paramInjector) rewrite(message []byte) ([]byte, bool) {
	if p.defaults == (sessionDefaults{}) || !bytes.Contains(message, []byte(quotedThreadStart)) || !p.startsThread(message) {
		return nil, false
	}
	if err := p.inject(message); err != nil {
		return nil, false
	}
	return p.out.Bytes(), true
}

func (p *paramInjector) restart(message []byte) {
	p.reader.Reset(message)
	p.decoder.Reset(&p.reader)
}

func (p *paramInjector) startsThread(message []byte) bool {
	p.restart(message)
	if token, err := p.decoder.ReadToken(); err != nil || token.Kind() != jsontext.KindBeginObject {
		return false
	}
	for p.decoder.PeekKind() == jsontext.KindString {
		name, err := p.decoder.ReadValue()
		if err != nil {
			return false
		}
		if string(name) == `"method"` {
			method, err := p.decoder.ReadValue()
			return err == nil && string(method) == quotedThreadStart
		}
		if p.decoder.SkipValue() != nil {
			return false
		}
	}
	return false
}

func (p *paramInjector) inject(message []byte) error {
	p.restart(message)
	p.out.Reset()
	p.encoder.Reset(&p.out)
	if err := p.copyToken(); err != nil {
		return err
	}
	sawParams, err := p.copyRequestMembers()
	if err != nil {
		return err
	}
	if !sawParams {
		if err := errors.Join(p.encoder.WriteToken(jsontext.String("params")), p.writeDefaultsObject()); err != nil {
			return err
		}
	}
	if err := p.copyToken(); err != nil {
		return err
	}
	if _, err := p.decoder.ReadToken(); !errors.Is(err, io.EOF) {
		return fmt.Errorf("trailing data after message: %w", err)
	}
	return nil
}

func (p *paramInjector) copyRequestMembers() (bool, error) {
	sawParams := false
	for p.decoder.PeekKind() == jsontext.KindString {
		isParams, err := p.copyName(`"params"`)
		if err == nil && isParams {
			sawParams = true
			err = p.copyParams()
		} else if err == nil {
			err = p.copyValue()
		}
		if err != nil {
			return false, err
		}
	}
	return sawParams, nil
}

func (p *paramInjector) copyToken() error {
	token, err := p.decoder.ReadToken()
	if err != nil {
		return err
	}
	return p.encoder.WriteToken(token)
}

func (p *paramInjector) copyValue() error {
	value, err := p.decoder.ReadValue()
	if err != nil {
		return err
	}
	return p.encoder.WriteValue(value)
}

func (p *paramInjector) copyName(wanted string) (bool, error) {
	name, err := p.decoder.ReadValue()
	if err != nil {
		return false, err
	}
	matched := string(name) == wanted
	return matched, p.encoder.WriteValue(name)
}

func (p *paramInjector) copyParams() error {
	kind := p.decoder.PeekKind()
	if kind == jsontext.KindNull {
		if _, err := p.decoder.ReadToken(); err != nil {
			return err
		}
		return p.writeDefaultsObject()
	}
	if kind != jsontext.KindBeginObject {
		return p.copyValue()
	}
	if err := p.copyToken(); err != nil {
		return err
	}
	var present [sessionFieldCount]bool
	for p.decoder.PeekKind() == jsontext.KindString {
		if err := p.copyParamsMember(&present); err != nil {
			return err
		}
	}
	if err := p.writeMissingDefaults(present); err != nil {
		return err
	}
	return p.copyToken()
}

func (p *paramInjector) copyParamsMember(present *[sessionFieldCount]bool) error {
	name, err := p.decoder.ReadValue()
	if err != nil {
		return err
	}
	p.name = append(p.name[:0], name...)
	field, isSessionField := sessionFieldNamed(p.name)
	if isSessionField && p.defaults[field] != "" && p.decoder.PeekKind() == jsontext.KindNull {
		_, err := p.decoder.ReadToken()
		return err
	}
	if isSessionField {
		present[field] = true
	}
	if err := p.encoder.WriteValue(p.name); err != nil {
		return err
	}
	return p.copyValue()
}

func sessionFieldNamed(name []byte) (sessionField, bool) {
	for field, quoted := range quotedSessionFieldNames {
		if string(name) == quoted {
			return sessionField(field), true
		}
	}
	return 0, false
}

func (p *paramInjector) writeDefaultsObject() error {
	return errors.Join(
		p.encoder.WriteToken(jsontext.BeginObject),
		p.writeMissingDefaults([sessionFieldCount]bool{}),
		p.encoder.WriteToken(jsontext.EndObject),
	)
}

func (p *paramInjector) writeMissingDefaults(present [sessionFieldCount]bool) error {
	var err error
	for field, value := range p.defaults {
		if value == "" || present[field] {
			continue
		}
		err = errors.Join(err,
			p.encoder.WriteToken(jsontext.String(sessionFieldNames[field])),
			p.encoder.WriteToken(jsontext.String(value)))
	}
	return err
}
