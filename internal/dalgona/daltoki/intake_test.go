package daltoki

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"testing"
	"time"
)

func signBody(secret string, body []byte) string {
	mac := hmac.New(sha256.New, []byte(secret))
	_, _ = mac.Write(body)
	return "sha256=" + hex.EncodeToString(mac.Sum(nil))
}

func validEnvelope(secret string, now time.Time) (Envelope, []byte) {
	body := []byte(`{"zen":"keep it simple"}`)
	envelope := Envelope{
		DeliveryID: "delivery-1",
		Event:      "issues",
		ReceivedAt: now,
		BodySHA256: BodyDigest(body),
		Body:       body,
	}
	envelope.Signature = signBody(secret, body)[len("sha256="):]
	return envelope, body
}

func TestRawSignature(t *testing.T) {
	secret := "webhook-secret"
	_, body := validEnvelope(secret, time.Now())
	good := signBody(secret, body)
	if !VerifyWebhookSignature(secret, body, good) {
		t.Fatal("valid signature rejected")
	}
	if VerifyWebhookSignature("other-secret", body, good) {
		t.Fatal("wrong secret accepted")
	}
	if VerifyWebhookSignature(secret, []byte(`{"zen":"tampered"}`), good) {
		t.Fatal("tampered body accepted")
	}
	if VerifyWebhookSignature(secret, body, "sha256=00") {
		t.Fatal("short signature accepted")
	}
	if VerifyWebhookSignature(secret, body, hex.EncodeToString([]byte("no-prefix"))) {
		t.Fatal("missing prefix accepted")
	}
}

func TestEnvelopeVerification(t *testing.T) {
	secret := "webhook-secret"
	now := time.Now()
	valid, body := validEnvelope(secret, now)
	if err := valid.Verify(secret, now); err != nil {
		t.Fatalf("Verify valid: %v", err)
	}
	tampered := valid
	tampered.Body = []byte(`{"zen":"tampered"}`)
	if err := tampered.Verify(secret, now); !errors.Is(err, ErrInvalidSignature) {
		t.Fatalf("tampered digest err = %v", err)
	}
	forged := valid
	forged.Signature = signBody("forged", body)[len("sha256="):]
	if err := forged.Verify(secret, now); !errors.Is(err, ErrInvalidSignature) {
		t.Fatalf("forged signature err = %v", err)
	}
}

func TestEnvelopeReplayWindow(t *testing.T) {
	secret := "webhook-secret"
	now := time.Now()
	valid, _ := validEnvelope(secret, now)
	old := valid
	old.ReceivedAt = now.Add(-relayEnvelopeMaxAge - time.Minute)
	if err := old.Verify(secret, now); !errors.Is(err, ErrOutsideWindow) {
		t.Fatalf("stale envelope err = %v", err)
	}
	future := valid
	future.ReceivedAt = now.Add(relayEnvelopeFuture + time.Minute)
	if err := future.Verify(secret, now); !errors.Is(err, ErrOutsideWindow) {
		t.Fatalf("future envelope err = %v", err)
	}
	skewed := valid
	skewed.ReceivedAt = now.Add(30 * time.Second)
	if err := skewed.Verify(secret, now); err != nil {
		t.Fatalf("small clock skew rejected: %v", err)
	}
}
