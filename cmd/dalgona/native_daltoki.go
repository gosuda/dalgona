package main

import (
	"context"
	"encoding/json/jsontext"
	"errors"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/rpcserver"
	"github.com/gosuda/dalgona/internal/dalgona/daltoki"
)

const (
	methodDaltokiRead   = "dalgona/daltoki/read"
	methodDaltokiDecide = "dalgona/daltoki/draft/decide"
	methodDaltokiEdit   = "dalgona/daltoki/draft/edit"
	methodDaltokiResume = "dalgona/daltoki/resume"
	daltokiDraftLimit   = 100
	decisionApprove     = "approve"
	decisionReject      = "reject"
)

var publicationResumed = jsontext.Value(`{"publicationPaused":false}`)

type daltokiSurface struct {
	service *daltoki.Service
}

type daltokiView struct {
	status daltoki.Status
	open   []daltoki.Draft
}

type draftView daltoki.Draft

type draftChange struct {
	id       string
	revision string
	decision string
	title    string
	body     string
}

func registerDaltoki(service *daltoki.Service, extensions, operations map[string]rpcserver.Extension) {
	surface := daltokiSurface{service: service}
	extensions[methodDaltokiRead] = surface.read
	operations[methodDaltokiDecide] = surface.decide
	operations[methodDaltokiEdit] = surface.edit
	operations[methodDaltokiResume] = surface.resume
}

func openDraft(draft daltoki.Draft) bool {
	return draft.State != daltoki.DraftPosted && draft.State != daltoki.DraftRejected
}

func (d daltokiSurface) read(context.Context, jsontext.Value) (jsontext.Value, error) {
	var view daltokiView
	if d.service == nil {
		return rpc.Marshal(view)
	}
	view.status = d.service.Status()
	for _, draft := range d.service.Drafts() {
		if openDraft(draft) {
			view.open = append(view.open, draft)
		}
	}
	return rpc.Marshal(view)
}

func decodeDraftChange(params jsontext.Value) (draftChange, error) {
	var change draftChange
	err := decodeMembers(params, change.readField)
	if err != nil || change.id == "" || change.revision == "" {
		return change, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return change, nil
}

func (c *draftChange) readField(name string, d *jsontext.Decoder) error {
	var target *string
	switch name {
	case "draftId":
		target = &c.id
	case "expectedRevision":
		target = &c.revision
	case "decision":
		target = &c.decision
	case "title":
		target = &c.title
	case "body":
		target = &c.body
	default:
		return d.SkipValue()
	}
	value, readErr := rpc.ReadString(d)
	*target = value
	return readErr
}

func (d daltokiSurface) decide(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	change, err := decodeDraftChange(params)
	if err != nil {
		return nil, err
	}
	if change.decision != decisionApprove && change.decision != decisionReject {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	if d.service == nil {
		return nil, rpc.NativeKind(rpc.ErrorUnavailable)
	}
	if err := d.service.DecideDraft(ctx, change.id, change.revision, change.decision == decisionApprove); err != nil {
		return nil, daltokiError(err)
	}
	return d.draft(change.id)
}

func (d daltokiSurface) edit(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	change, err := decodeDraftChange(params)
	if err != nil {
		return nil, err
	}
	if d.service == nil {
		return nil, rpc.NativeKind(rpc.ErrorUnavailable)
	}
	if err := d.service.EditDraft(ctx, change.id, change.revision, change.title, change.body); err != nil {
		return nil, daltokiError(err)
	}
	return d.draft(change.id)
}

func (d daltokiSurface) resume(ctx context.Context, _ jsontext.Value) (jsontext.Value, error) {
	if d.service == nil {
		return nil, rpc.NativeKind(rpc.ErrorUnavailable)
	}
	if err := d.service.Resume(ctx); err != nil {
		return nil, daltokiError(err)
	}
	return publicationResumed, nil
}

func (d daltokiSurface) draft(id string) (jsontext.Value, error) {
	for _, draft := range d.service.Drafts() {
		if draft.ID == id {
			return rpc.Marshal(draftView(draft))
		}
	}
	return nil, rpc.NativeKind(rpc.ErrorNotFound)
}

func daltokiError(err error) error {
	if stale, ok := errors.AsType[*daltoki.StaleDraftError](err); ok {
		return rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorStaleRevision, CurrentRevision: stale.Current})
	}
	switch {
	case errors.Is(err, daltoki.ErrUnknownDraft):
		return rpc.NativeKind(rpc.ErrorNotFound)
	case errors.Is(err, daltoki.ErrDraftState), errors.Is(err, daltoki.ErrEmptyDraft):
		return rpc.NativeKind(rpc.ErrorInvalidParams)
	case errors.Is(err, daltoki.ErrServiceDisabled), errors.Is(err, daltoki.ErrServiceStopped):
		return rpc.NativeKind(rpc.ErrorUnavailable)
	}
	return err
}

func (v daltokiView) Encode(o *rpc.Writer) {
	o.Open()
	o.Bool("enabled", v.status.Enabled)
	o.Bool("running", v.status.Running)
	o.Bool("publicationPaused", v.status.PublicationPaused)
	o.Str("lastError", v.status.LastError)
	o.Int("openDrafts", int64(len(v.open)))
	o.Key("drafts")
	o.OpenArray()
	for _, draft := range v.open[:min(len(v.open), daltokiDraftLimit)] {
		draftView(draft).Encode(o)
	}
	o.CloseArray()
	o.Close()
}

func (v draftView) Encode(o *rpc.Writer) {
	o.Open()
	o.Str("id", v.ID)
	o.Str("jobId", v.JobID)
	o.Str("kind", string(v.Kind))
	o.Str("repository", v.Repository.Owner+"/"+v.Repository.Name)
	o.Int("issue", v.Issue)
	o.Str("title", v.Title)
	o.Str("body", v.Body)
	o.OptStr("head", v.Head)
	o.OptStr("base", v.Base)
	o.Str("state", string(v.State))
	o.OptStr("reason", v.Reason)
	o.OptStr("url", v.URL)
	o.Str("revision", daltoki.Draft(v).Revision())
	o.Close()
}
