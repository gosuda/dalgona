package rpc

import "encoding/json/jsontext"

func (s ThreadStatus) MarshalJSONTo(enc *jsontext.Encoder) error { return marshalTo(enc, s) }
func (t Thread) MarshalJSONTo(enc *jsontext.Encoder) error       { return marshalTo(enc, t) }
func (t Turn) MarshalJSONTo(enc *jsontext.Encoder) error         { return marshalTo(enc, t) }

func (s ThreadStatus) write(o *out) {
	o.open()
	o.str("type", s.Type)
	if s.Type == "active" {
		o.key("activeFlags")
		o.openArray()
		for _, flag := range s.ActiveFlags {
			o.text(flag)
		}
		o.closeArray()
	}
	o.close()
}

func (t Thread) write(o *out) {
	o.open()
	o.str("id", t.ID)
	o.str("sessionId", t.SessionID)
	o.optStr("name", t.Name)
	o.str("preview", t.Preview)
	o.optStr("model", t.Model)
	o.str("modelProvider", t.ModelProvider)
	o.int("createdAt", t.CreatedAt)
	o.int("updatedAt", t.UpdatedAt)
	o.str("cwd", t.Cwd)
	o.boolean("ephemeral", t.Ephemeral)
	o.str("cliVersion", t.CLIVersion)
	o.str("source", t.Source)
	o.key("projectId")
	o.null()
	o.key("status")
	t.Status.write(o)
	o.optStr("parentThreadId", t.ParentThreadID)
	o.optStr("forkedFromId", t.ForkedFromID)
	o.key("turns")
	o.openArray()
	for _, turn := range t.Turns {
		turn.write(o)
	}
	o.closeArray()
	o.close()
}

func (t Turn) write(o *out) {
	o.open()
	o.str("id", t.ID)
	o.key("items")
	o.openArray()
	for _, it := range t.Items {
		it.write(o)
	}
	o.closeArray()
	o.str("status", t.Status)
	if t.Error != nil {
		o.key("error")
		t.Error.write(o)
	}
	if t.StartedAt != 0 {
		o.int("startedAt", t.StartedAt)
	}
	if t.CompletedAt != 0 {
		o.int("completedAt", t.CompletedAt)
	}
	if t.DurationMs != 0 {
		o.int("durationMs", t.DurationMs)
	}
	o.close()
}

func (e TurnError) write(o *out) {
	o.open()
	o.str("message", e.Message)
	o.close()
}

func (r ThreadStartResponse) write(o *out) {
	o.open()
	o.str("approvalPolicy", r.ApprovalPolicy)
	o.str("approvalsReviewer", r.ApprovalsReviewer)
	o.str("cwd", r.Cwd)
	o.str("model", r.Model)
	o.str("modelProvider", r.ModelProvider)
	o.key("sandbox")
	o.open()
	o.str("type", r.Sandbox.Type)
	o.close()
	o.optStr("reasoningEffort", r.ReasoningEffort)
	if r.TokenUsage != nil {
		o.key("tokenUsage")
		r.TokenUsage.write(o)
	}
	o.key("thread")
	r.Thread.write(o)
	o.close()
}

func (r ThreadListResponse) write(o *out) {
	o.open()
	o.key("data")
	o.openArray()
	for _, thread := range r.Data {
		thread.write(o)
	}
	o.closeArray()
	o.optStr("nextCursor", r.NextCursor)
	o.close()
}

func (r ThreadReadResponse) write(o *out) {
	o.open()
	o.key("thread")
	r.Thread.write(o)
	o.close()
}

func (r TurnStartResponse) write(o *out) {
	o.open()
	o.key("turn")
	r.Turn.write(o)
	o.close()
}

func (r TurnSteerResponse) write(o *out) {
	o.open()
	o.str("turnId", r.TurnID)
	o.close()
}

func (r ModelListResponse) write(o *out) {
	o.open()
	o.key("data")
	o.openArray()
	for _, m := range r.Data {
		m.write(o)
	}
	o.closeArray()
	o.close()
}

func (m Model) write(o *out) {
	o.open()
	o.str("id", m.ID)
	o.str("model", m.Model)
	o.str("displayName", m.DisplayName)
	o.str("description", m.Description)
	o.boolean("hidden", m.Hidden)
	o.boolean("isDefault", m.IsDefault)
	o.str("defaultReasoningEffort", m.DefaultReasoningEffort)
	o.key("supportedReasoningEfforts")
	o.openArray()
	for _, effort := range m.SupportedReasoningEfforts {
		o.open()
		o.str("reasoningEffort", effort.ReasoningEffort)
		o.str("description", effort.Description)
		o.close()
	}
	o.closeArray()
	o.int("contextWindow", int64(m.ContextWindow))
	o.str("provider", m.Provider)
	o.key("presets")
	o.openArray()
	for _, role := range m.Presets {
		o.text(role)
	}
	o.closeArray()
	o.close()
}

func (r ConfigReadResponse) write(o *out) {
	o.open()
	o.key("config")
	o.open()
	o.optStr("model", r.Config.Model)
	o.optStr("model_provider", r.Config.ModelProvider)
	o.optStr("approval_policy", r.Config.ApprovalPolicy)
	o.optStr("sandbox_mode", r.Config.SandboxMode)
	o.optStr("model_reasoning_effort", r.Config.ModelReasoningEffort)
	o.close()
	o.key("origins")
	o.open()
	for key, origin := range r.Origins {
		o.str(key, origin)
	}
	o.close()
	o.close()
}

func (r ConfigWriteResponse) write(o *out) {
	o.open()
	o.str("filePath", r.FilePath)
	o.str("status", r.Status)
	o.str("version", r.Version)
	o.close()
}

func (r AccountReadResponse) write(o *out) {
	o.open()
	o.key("account")
	if r.Account == nil {
		o.null()
	} else {
		o.open()
		o.str("type", r.Account.Type)
		if r.Account.Type == "chatgpt" {
			o.str("email", r.Account.Email)
			o.str("planType", r.Account.PlanType)
		}
		o.close()
	}
	o.boolean("requiresOpenaiAuth", r.RequiresOpenaiAuth)
	o.close()
}

func (r LoginAccountResponse) write(o *out) {
	o.open()
	o.str("type", r.Type)
	o.optStr("loginId", r.LoginID)
	o.optStr("authUrl", r.AuthURL)
	o.optStr("verificationUrl", r.VerificationURL)
	o.optStr("userCode", r.UserCode)
	o.close()
}

func (r CancelLoginAccountResponse) write(o *out) {
	o.open()
	o.str("status", r.Status)
	o.close()
}

func (r SkillsListResponse) write(o *out) {
	o.open()
	o.key("data")
	o.openArray()
	for _, entry := range r.Data {
		o.open()
		o.str("cwd", entry.Cwd)
		o.key("skills")
		o.openArray()
		for _, skill := range entry.Skills {
			skill.write(o)
		}
		o.closeArray()
		o.empty("errors")
		o.close()
	}
	o.closeArray()
	o.close()
}

func (s SkillMetadata) write(o *out) {
	o.open()
	o.str("name", s.Name)
	o.str("description", s.Description)
	o.str("path", s.Path)
	o.str("scope", s.Scope)
	o.boolean("enabled", s.Enabled)
	o.close()
}

type keywordList []Keyword

func (k keywordList) write(o *out) {
	o.openArray()
	for _, keyword := range k {
		o.open()
		o.str("word", keyword.Word)
		o.key("hue")
		o.openArray()
		o.token(jsontext.Int(int64(keyword.Hue[0])))
		o.token(jsontext.Int(int64(keyword.Hue[1])))
		o.closeArray()
		o.close()
	}
	o.closeArray()
}

func (m KeywordMatch) write(o *out) {
	o.open()
	o.str("word", m.Word)
	o.key("byteRange")
	o.open()
	o.int("start", int64(m.ByteRange.Start))
	o.int("end", int64(m.ByteRange.End))
	o.close()
	o.key("hue")
	o.openArray()
	o.token(jsontext.Int(int64(m.Hue[0])))
	o.token(jsontext.Int(int64(m.Hue[1])))
	o.closeArray()
	if m.Effort != "" {
		o.str("effort", m.Effort)
	}
	o.str("summary", m.Summary)
	o.close()
}

func (r KeywordsMatchResponse) write(o *out) {
	o.open()
	o.key("matches")
	o.openArray()
	for _, match := range r.Matches {
		match.write(o)
	}
	o.closeArray()
	o.close()
}

func (r ThreadTreeResponse) write(o *out) {
	o.open()
	o.key("turns")
	o.openArray()
	for _, turn := range r.Turns {
		o.open()
		o.str("turnId", turn.TurnID)
		o.key("parentTurnId")
		if turn.ParentTurnID == "" {
			o.null()
		} else {
			o.text(turn.ParentTurnID)
		}
		o.str("preview", turn.Preview)
		o.boolean("active", turn.Active)
		o.close()
	}
	o.closeArray()
	o.close()
}

type advisorList []AdvisorState

func (a advisorList) write(o *out) {
	o.openArray()
	for _, state := range a {
		o.open()
		o.str("name", state.Name)
		o.str("model", state.Model)
		o.str("status", state.Status)
		o.int("notes", int64(state.Notes))
		o.key("usage")
		o.open()
		o.int("inputTokens", int64(state.Usage.InputTokens))
		o.int("outputTokens", int64(state.Usage.OutputTokens))
		o.close()
		o.close()
	}
	o.closeArray()
}
