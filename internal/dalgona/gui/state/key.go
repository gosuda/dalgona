package state

import "github.com/gosuda/dalgona/internal/dalgona/client"

type WindowID string

type ThreadKey struct {
	Endpoint client.EndpointID
	Thread   string
}

type ItemKey struct {
	Turn string
	Item string
}

type RequestKey struct {
	Thread  ThreadKey
	Request string
}

type DraftKey struct {
	Endpoint client.EndpointID
	Window   WindowID
	Thread   string
}

func (k DraftKey) thread() ThreadKey { return ThreadKey{Endpoint: k.Endpoint, Thread: k.Thread} }
