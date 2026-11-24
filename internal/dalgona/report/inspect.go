package report

import (
	"cmp"
	"path/filepath"
	"time"
)

const (
	AuthorizationReady       = "ready"
	AuthorizationMissing     = "missing"
	AuthorizationExpired     = "expired"
	AuthorizationUnavailable = "unavailable"

	RestrictionStopped     = "stopped"
	RestrictionRateLimited = "rateLimited"
	RestrictionLocalLimit  = "localLimit"

	rateLimitedMessage = "failure reporting waits for the GitHub rate limit to reset"
	localLimitMessage  = "failure reporting paused: local limit reached"
)

type ConsentReceipt struct {
	SchemaVersion int
	Time          time.Time
	Method        string
	TextHash      string
}

type Restriction struct {
	Kind    string
	Until   time.Time
	Message string
}

type Inspection struct {
	Consent        *ConsentReceipt
	Authorization  string
	Restriction    *Restriction
	QueueCount     int
	UncertainCount int
}

func Inspect(home, method string, now time.Time) Inspection {
	inspection := Inspection{Consent: storedReceipt(home), Authorization: authorization(home, method, now)}
	dir := filepath.Join(home, reportDirName)
	queue, queueErr := readQueueFile(dir)
	state, stateErr := readStateFile(dir)
	if err := cmp.Or(queueErr, stateErr); err != nil {
		inspection.Restriction = &Restriction{Kind: RestrictionStopped, Message: err.Error()}
		return inspection
	}
	inspection.Restriction = restrictionOf(state, now)
	inspection.QueueCount = len(queue)
	inspection.UncertainCount = uncertainCount(state.Receipts)
	return inspection
}

func storedReceipt(home string) *ConsentReceipt {
	stored, err := readConsent(home)
	if err != nil {
		return nil
	}
	return &ConsentReceipt{SchemaVersion: stored.Version, Time: stored.At, Method: stored.Method, TextHash: stored.TextSHA256}
}

func authorization(home, method string, now time.Time) string {
	if method == consentMethodGH {
		if _, err := NewGHSource().resolveExe(); err != nil {
			return AuthorizationUnavailable
		}
		return AuthorizationReady
	}
	if reportAppClientID == "" {
		return AuthorizationUnavailable
	}
	return appAuthorization(home, now)
}

func appAuthorization(home string, now time.Time) string {
	credential, err := readCredential(home)
	switch {
	case err != nil:
		return AuthorizationMissing
	case !credential.Expires.IsZero() && !now.Before(credential.Expires):
		return AuthorizationExpired
	}
	return AuthorizationReady
}

func restrictionOf(state reporterState, now time.Time) *Restriction {
	if state.StoppedStatus != 0 {
		return &Restriction{Kind: RestrictionStopped, Message: latchError(state.StoppedStatus).Error()}
	}
	core := pausedUntil(now, state.CoreRemaining, state.CoreReset, state.CoreRetryAfter)
	search := pausedUntil(now, state.SearchRemaining, state.SearchReset, state.SearchRetryAfter)
	if until := later(core, search); !until.IsZero() {
		return &Restriction{Kind: RestrictionRateLimited, Until: until, Message: rateLimitedMessage}
	}
	today := now.UTC().Format(time.DateOnly)
	if state.RestsDay == today && state.Rests >= maxDayRests || state.CreatesDay == today && state.Creates >= maxDayCreates {
		midnight := now.UTC().Truncate(24 * time.Hour)
		return &Restriction{Kind: RestrictionLocalLimit, Until: midnight.Add(24 * time.Hour), Message: localLimitMessage}
	}
	return nil
}

func pausedUntil(now time.Time, remaining int, reset, retryAfter time.Time) time.Time {
	if !bucketPaused(now, remaining, reset, retryAfter) {
		return time.Time{}
	}
	return later(retryAfter, reset.Add(time.Second))
}

func later(a, b time.Time) time.Time {
	if a.After(b) {
		return a
	}
	return b
}

func uncertainCount(receipts []Receipt) int {
	count := 0
	for _, receipt := range receipts {
		if receipt.State == receiptUncertain {
			count++
		}
	}
	return count
}
