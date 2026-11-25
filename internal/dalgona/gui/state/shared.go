package state

import (
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"slices"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

const (
	methodModelList    = "model/list"
	methodConfigRead   = "config/read"
	methodSettingsRead = "dalgona/settings/read"
	modelPageLimit     = 8
)

type (
	RefreshEndpoint struct{ Endpoint client.EndpointID }
	sharedState     struct {
		models   []rpc.Model
		config   rpc.ConfigValues
		settings []rpc.Setting
		err      error
		pending  uint64
	}
	sharedCall struct {
		link   client.Link
		round  uint64
		method string
		pages  int
	}
	modelListParams struct {
		Cursor string `json:"cursor,omitzero"`
	}
	settingsNote struct {
		Settings []settingNote `json:"settings"`
	}
	settingNote struct {
		Key     string           `json:"key"`
		Value   jsontext.Value   `json:"value"`
		Default jsontext.Value   `json:"defaultValue"`
		Options []jsontext.Value `json:"options"`
	}
)

var sharedReaders = map[string]func(m *machine, ep *endpointState, c sharedCall, raw jsontext.Value) error{
	methodModelList:    (*machine).readModels,
	methodConfigRead:   (*machine).readConfig,
	methodSettingsRead: (*machine).readSettings,
}

func (i RefreshEndpoint) apply(m *machine) {
	ep := m.endpoint(i.Endpoint)
	m.use(ep)
	if ep.live {
		m.loadShared(ep)
	}
}

func (m *machine) loadShared(ep *endpointState) {
	round := m.next()
	ep.shared.pending, ep.shared.err = round, nil
	m.call(ep, sharedCall{link: ep.link, round: round, method: methodModelList}, methodModelList, modelListParams{})
	m.call(ep, sharedCall{link: ep.link, round: round, method: methodConfigRead}, methodConfigRead, struct{}{})
	m.call(ep, sharedCall{link: ep.link, round: round, method: methodSettingsRead}, methodSettingsRead, struct{}{})
}

func (c sharedCall) resume(m *machine, r resultMsg) {
	ep, ok := m.endpoints[c.link.Endpoint]
	if !ok || !ep.live || ep.link != c.link || ep.shared.pending != c.round {
		return
	}
	m.touchEndpoint(ep)
	err := r.err
	if err == nil {
		err = sharedReaders[c.method](m, ep, c, r.raw)
	}
	if err != nil {
		ep.shared.err = err
	}
}

func (m *machine) readModels(ep *endpointState, c sharedCall, raw jsontext.Value) error {
	page, err := decode[rpc.ModelListResponse](raw)
	if err != nil {
		return err
	}
	if c.pages == 0 {
		ep.shared.models = nil
	}
	ep.shared.models = append(slices.Clip(ep.shared.models), page.Data...)
	if page.NextCursor != "" && c.pages+1 < modelPageLimit {
		next := sharedCall{link: c.link, round: c.round, method: methodModelList, pages: c.pages + 1}
		m.call(ep, next, methodModelList, modelListParams{Cursor: page.NextCursor})
	}
	return nil
}

func (m *machine) readConfig(ep *endpointState, _ sharedCall, raw jsontext.Value) error {
	response, err := decode[rpc.ConfigReadResponse](raw)
	if err == nil {
		ep.shared.config = response.Config
	}
	return err
}

func (m *machine) readSettings(ep *endpointState, _ sharedCall, raw jsontext.Value) error {
	note, err := decode[settingsNote](raw)
	if err != nil {
		return err
	}
	settings := make([]rpc.Setting, 0, len(note.Settings))
	for _, s := range note.Settings {
		options := make([]any, 0, len(s.Options))
		for _, option := range s.Options {
			options = append(options, settingValue(option))
		}
		settings = append(settings, rpc.Setting{Key: s.Key, Value: settingValue(s.Value), Default: settingValue(s.Default), Options: options})
	}
	ep.shared.settings = settings
	return nil
}

func settingValue(raw jsontext.Value) any {
	var text string
	if json.Unmarshal(raw, &text) == nil {
		return text
	}
	var flag bool
	if json.Unmarshal(raw, &flag) == nil {
		return flag
	}
	var list []string
	if json.Unmarshal(raw, &list) == nil {
		return list
	}
	return nil
}

func (s *sharedState) setting(key string) any {
	at := slices.IndexFunc(s.settings, func(setting rpc.Setting) bool { return setting.Key == key })
	if at < 0 {
		return nil
	}
	return s.settings[at].Value
}

func (s *sharedState) text(key string) string {
	text, _ := s.setting(key).(string)
	return text
}

func (s *sharedState) list(key string) []string {
	list, _ := s.setting(key).([]string)
	return list
}
