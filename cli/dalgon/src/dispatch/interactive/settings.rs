//! The validated `dal.toml` edit transaction behind the TUI settings picker.
//!
//! Every operation rereads the latest user document, so an edit made in
//! another editor since startup is kept. A product edit is applied to that
//! text, loaded through the core configuration layers and the product's own
//! section decoder, and written atomically with mode 0600 only when both
//! accept it: a refused edit leaves the file byte-identical.

use std::path::PathBuf;

use dal_core::{Config, ProductSetting, SettingEdit};
use dal_tui::TuiError;

use crate::{ProductFactory, edge};

/// Reads, validates, and writes one product's user `dal.toml`.
pub(super) struct ConfigEditor {
    factory: ProductFactory,
    data_root: PathBuf,
    path: PathBuf,
    /// The configuration the running host started with, so a row saved since
    /// then shows both values. It is decoded only when settings open, so a
    /// remote session, which lists no product rows, never decodes it.
    startup: Config,
}

/// Names the product once: some product decoders already start their text
/// with the binary name.
fn product_error(binary: &str, error: &crate::BuildError) -> TuiError {
    let text = error.to_string();
    let prefix = format!("{binary}: ");
    if text.starts_with(&prefix) {
        TuiError::Terminal(text)
    } else {
        TuiError::Terminal(format!("{prefix}dal.toml: {text}"))
    }
}

impl ConfigEditor {
    /// An editor over `path` for the host that started with `startup`.
    pub(super) fn new(
        factory: ProductFactory,
        startup: &Config,
        data_root: PathBuf,
        path: PathBuf,
    ) -> Self {
        Self {
            factory,
            data_root,
            path,
            startup: startup.clone(),
        }
    }

    fn fail(&self, text: impl std::fmt::Display) -> TuiError {
        TuiError::Terminal(format!("{}: {text}", self.factory.binary))
    }

    /// Reads the latest user document; `None` when the file does not exist.
    fn read(&self) -> Result<Option<String>, TuiError> {
        edge::read_user_config(&self.path)
            .map_err(|error| self.fail(format_args!("cannot read dal.toml: {error}")))
    }

    /// Loads `user_toml` through every layer and the product decoder.
    fn validate(&self, user_toml: Option<&str>) -> Result<Vec<ProductSetting>, TuiError> {
        let config = Config::load(
            self.factory.config_product,
            &self.data_root,
            self.factory.defaults,
            user_toml,
        )
        .map_err(|error| self.fail(format_args!("dal.toml: {error}")))?;
        (self.factory.settings)(&config).map_err(|error| product_error(self.factory.binary, &error))
    }

    /// The product's editable rows: the saved values the next start reads
    /// from the latest user document, each with the value the running host
    /// started with when the two differ.
    ///
    /// # Errors
    /// Returns the refusal of the running configuration when the product's
    /// settings check refuses it (startup may only warn, as for an invalid
    /// `[plugin.history]`): its values are unknown, so no row could state
    /// them. Otherwise returns read or validation errors.
    pub(super) fn catalog(&self) -> Result<Vec<ProductSetting>, TuiError> {
        let running = (self.factory.settings)(&self.startup).map_err(|error| {
            TuiError::Terminal(format!(
                "{} · the running configuration was refused; fix dal.toml and restart {}",
                product_error(self.factory.binary, &error),
                self.factory.binary
            ))
        })?;
        let user_toml = self.read()?;
        let mut rows = self.validate(user_toml.as_deref())?;
        for row in &mut rows {
            row.running = running
                .iter()
                .find(|running| running.key == row.key)
                .map(|running| &running.value)
                .filter(|running| **running != row.value)
                .cloned();
        }
        Ok(rows)
    }

    /// Applies one edit to the latest document and writes it once valid.
    pub(super) fn edit(&self, edit: &SettingEdit) -> Result<(), TuiError> {
        let user_toml = self.read()?;
        let candidate = dal_core::edit_user_toml(user_toml.as_deref(), edit)
            .map_err(|error| self.fail(format_args!("cannot update dal.toml: {error}")))?;
        self.validate(Some(&candidate))?;
        self.write(&candidate)
    }

    /// Persists the TUI diagram preference through its typed core update.
    pub(super) fn save_diagrams(&self, config: &mut Config, enabled: bool) -> Result<(), TuiError> {
        let user_toml = self.read()?;
        let updated = config
            .update_tui_diagrams(enabled, user_toml.as_deref())
            .map_err(|error| self.fail(format_args!("cannot update dal.toml: {error}")))?;
        self.validate(Some(&updated))?;
        self.write(&updated)
    }

    /// Replaces the user document atomically with owner-only permissions.
    fn write(&self, text: &str) -> Result<(), TuiError> {
        let dir = self
            .path
            .parent()
            .ok_or_else(|| self.fail("config path has no parent directory"))?;
        dal_store::create_private_dir_all(dir)
            .map_err(|error| self.fail(format_args!("cannot create config directory: {error}")))?;
        dal_store::write_atomic(&self.path, text.as_bytes(), dal_store::FileMode::Mode0600).map_err(
            |error| self.fail(format_args!("cannot save {}: {error}", self.path.display())),
        )
    }
}

#[cfg(test)]
mod tests {
    use dal_core::{
        Config, ConfigProduct, ProductSetting, SettingControl, SettingEdit, SettingValue,
    };

    use super::ConfigEditor;
    use crate::{BuildError, ProductFactory};

    #[derive(Debug, thiserror::Error)]
    #[error("[plugin.demo] limit must be 1 to 5")]
    struct LimitRange;

    /// A product owning `[plugin.demo] limit`, refused outside `1..=5`.
    fn demo_settings(config: &Config) -> Result<Vec<ProductSetting>, BuildError> {
        let limit = config
            .section("plugin.demo")
            .and_then(|demo| demo.get("limit")?.as_integer())
            .unwrap_or(3);
        if !(1..=5).contains(&limit) {
            return Err(BuildError::Section {
                section: "plugin.demo".into(),
                source: Box::new(LimitRange),
            });
        }
        Ok(vec![ProductSetting {
            key: "plugin.demo.limit".into(),
            group: "demo".into(),
            label: "limit".into(),
            value: SettingValue::Integer(limit),
            running: None,
            control: SettingControl::Integer,
        }])
    }

    fn factory() -> ProductFactory {
        ProductFactory {
            config_product: ConfigProduct::Dalgon,
            settings: demo_settings,
            ..crate::product()
        }
    }

    fn editor(dir: &tempfile::TempDir, user_toml: &str) -> (ConfigEditor, std::path::PathBuf) {
        let path = dir.path().join("config").join("dal.toml");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("config dir");
        std::fs::write(&path, user_toml).expect("seed dal.toml");
        let startup = Config::load(ConfigProduct::Dalgon, dir.path(), "", Some(user_toml))
            .expect("startup config");
        let editor = ConfigEditor::new(factory(), &startup, dir.path().to_path_buf(), path.clone());
        (editor, path)
    }

    fn limit(value: i64) -> SettingEdit {
        SettingEdit {
            key: "plugin.demo.limit".into(),
            value: SettingValue::Integer(value),
        }
    }

    #[test]
    fn a_valid_edit_is_written_and_keeps_the_rest_of_the_document() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = "# mine\nmodel = \"a/b\"\n\n[plugin.demo]\nlimit = 2 # note\n\n[plugin.other]\nkeep = true\n";
        let (editor, path) = editor(&dir, user);
        editor.edit(&limit(4)).expect("valid edit saves");
        let written = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(
            written,
            user.replace("limit = 2 # note", "limit = 4 # note")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let rows = editor.catalog().expect("catalog rereads");
        assert_eq!(rows[0].value, SettingValue::Integer(4));
        assert_eq!(rows[0].running, Some(SettingValue::Integer(2)));
    }

    #[test]
    fn a_refused_edit_leaves_the_file_byte_identical_and_names_the_field() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = "[plugin.demo]\nlimit = 2   # spacing kept\n";
        let (editor, path) = editor(&dir, user);
        let before = std::fs::read(&path).expect("read before");
        let error = editor
            .edit(&limit(9))
            .expect_err("out-of-range edit is refused");
        assert!(
            error.to_string().contains("limit must be 1 to 5"),
            "{error}"
        );
        assert!(
            error.to_string().starts_with("dalgon: dal.toml:"),
            "{error}"
        );
        assert_eq!(std::fs::read(&path).expect("read after"), before);

        let error = editor
            .edit(&SettingEdit {
                key: "model".into(),
                value: SettingValue::Integer(1),
            })
            .expect_err("core validation refuses a mistyped key");
        assert!(error.to_string().contains("dal.toml"), "{error}");
        assert_eq!(std::fs::read(&path).expect("read after"), before);
    }

    /// A running configuration the settings check refuses (startup may only
    /// warn, as for an invalid `[plugin.history]`) still yields an editor, so
    /// the TUI starts; opening settings names the refusal instead of showing
    /// rows without their running values.
    #[test]
    fn a_refused_running_configuration_is_explained_not_hidden() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (editor, _path) = editor(&dir, "[plugin.demo]\nlimit = 9\n");
        let error = editor
            .catalog()
            .expect_err("settings name the refused running configuration");
        assert_eq!(
            error.to_string(),
            "dalgon: dal.toml: [plugin.demo] limit must be 1 to 5 · the running configuration was refused; fix dal.toml and restart dalgon"
        );
    }

    #[test]
    fn the_diagram_preference_keeps_its_typed_update() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (editor, path) = editor(&dir, "[tui]\ndiagrams = false # mine\n");
        let mut config = Config::load(ConfigProduct::Dalgon, dir.path(), "", None).expect("config");
        editor
            .save_diagrams(&mut config, true)
            .expect("diagrams save");
        assert!(config.tui().diagrams);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read back"),
            "[tui]\ndiagrams = true # mine\n"
        );

        let broken = "[plugin.demo]\nlimit = 9\n";
        std::fs::write(&path, broken).expect("break the product table");
        editor
            .save_diagrams(&mut config, false)
            .expect_err("a document the product refuses is not rewritten");
        assert_eq!(std::fs::read_to_string(&path).expect("read back"), broken);
    }
}
