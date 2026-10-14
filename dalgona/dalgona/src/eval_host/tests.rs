// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use super::*;

fn table(text: &str) -> toml::Table {
    text.parse().expect("test toml")
}

fn config(user: Option<&str>, against: Option<&str>, model: Option<&str>) -> Config {
    load_config(&ArmInputs {
        data_root: &std::env::temp_dir(),
        user_toml: user,
        against_toml: against,
        model,
    })
    .expect("config loads")
}

fn error_text(user: Option<&str>, against: Option<&str>) -> String {
    match load_config(&ArmInputs {
        data_root: &std::env::temp_dir(),
        user_toml: user,
        against_toml: against,
        model: None,
    }) {
        Err(error) => error.to_string(),
        Ok(_) => panic!("expected a config error"),
    }
}

#[test]
fn merge_replaces_scalars_and_arrays_whole_and_merges_tables_by_key() {
    let mut base =
        table("model = \"a\"\nlist = [1, 2]\n[t]\nkeep = 1\nover = 1\n[t.deep]\nx = 1\ny = 1\n");
    merge(
        &mut base,
        table("model = \"b\"\nlist = [3]\nadded = true\n[t]\nover = 2\n[t.deep]\ny = 2\n"),
    );
    assert_eq!(
        base,
        table(
            "model = \"b\"\nlist = [3]\nadded = true\n[t]\nkeep = 1\nover = 2\n[t.deep]\nx = 1\ny = 2\n"
        )
    );
}

#[test]
fn a_table_replaces_a_scalar_and_a_scalar_replaces_a_table() {
    let mut base = table("a = 1\n[b]\nx = 1\n");
    merge(&mut base, table("b = 2\n[a]\ny = 1\n"));
    assert_eq!(base, table("b = 2\n[a]\ny = 1\n"));
}

#[test]
fn the_model_flag_beats_every_file() {
    let loaded = config(
        Some("model = \"user/model\"\n"),
        Some("model = \"against/model\"\n"),
        Some("flag/model"),
    );
    assert_eq!(loaded.model(), Some("flag/model"));
}

#[test]
fn the_against_file_sits_on_top_of_the_user_file_key_by_key() {
    let user = "model = \"user/model\"\ndisabled_batteries = [\"web\"]\n";
    let untouched = config(Some(user), Some("approval = \"ask\"\n"), None);
    assert_eq!(untouched.model(), Some("user/model"));
    assert_eq!(untouched.disabled_batteries().len(), 1);

    let replaced = config(
        Some(user),
        Some("model = \"against/model\"\ndisabled_batteries = [\"mcp\", \"review\"]\n"),
        None,
    );
    assert_eq!(replaced.model(), Some("against/model"));
    let names: Vec<_> = replaced
        .disabled_batteries()
        .iter()
        .map(AsRef::<str>::as_ref)
        .collect();
    assert_eq!(names, ["mcp", "review"]);
}

#[test]
fn no_file_means_the_product_defaults_and_no_model() {
    let loaded = config(None, None, None);
    assert_eq!(loaded.model(), None);
}

#[test]
fn a_run_grants_edits_and_never_execution_whatever_the_files_say() {
    for user in ["approval = \"all\"\n", "approval = \"ask\"\n", ""] {
        let loaded = config(Some(user), Some("approval = \"all\"\n"), None);
        assert_eq!(loaded.approval(), ApprovalMode::Edits, "{user:?}");
    }
}

#[test]
fn a_broken_layer_names_itself() {
    let user = error_text(Some("model = \n"), None);
    assert!(user.starts_with("dal.toml: "), "{user}");
    let against = error_text(Some("model = \"a\"\n"), Some("[\n"));
    assert!(against.starts_with("--against: "), "{against}");
}

#[test]
fn a_cost_is_the_sum_of_the_reported_costs_and_unknown_when_none_is() {
    assert_eq!(add_cost(None, None), None);
    assert_eq!(add_cost(None, Some(0.5)), Some(0.5));
    assert_eq!(add_cost(Some(0.5), None), Some(0.5));
    assert_eq!(add_cost(Some(0.5), Some(0.25)), Some(0.75));
}

#[test]
fn only_a_normal_stop_is_ok() {
    assert_eq!(exit_of(Stop::EndTurn), Exit::Ok);
    assert_eq!(exit_of(Stop::Cancelled), Exit::Cancelled);
    assert_eq!(exit_of(Stop::Length), Exit::Failed);
}
