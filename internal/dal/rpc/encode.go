package rpc

import "encoding/json/jsontext"

func (s ThreadStatus) MarshalJSONTo(enc *jsontext.Encoder) error { return marshalTo(enc, s) }

func (t Thread) MarshalJSONTo(enc *jsontext.Encoder) error { return marshalTo(enc, t) }

func (t Turn) MarshalJSONTo(enc *jsontext.Encoder) error { return marshalTo(enc, t) }

func (s ThreadStatus) Encode(o *Writer) {
	o.Open()
	o.Str("type", s.Type)
	if s.Type == "active" {
		o.Key("activeFlags")
		o.OpenArray()
		for _, flag := range s.ActiveFlags {
			o.Text(flag)
		}
		o.CloseArray()
	}
	o.Close()
}

func (t Thread) Encode(o *Writer) {
	o.Open()
	o.Str("id", t.ID)
	o.Str("sessionId", t.SessionID)
	o.OptStr("name", t.Name)
	o.Str("preview", t.Preview)
	o.OptStr("model", t.Model)
	o.Str("modelProvider", t.ModelProvider)
	o.Int("createdAt", t.CreatedAt)
	o.Int("updatedAt", t.UpdatedAt)
	o.Str("cwd", t.Cwd)
	o.Bool("ephemeral", t.Ephemeral)
	o.Str("cliVersion", t.CLIVersion)
	o.Str("source", t.Source)
	o.Key("projectId")
	o.Null()
	o.Key("status")
	t.Status.Encode(o)
	o.OptStr("parentThreadId", t.ParentThreadID)
	o.OptStr("forkedFromId", t.ForkedFromID)
	o.Key("turns")
	o.OpenArray()
	for _, turn := range t.Turns {
		turn.Encode(o)
	}
	o.CloseArray()
	o.Close()
}

func (t Turn) Encode(o *Writer) {
	o.Open()
	o.Str("id", t.ID)
	o.Key("items")
	o.OpenArray()
	for _, it := range t.Items {
		it.Encode(o)
	}
	o.CloseArray()
	o.Str("status", t.Status)
	if t.Error != nil {
		o.Key("error")
		t.Error.Encode(o)
	}
	if t.StartedAt != 0 {
		o.Int("startedAt", t.StartedAt)
	}
	if t.CompletedAt != 0 {
		o.Int("completedAt", t.CompletedAt)
	}
	if t.DurationMs != 0 {
		o.Int("durationMs", t.DurationMs)
	}
	o.Close()
}

func (e TurnError) Encode(o *Writer) {
	o.Open()
	o.Str("message", e.Message)
	o.Close()
}

func (r ThreadStartResponse) Encode(o *Writer) {
	o.Open()
	o.Str("approvalPolicy", r.ApprovalPolicy)
	o.Str("approvalsReviewer", r.ApprovalsReviewer)
	o.Str("cwd", r.Cwd)
	o.Str("model", r.Model)
	o.Str("modelProvider", r.ModelProvider)
	o.Key("sandbox")
	o.Open()
	o.Str("type", r.Sandbox.Type)
	o.Close()
	o.OptStr("reasoningEffort", r.ReasoningEffort)
	if r.TurnEffort != nil {
		o.Str("turnEffort", *r.TurnEffort)
	}
	if r.TokenUsage != nil {
		o.Key("tokenUsage")
		r.TokenUsage.Encode(o)
	}
	if r.SandboxFallback != nil {
		o.Key("sandboxFallback")
		r.SandboxFallback.Encode(o)
	}
	o.Key("thread")
	r.Thread.Encode(o)
	o.Close()
}

func (r ThreadListResponse) Encode(o *Writer) {
	o.Open()
	o.Key("data")
	o.OpenArray()
	for _, thread := range r.Data {
		thread.Encode(o)
	}
	o.CloseArray()
	o.OptStr("nextCursor", r.NextCursor)
	o.Close()
}

func (r ThreadReadResponse) Encode(o *Writer) {
	o.Open()
	o.Key("thread")
	r.Thread.Encode(o)
	o.Close()
}

func (r TurnStartResponse) Encode(o *Writer) {
	o.Open()
	o.Key("turn")
	r.Turn.Encode(o)
	o.Close()
}

func (r TurnSteerResponse) Encode(o *Writer) {
	o.Open()
	o.Str("turnId", r.TurnID)
	o.Close()
}

func (r ModelListResponse) Encode(o *Writer) {
	o.Open()
	o.Key("data")
	o.OpenArray()
	for _, m := range r.Data {
		m.Encode(o)
	}
	o.CloseArray()
	o.Close()
}

func (m Model) Encode(o *Writer) {
	o.Open()
	o.Str("id", m.ID)
	o.Str("model", m.Model)
	o.Str("displayName", m.DisplayName)
	o.Str("description", m.Description)
	o.Bool("hidden", m.Hidden)
	o.Bool("isDefault", m.IsDefault)
	o.Str("defaultReasoningEffort", m.DefaultReasoningEffort)
	o.Key("supportedReasoningEfforts")
	o.OpenArray()
	for _, effort := range m.SupportedReasoningEfforts {
		o.Open()
		o.Str("reasoningEffort", effort.ReasoningEffort)
		o.Str("description", effort.Description)
		o.Close()
	}
	o.CloseArray()
	o.Int("contextWindow", int64(m.ContextWindow))
	o.Str("provider", m.Provider)
	o.Key("presets")
	o.OpenArray()
	for _, role := range m.Presets {
		o.Text(role)
	}
	o.CloseArray()
	o.Key("inputModalities")
	o.OpenArray()
	for _, modality := range m.InputModalities {
		o.Text(modality)
	}
	o.CloseArray()
	o.Close()
}

func (r ConfigReadResponse) Encode(o *Writer) {
	o.Open()
	o.Key("config")
	o.Open()
	o.OptStr("model", r.Config.Model)
	o.OptStr("model_provider", r.Config.ModelProvider)
	o.OptStr("approval_policy", r.Config.ApprovalPolicy)
	o.OptStr("sandbox_mode", r.Config.SandboxMode)
	o.OptStr("model_reasoning_effort", r.Config.ModelReasoningEffort)
	o.Close()
	o.Key("origins")
	o.Open()
	for key, origin := range r.Origins {
		o.Str(key, origin)
	}
	o.Close()
	o.Close()
}

func (r ConfigWriteResponse) Encode(o *Writer) {
	o.Open()
	o.Str("filePath", r.FilePath)
	o.Str("status", r.Status)
	o.Str("version", r.Version)
	o.Close()
}

func (r AccountReadResponse) Encode(o *Writer) {
	o.Open()
	o.Key("account")
	if r.Account == nil {
		o.Null()
	} else {
		o.Open()
		o.Str("type", r.Account.Type)
		if r.Account.Type == "chatgpt" {
			o.Str("email", r.Account.Email)
			o.Str("planType", r.Account.PlanType)
		}
		o.Close()
	}
	o.Bool("requiresOpenaiAuth", r.RequiresOpenaiAuth)
	o.Close()
}

func (r LoginAccountResponse) Encode(o *Writer) {
	o.Open()
	o.Str("type", r.Type)
	o.OptStr("loginId", r.LoginID)
	o.OptStr("authUrl", r.AuthURL)
	o.OptStr("verificationUrl", r.VerificationURL)
	o.OptStr("userCode", r.UserCode)
	o.Close()
}

func (r CancelLoginAccountResponse) Encode(o *Writer) {
	o.Open()
	o.Str("status", r.Status)
	o.Close()
}

func (r SkillsListResponse) Encode(o *Writer) {
	o.Open()
	o.Key("data")
	o.OpenArray()
	for _, entry := range r.Data {
		o.Open()
		o.Str("cwd", entry.Cwd)
		o.Key("skills")
		o.OpenArray()
		for _, skill := range entry.Skills {
			skill.Encode(o)
		}
		o.CloseArray()
		o.Empty("errors")
		o.Close()
	}
	o.CloseArray()
	o.Close()
}

func (s SkillMetadata) Encode(o *Writer) {
	o.Open()
	o.Str("name", s.Name)
	o.Str("description", s.Description)
	o.Str("path", s.Path)
	o.Str("scope", s.Scope)
	o.Bool("enabled", s.Enabled)
	o.Close()
}

func (m KeywordMatch) Encode(o *Writer) {
	o.Open()
	o.Str("word", m.Word)
	o.Key("byteRange")
	o.Open()
	o.Int("start", int64(m.ByteRange.Start))
	o.Int("end", int64(m.ByteRange.End))
	o.Close()
	o.Key("hue")
	o.OpenArray()
	o.Token(jsontext.Int(int64(m.Hue[0])))
	o.Token(jsontext.Int(int64(m.Hue[1])))
	o.CloseArray()
	if m.Effort != "" {
		o.Str("effort", m.Effort)
	}
	o.Str("summary", m.Summary)
	o.Close()
}

func (r KeywordsMatchResponse) Encode(o *Writer) {
	o.Open()
	o.Key("matches")
	o.OpenArray()
	for _, match := range r.Matches {
		match.Encode(o)
	}
	o.CloseArray()
	o.Close()
}

func (r ThreadTreeResponse) Encode(o *Writer) {
	o.Open()
	o.Key("turns")
	o.OpenArray()
	for _, turn := range r.Turns {
		o.Open()
		o.Str("turnId", turn.TurnID)
		o.Key("parentTurnId")
		if turn.ParentTurnID == "" {
			o.Null()
		} else {
			o.Text(turn.ParentTurnID)
		}
		o.Str("preview", turn.Preview)
		o.Bool("active", turn.Active)
		o.Close()
	}
	o.CloseArray()
	o.Close()
}
