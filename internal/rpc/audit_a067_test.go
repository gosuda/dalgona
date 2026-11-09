package rpc

import (
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestAuditSettingKeyRejectsBadEffort(t *testing.T) {
	if _, _, err := settingKey("model_reasoning_effort", "loud"); err == nil {
		t.Fatal("loud effort accepted")
	}
	if _, _, err := settingKey("model_reasoning_effort", 3); err == nil {
		t.Fatal("numeric effort accepted")
	}
}

func TestAuditSettingKeyRejectsNonStringPolicy(t *testing.T) {
	if _, _, err := settingKey("approval_policy", true); err == nil {
		t.Fatal("bool approval accepted")
	}
	if _, _, err := settingKey("sandbox_mode", 7); err == nil {
		t.Fatal("numeric sandbox accepted")
	}
}

func TestAuditSettingKeyRejectsUnmappedAndNumeric(t *testing.T) {
	if _, _, err := settingKey("compaction_threshold", 0.5); err == nil {
		t.Fatal("unmapped numeric key accepted")
	}
	if _, _, err := settingKey("wat", "x"); err == nil {
		t.Fatal("unknown key accepted")
	}
	if _, _, err := settingKey("model", 3); err == nil {
		t.Fatal("numeric model accepted")
	}
	if _, _, err := settingKey("", "x"); err == nil {
		t.Fatal("empty keyPath accepted")
	}
}

func checkAuditDirectSettingKey(t *testing.T, keyPath, input, wantKey, wantValue string) {
	t.Helper()
	key, value, err := settingKey(keyPath, input)
	if err != nil || key != wantKey || value != wantValue {
		t.Fatalf("direct %s rejected: %q %v %v", keyPath, key, value, err)
	}
}

func TestAuditSettingKeyAcceptsDirectKeys(t *testing.T) {
	checkAuditDirectSettingKey(t, "model", "openai/gpt-x", "model", "openai/gpt-x")
	checkAuditDirectSettingKey(t, "approval", "yolo", "approval", "yolo")
	checkAuditDirectSettingKey(t, "sandbox", "off", "sandbox", string(config.SandboxOff))
	checkAuditDirectSettingKey(t, "reasoning", "low", "reasoning", "low")
}

func TestAuditConfigWriteRejectsUnsupportedKeepsFile(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	c.fails("config/value/write", map[string]any{"keyPath": "compaction_threshold", "value": 0.5}, codeInvalidParams, "unsupported keyPath compaction_threshold")
	c.fails("config/value/write", map[string]any{"keyPath": "model", "value": 3}, codeInvalidParams, "model must be a string")
	var cfg ConfigReadResponse
	c.ok("config/read", map[string]any{}, &cfg)
	if cfg.Config.Model != "openai/gpt-test" {
		t.Fatalf("rejected write changed memory: %+v", cfg.Config)
	}
}
