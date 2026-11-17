package daltoki

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"strings"
	"time"
)

const (
	relayEnvelopeMaxAge = 7 * 24 * time.Hour
	relayEnvelopeFuture = 60 * time.Second
	maxWebhookBody      = 1048576
)

var (
	ErrInvalidSignature = errors.New("daltoki: invalid webhook signature")
	ErrOutsideWindow    = errors.New("daltoki: relay envelope outside replay window")
)

type Envelope struct {
	DeliveryID string    `json:"deliveryID"`
	Event      string    `json:"event"`
	ReceivedAt time.Time `json:"receivedAt"`
	BodySHA256 string    `json:"bodySHA256"`
	Signature  string    `json:"signature"`
	Body       []byte    `json:"body"`
}

func VerifyWebhookSignature(secret string, body []byte, signatureHeader string) bool {
	signature, ok := strings.CutPrefix(signatureHeader, "sha256=")
	if !ok || signature == "" {
		return false
	}
	expected, err := hex.DecodeString(signature)
	if err != nil || len(expected) != sha256.Size {
		return false
	}
	mac := hmac.New(sha256.New, []byte(secret))
	_, _ = mac.Write(body)
	return hmac.Equal(mac.Sum(nil), expected)
}

func BodyDigest(body []byte) string {
	sum := sha256.Sum256(body)
	return hex.EncodeToString(sum[:])
}

func (e Envelope) Verify(secret string, now time.Time) error {
	if e.DeliveryID == "" || e.BodySHA256 == "" || e.Signature == "" {
		return ErrInvalidSignature
	}
	if BodyDigest(e.Body) != strings.ToLower(e.BodySHA256) {
		return ErrInvalidSignature
	}
	mac := hmac.New(sha256.New, []byte(secret))
	_, _ = mac.Write(e.Body)
	if hex.EncodeToString(mac.Sum(nil)) != strings.ToLower(e.Signature) {
		return ErrInvalidSignature
	}
	if e.ReceivedAt.IsZero() {
		return ErrOutsideWindow
	}
	if now.Sub(e.ReceivedAt) > relayEnvelopeMaxAge || e.ReceivedAt.Sub(now) > relayEnvelopeFuture {
		return ErrOutsideWindow
	}
	return nil
}
