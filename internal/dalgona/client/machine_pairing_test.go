package client

import (
	"crypto/sha256"
	"crypto/tls"
	"encoding/hex"
	"encoding/json/jsontext"
	"errors"
	"net/http"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func TestPairingSendsNoSecretBeforeSamePeerVerification(t *testing.T) {
	files := writeRelayFiles(t)
	pinDigest := sha256.Sum256(files.pair.Leaf.RawSubjectPublicKeyInfo)
	machine := strings.Repeat("1", 64)
	for _, test := range []struct {
		name           string
		wrongPin       bool
		wrongMachine   bool
		dropConnection bool
	}{
		{name: "wrong relay key", wrongPin: true},
		{name: "wrong machine", wrongMachine: true},
		{name: "no reuse of unverified second connection", dropConnection: true},
	} {
		t.Run(test.name, func(t *testing.T) {
			actual := machine
			if test.wrongMachine {
				actual = strings.Repeat("2", 64)
			}
			peer, traffic := startRelayServer(t, files, tls.VersionTLS13, unverifiedPairingHandler(t, actual, test.dropConnection))
			pin := hex.EncodeToString(pinDigest[:])
			if test.wrongPin {
				pin = strings.Repeat("0", 64)
			}
			card := rpc.PairingCard{Origin: peer.URL, MachineID: machine, PublicKeyPin: pin}
			remotes := NewRemotes(config.Config{Home: t.TempDir(), TLSCAFile: files.cert}, nil)
			_, err := remotes.PairDevice(t.Context(), "work", card, "test-only-grant", "workstation")
			refuseUnverifiedPairing(t, err, test.wrongMachine, machine, actual)
			assertPairingTraffic(t, traffic, rpc.MachineIdentityPath, test.wrongPin)
		})
	}
}

func TestPairedDeviceRequiresVerifiedChallengeBeforeSendingBearer(t *testing.T) {
	files := writeRelayFiles(t)
	pin := sha256.Sum256(files.pair.Leaf.RawSubjectPublicKeyInfo)
	machine, other := strings.Repeat("1", 64), strings.Repeat("2", 64)
	tokenFile := filepath.Join(t.TempDir(), "device.json")
	if err := auth.WriteSecure(tokenFile, func(e *jsontext.Encoder) error {
		return e.WriteToken(jsontext.String(strings.Repeat("A", 43)))
	}); err != nil {
		t.Fatal(err)
	}
	t.Run("wrong machine", func(t *testing.T) {
		peer, traffic := startRelayServer(t, files, tls.VersionTLS13, deviceChallenge(other, ""))
		spec := config.RemoteSpec{URL: peer.URL, MachineID: machine, PublicKeyPin: hex.EncodeToString(pin[:]), TokenFile: tokenFile}
		remotes := NewRemotes(config.Config{TLSCAFile: files.cert}, nil)
		if _, err := remotes.dialDevice(t.Context(), spec); !matchesMachineMismatch(err, machine, other) {
			t.Fatalf("wrong device challenge did not reach typed refusal: %v", err)
		}
		assertPairingTraffic(t, traffic, rpc.MachineDevicePath, false)
	})
	t.Run("peer closes after challenge", func(t *testing.T) {
		peer, traffic := startRelayServer(t, files, tls.VersionTLS13, deviceChallenge(machine, "close"))
		spec := config.RemoteSpec{URL: peer.URL, MachineID: machine, PublicKeyPin: hex.EncodeToString(pin[:]), TokenFile: tokenFile}
		remotes := NewRemotes(config.Config{TLSCAFile: files.cert}, nil)
		if _, err := remotes.dialDevice(t.Context(), spec); err == nil {
			t.Fatal("closed verified connection was replaced")
		}
		assertPairingTraffic(t, traffic, rpc.MachineDevicePath, false)
	})
}

func unverifiedPairingHandler(t *testing.T, actual string, dropConnection bool) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != rpc.MachineIdentityPath {
			t.Error("secret-bearing route reached without preflight trust")
			http.Error(w, "unexpected route", http.StatusInternalServerError)
			return
		}
		if dropConnection {
			w.Header().Set("Connection", "close")
		}
		w.Header().Set(rpc.MachineIDHeader, actual)
		w.WriteHeader(http.StatusNoContent)
	}
}

func refuseUnverifiedPairing(t *testing.T, err error, wrongMachine bool, machine, actual string) {
	t.Helper()
	if err == nil {
		t.Fatal("pairing succeeded without verified peer and identity")
	}
	if wrongMachine && !matchesMachineMismatch(err, machine, actual) {
		t.Fatalf("wrong machine did not reach typed refusal: %v", err)
	}
}

func deviceChallenge(machine, connection string) http.HandlerFunc {
	return func(w http.ResponseWriter, _ *http.Request) {
		if connection != "" {
			w.Header().Set("Connection", connection)
		}
		w.Header().Set(rpc.MachineIDHeader, machine)
		w.WriteHeader(http.StatusUnauthorized)
	}
}

func assertPairingTraffic(t *testing.T, traffic <-chan relayRequest, path string, noTraffic bool) {
	t.Helper()
	if !noTraffic {
		select {
		case event := <-traffic:
			if event.uri != path || event.authorization != "" {
				t.Fatal("identity discovery carried a credential or wrong path")
			}
		default:
			t.Fatal("preflight never reached the relay")
		}
	}
	select {
	case <-traffic:
		t.Fatal("unexpected traffic after refusing identity or TLS")
	default:
	}
}

func matchesMachineMismatch(err error, expected, actual string) bool {
	mismatch, ok := errors.AsType[*MachineMismatchError](err)
	return ok && mismatch.Expected == expected && mismatch.Actual == actual
}
