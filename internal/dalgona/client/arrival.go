package client

import (
	"context"
	"encoding/json/jsontext"
)

func (r *linkRoute) RequestArrived(method string, params jsontext.Value) {
	if r.announcedLive() {
		r.observer.Arrived(r.link, method, params)
	}
}

func (s *EndpointSet) Respond(ctx context.Context, link Link, requestID string, result any) error {
	conn, release, err := s.use(link)
	if err != nil {
		return err
	}
	defer release()
	return conn.Respond(ctx, requestID, result)
}
