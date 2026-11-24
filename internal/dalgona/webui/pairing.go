package webui

import (
	"context"
	"errors"
	"io"
	"net"
	"net/http"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const maxPairingRequests = 16

func (s *server) pairingIdentity(w http.ResponseWriter, r *http.Request) {
	if !s.pairingAllowed(w, r) {
		return
	}
	s.sendMachineIdentity(w, r, http.StatusNoContent)
}

func (s *server) sendMachineIdentity(w http.ResponseWriter, r *http.Request, status int) {
	conn, reply := s.machineRequest(w, r, rpc.MachineRequest{Action: rpc.MachineIdentity})
	if conn == nil {
		return
	}
	_ = conn.Close()
	w.Header().Set(rpc.MachineIDHeader, reply.MachineID)
	w.WriteHeader(status)
}

func (s *server) pairingExchange(w http.ResponseWriter, r *http.Request) {
	if !s.pairingAllowed(w, r) {
		return
	}
	if r.Header.Get("Content-Type") != "application/octet-stream" {
		http.Error(w, "pairing exchange needs a machine frame", http.StatusUnsupportedMediaType)
		return
	}
	req, err := readPairingRequest(w, r)
	if err != nil {
		http.Error(w, "invalid pairing request", http.StatusBadRequest)
		return
	}
	conn, reply := s.machineRequest(w, r, req)
	if conn == nil {
		return
	}
	_ = conn.Close()
	w.Header().Set("Content-Type", "application/octet-stream")
	if err := rpc.WriteMachineFrame(w, reply); err != nil {
		s.logf("web: could not send pairing result\n")
	}
}

func readPairingRequest(w http.ResponseWriter, r *http.Request) (rpc.MachineRequest, error) {
	var req rpc.MachineRequest
	if err := http.NewResponseController(w).SetReadDeadline(time.Now().Add(5 * time.Second)); err != nil {
		return req, err
	}
	body := http.MaxBytesReader(w, r.Body, rpc.MachinePrefaceLimit+2)
	if err := rpc.ReadMachineFrame(body, rpc.MachinePrefaceLimit, &req); err != nil {
		return req, err
	}
	var trailing [1]byte
	_, err := body.Read(trailing[:])
	if !errors.Is(err, io.EOF) || req.Version != rpc.MachineVersion || req.Action != rpc.MachineExchange {
		return req, errors.New("invalid pairing exchange frame")
	}
	return req, nil
}

func (s *server) deviceRPC(w http.ResponseWriter, r *http.Request) {
	if !s.pairingAllowed(w, r) {
		return
	}
	if len(r.Header.Values("Authorization")) == 0 {
		w.Header().Set("WWW-Authenticate", "Bearer")
		s.sendMachineIdentity(w, r, http.StatusUnauthorized)
		return
	}
	req, ok := deviceAdmission(r)
	if !ok {
		http.Error(w, "device credential and expected machine are required", http.StatusUnauthorized)
		return
	}
	s.relays.Add(1)
	defer s.relays.Done()
	conn, _ := s.machineRequest(w, r, req)
	if conn != nil {
		s.serveRelay(w, r, conn)
	}
}

func deviceAdmission(r *http.Request) (rpc.MachineRequest, bool) {
	auth, machine := r.Header.Values("Authorization"), r.Header.Values(rpc.MachineIDHeader)
	if len(auth) != 1 || len(machine) != 1 || len(machine[0]) != 64 {
		return rpc.MachineRequest{}, false
	}
	scheme, credential, separated := strings.Cut(auth[0], " ")
	if !separated || !strings.EqualFold(scheme, "Bearer") || len(credential) != 43 {
		return rpc.MachineRequest{}, false
	}
	return rpc.MachineRequest{Action: rpc.MachineAdmit, MachineID: machine[0], Credential: credential}, true
}

func (s *server) pairingAllowed(w http.ResponseWriter, r *http.Request) bool {
	w.Header().Set("Cache-Control", "no-store")
	if s.pairingDial == nil || r.TLS == nil {
		http.NotFound(w, r)
		return false
	}
	if r.URL.RawQuery != "" || r.URL.ForceQuery || len(r.Header.Values("Origin")) != 0 {
		http.Error(w, "pairing routes accept no query or browser origin", http.StatusBadRequest)
		return false
	}
	return true
}

func (s *server) machineRequest(w http.ResponseWriter, r *http.Request, req rpc.MachineRequest) (net.Conn, rpc.MachineReply) {
	select {
	case s.pairingSlots <- struct{}{}:
		defer func() { <-s.pairingSlots }()
	default:
		http.Error(w, "pairing busy", http.StatusServiceUnavailable)
		return nil, rpc.MachineReply{}
	}
	ctx, cancel := context.WithTimeout(r.Context(), 5*time.Second)
	defer cancel()
	conn, err := s.pairingDial(ctx)
	if err != nil {
		http.Error(w, "local daemon unavailable", http.StatusBadGateway)
		return nil, rpc.MachineReply{}
	}
	reply, err := rpc.ExchangeMachine(ctx, conn, req)
	if err != nil {
		_ = conn.Close()
		http.Error(w, "local daemon machine protocol unavailable", http.StatusBadGateway)
		return nil, rpc.MachineReply{}
	}
	if reply.Status != "" {
		_ = conn.Close()
		w.Header().Set(rpc.MachineIDHeader, reply.MachineID)
		httpCode := machineStatus(reply.Status)
		http.Error(w, http.StatusText(httpCode), httpCode)
		return nil, reply
	}
	return conn, reply
}

func machineStatus(status string) int {
	switch status {
	case rpc.MachineMismatch:
		return http.StatusConflict
	case rpc.MachineDenied:
		return http.StatusUnauthorized
	case rpc.MachineInvalid:
		return http.StatusBadRequest
	default:
		return http.StatusServiceUnavailable
	}
}
