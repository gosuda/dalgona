package rpc

import (
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

type restartItem struct {
	ID       string `json:"id"`
	Type     string `json:"type"`
	Keywords []struct {
		Word string `json:"word"`
	} `json:"keywords"`
}

type restartResume struct {
	Thread struct {
		Turns []struct {
			Items []restartItem `json:"items"`
		} `json:"turns"`
	} `json:"thread"`
}

func TestUserMessageKeywordSurvivesRestart(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	r.cfg.Config.Keywords = map[string]config.KeywordSpec{"probe": {Enabled: true, Text: "probe notice", Requires: []string{"exec"}}}
	reopen(t, r)
	r.llm.push(llmReply{body: sseText("ok")})
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	startTurn(t, c, threadID, "probe this")
	c.collect()
	reopen(t, r)
	var resumed restartResume
	r.connect().ok("thread/resume", map[string]any{"threadId": threadID}, &resumed)
	var users []restartItem
	for _, turn := range resumed.Thread.Turns {
		for _, item := range turn.Items {
			if item.Type == "userMessage" {
				users = append(users, item)
			}
		}
	}
	if len(users) != 1 || len(users[0].Keywords) != 1 || users[0].Keywords[0].Word != "probe" {
		t.Fatalf("resumed user messages = %+v, want one message with the probe keyword", users)
	}
}
