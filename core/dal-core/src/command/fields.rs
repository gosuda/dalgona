#[cfg(feature = "schema")]
use super::cmd::CommandFields;
use super::{
    ApprovalMode, CancelScope, Command, Deserialize, EntryId, Expect, ExportFormat, Mode,
    ModelRoute, Part, PathBuf, Save, Tagged, ThinkingLevel, TurnId, de,
};

// Command decodes through the raw tagged carrier rather than a derived
// internally tagged enum: `SetModel` carries a `ModelRoute`, which reads
// its own raw text, and serde's tagged buffering cannot hand raw bytes
// to a nested member.
#[derive(Deserialize)]
pub(super) struct PromptCommandFields {
    #[serde(default)]
    pub(super) expect: Expect,
    pub(super) content: Vec<Part>,
}

#[derive(Deserialize)]
pub(super) struct TurnCommandFields {
    pub(super) turn: TurnId,
    pub(super) content: Vec<Part>,
}

#[derive(Deserialize)]
pub(super) struct QueuedCommandFields {
    pub(super) turn: TurnId,
}

#[derive(Deserialize)]
pub(super) struct ScopeCommandFields {
    pub(super) scope: CancelScope,
}

#[derive(Deserialize)]
pub(super) struct ModelCommandFields {
    pub(super) model: ModelRoute,
    pub(super) save: Save,
}

#[derive(Deserialize)]
pub(super) struct LevelCommandFields {
    pub(super) level: ThinkingLevel,
    pub(super) save: Save,
}

#[derive(Deserialize)]
pub(super) struct ApprovalCommandFields {
    pub(super) mode: ApprovalMode,
    pub(super) save: Save,
}

#[derive(Deserialize)]
pub(super) struct ModeCommandFields {
    pub(super) mode: Mode,
    pub(super) save: Save,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ScopedModelsCommandFields {
    pub(super) scoped_models: Vec<Box<str>>,
}

#[derive(Deserialize)]
pub(super) struct ExportCommandFields {
    pub(super) path: Option<PathBuf>,
    pub(super) format: ExportFormat,
}

#[derive(Deserialize)]
pub(super) struct CompactCommandFields {
    pub(super) focus: Option<Box<str>>,
}

#[derive(Deserialize)]
pub(super) struct EntryCommandFields {
    pub(super) entry: EntryId,
}

#[derive(Deserialize)]
pub(super) struct NameCommandFields {
    pub(super) name: Box<str>,
}

#[derive(Deserialize)]
pub(super) struct RunCommandFields {
    pub(super) name: Box<str>,
    pub(super) args: Box<str>,
    pub(super) expected: Option<Expect>,
}

impl<'de> Deserialize<'de> for Command {
    #[expect(
        clippy::too_many_lines,
        reason = "one match arm per command variant; splitting would scatter the wire codec"
    )]
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tagged = Tagged::decode(
            deserializer,
            "type",
            &[
                "prompt",
                "steer",
                "follow_up",
                "cancel_queued",
                "cancel",
                "set_model",
                "set_thinking",
                "set_approval",
                "set_mode",
                "compact",
                "move_leaf",
                "fork",
                "clone",
                "rename",
                "set_scoped_models",
                "export",
                "reload_plugins",
                "run",
            ],
        )?;

        match tagged.kind() {
            "prompt" => {
                let wire: PromptCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Prompt {
                    expect: wire.expect,
                    content: wire.content,
                })
            }
            "cancel_queued" => {
                let wire: QueuedCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::CancelQueued { turn: wire.turn })
            }
            "steer" => {
                let wire: TurnCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Steer {
                    turn: wire.turn,
                    content: wire.content,
                })
            }
            "follow_up" => {
                let wire: TurnCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::FollowUp {
                    turn: wire.turn,
                    content: wire.content,
                })
            }
            "cancel" => {
                let wire: ScopeCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Cancel { scope: wire.scope })
            }
            "set_model" => {
                let wire: ModelCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::SetModel {
                    model: wire.model,
                    save: wire.save,
                })
            }
            "set_thinking" => {
                let wire: LevelCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::SetThinking {
                    level: wire.level,
                    save: wire.save,
                })
            }
            "set_approval" => {
                let wire: ApprovalCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::SetApproval {
                    mode: wire.mode,
                    save: wire.save,
                })
            }
            "set_mode" => {
                let wire: ModeCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::SetMode {
                    mode: wire.mode,
                    save: wire.save,
                })
            }
            "compact" => {
                let wire: CompactCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Compact { focus: wire.focus })
            }
            "move_leaf" => {
                let wire: EntryCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::MoveLeaf(wire.entry))
            }
            "fork" => {
                let wire: EntryCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Fork(wire.entry))
            }
            "clone" => Ok(Self::Clone),
            "rename" => {
                let wire: NameCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Rename(wire.name))
            }
            "run" => {
                let wire: RunCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Run {
                    name: wire.name,
                    args: wire.args,
                    expected: wire.expected,
                })
            }
            "set_scoped_models" => {
                let wire: ScopedModelsCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::SetScopedModels(wire.scoped_models))
            }
            "export" => {
                let wire: ExportCommandFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Export {
                    path: wire.path,
                    format: wire.format,
                })
            }
            "reload_plugins" => Ok(Self::ReloadPlugins),
            other => Err(de::Error::custom(format!("unknown command type `{other}`"))),
        }
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for Command {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Command".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <CommandFields as schemars::JsonSchema>::json_schema(generator)
    }
}
