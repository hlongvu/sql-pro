//! Ported from TableProTests/Core/ChangeTracking/SQLStatementGenerator*Tests
//! (core suite + composite PK + no-PK + parameter styles + MSSQL + schema +
//! ordering + generated columns + all-defaults). Names follow the Swift
//! originals; assertions check the same SQL fragments and parameter counts.

use std::collections::{BTreeMap, BTreeSet};

use sqlpro_core::change::{CellChange, ChangeType, RowChange, RowID};
use sqlpro_core::database_type::DatabaseType;
use sqlpro_core::sqlgen::{
    quote_identifier_from_dialect, SqlDialect, SqlStatementGenerator,
};
use sqlpro_core::value::{BindValue, CellValue};

fn db(s: &str) -> DatabaseType {
    DatabaseType::new(s)
}

fn text(s: &str) -> CellValue {
    CellValue::text(s)
}

fn insert_change(row_id: RowID) -> RowChange {
    RowChange::new(row_id, ChangeType::Insert)
}

fn make_generator(
    table_name: &str,
    columns: Vec<&str>,
    primary_key_columns: Vec<&str>,
    database_type: DatabaseType,
) -> SqlStatementGenerator {
    SqlStatementGenerator::simple(
        table_name,
        columns.into_iter().map(String::from).collect(),
        primary_key_columns.into_iter().map(String::from).collect(),
        database_type,
    )
}

fn run(generator: &SqlStatementGenerator, changes: Vec<RowChange>, inserted: Vec<(i64, Vec<CellValue>)>, inserted_ids: Vec<i64>, deleted_ids: Vec<i64>) -> Vec<(String, Vec<Option<BindValue>>)> {
    let inserted_row_data: BTreeMap<RowID, Vec<CellValue>> = inserted
        .into_iter()
        .map(|(ix, values)| (RowID::Existing(ix), values))
        .collect();
    let deleted: BTreeSet<RowID> = deleted_ids.into_iter().map(RowID::Existing).collect();
    let inserted_set: BTreeSet<RowID> = inserted_ids.into_iter().map(RowID::Existing).collect();
    generator
        .generate_statements(&changes, &inserted_row_data, &deleted, &inserted_set)
        .into_iter()
        .map(|s| (s.sql, s.parameters))
        .collect()
}

fn param_text(params: &[Option<BindValue>], ix: usize) -> String {
    match &params[ix] {
        Some(BindValue::Text(s)) => s.clone(),
        other => panic!("expected text param at {ix}, got {other:?}"),
    }
}

fn param_null(params: &[Option<BindValue>], ix: usize) {
    assert!(params[ix].is_none(), "expected NULL at {ix}");
}

// ---- INSERT (testSimpleInsertMySQL etc.) ----

#[test]
fn simple_insert_mysql() {
    let g = make_generator("users", vec!["id", "name", "email"], vec!["id"], db("mysql"));
    let changes = vec![insert_change(RowID::Existing(0))];
    let statements = run(
        &g,
        changes,
        vec![(0, vec![text("1"), text("John"), text("john@example.com")])],
        vec![0],
        vec![],
    );
    assert_eq!(statements.len(), 1);
    let (sql, params) = &statements[0];
    assert!(sql.contains("INSERT INTO"));
    assert!(sql.contains("`users`"));
    assert!(sql.contains("`id`"));
    assert!(sql.contains("`name`"));
    assert!(sql.contains("`email`"));
    assert!(sql.contains('?'));
    assert_eq!(params.len(), 3);
    assert_eq!(param_text(params, 0), "1");
    assert_eq!(param_text(params, 1), "John");
    assert_eq!(param_text(params, 2), "john@example.com");
}

#[test]
fn insert_with_null_value() {
    let g = make_generator("users", vec!["id", "name", "email"], vec!["id"], db("mysql"));
    let changes = vec![insert_change(RowID::Existing(0))];
    let statements = run(
        &g,
        changes,
        vec![(0, vec![text("1"), text("John"), CellValue::Null])],
        vec![0],
        vec![],
    );
    assert_eq!(statements.len(), 1);
    let (_, params) = &statements[0];
    assert_eq!(params.len(), 3);
    param_null(params, 2);
}

#[test]
fn insert_skips_default_columns() {
    let g = make_generator("users", vec!["id", "name", "email"], vec!["id"], db("mysql"));
    let changes = vec![insert_change(RowID::Existing(0))];
    let statements = run(
        &g,
        changes,
        vec![(0, vec![text("__DEFAULT__"), text("John"), text("john@example.com")])],
        vec![0],
        vec![],
    );
    assert_eq!(statements.len(), 1);
    let (sql, params) = &statements[0];
    assert!(!sql.contains("`id`"));
    assert!(sql.contains("`name`"));
    assert!(sql.contains("`email`"));
    assert_eq!(params.len(), 2);
}

#[test]
fn insert_all_default_names_no_column() {
    for (database_type, expected) in [
        ("mysql", "() VALUES ()"),
        ("postgresql", "DEFAULT VALUES"),
        ("sqlite", "DEFAULT VALUES"),
    ] {
        let g = make_generator("users", vec!["id", "name", "email"], vec!["id"], db(database_type));
        let changes = vec![insert_change(RowID::Existing(0))];
        let statements = run(
            &g,
            changes,
            vec![(0, vec![text("__DEFAULT__"), text("__DEFAULT__"), text("__DEFAULT__")])],
            vec![0],
            vec![],
        );
        assert_eq!(statements.len(), 1, "{database_type}");
        let (sql, params) = &statements[0];
        assert!(sql.starts_with("INSERT INTO "), "{database_type}: {sql}");
        assert!(sql.ends_with(expected), "{database_type}: {sql}");
        assert!(params.is_empty());
    }
}

#[test]
fn insert_from_cell_changes_fallback() {
    let g = make_generator("users", vec!["id", "name", "email"], vec!["id"], db("mysql"));
    let mut changes = insert_change(RowID::Existing(0));
    changes.cell_changes = vec![
        CellChange::new(0, "id", CellValue::Null, text("1")),
        CellChange::new(1, "name", CellValue::Null, text("John")),
        CellChange::new(2, "email", CellValue::Null, text("john@example.com")),
    ];
    let statements = run(&g, vec![changes], vec![], vec![0], vec![]);
    assert_eq!(statements.len(), 1);
    assert_eq!(statements[0].1.len(), 3);
}

#[test]
fn insert_with_sql_function() {
    let g = make_generator("users", vec!["id", "name", "email"], vec!["id"], db("mysql"));
    let changes = vec![insert_change(RowID::Existing(0))];
    let statements = run(
        &g,
        changes,
        vec![(0, vec![text("1"), text("John"), text("NOW()")])],
        vec![0],
        vec![],
    );
    assert_eq!(statements.len(), 1);
    let (sql, params) = &statements[0];
    assert!(sql.contains("NOW()"));
    assert_eq!(params.len(), 2);
}

#[test]
fn insert_postgresql_placeholders() {
    let g = make_generator("users", vec!["id", "name", "email"], vec!["id"], db("postgresql"));
    let changes = vec![insert_change(RowID::Existing(0))];
    let statements = run(
        &g,
        changes,
        vec![(0, vec![text("1"), text("John"), text("john@example.com")])],
        vec![0],
        vec![],
    );
    assert_eq!(statements.len(), 1);
    let (sql, _) = &statements[0];
    assert!(sql.contains("$1"));
    assert!(sql.contains("$2"));
    assert!(sql.contains("$3"));
}

#[test]
fn table_name_quoted() {
    let g = make_generator("my_table", vec!["id", "name", "email"], vec!["id"], db("mysql"));
    let statements = run(&g, vec![insert_change(RowID::Existing(0))], vec![(0, vec![text("1"), text("J"), text("e")])], vec![0], vec![]);
    assert!(statements[0].0.contains("`my_table`"));
}

#[test]
fn column_names_quoted() {
    let g = make_generator("users", vec!["user_id", "full_name", "email_address"], vec!["user_id"], db("mysql"));
    let statements = run(&g, vec![insert_change(RowID::Existing(0))], vec![(0, vec![text("1"), text("J"), text("e")])], vec![0], vec![]);
    let (sql, _) = &statements[0];
    assert!(sql.contains("`user_id`"));
    assert!(sql.contains("`full_name`"));
    assert!(sql.contains("`email_address`"));
}

#[test]
fn insert_multiple_rows_separate_statements() {
    let g = make_generator("users", vec!["id", "name", "email"], vec!["id"], db("mysql"));
    let changes = vec![insert_change(RowID::Existing(0)), insert_change(RowID::Existing(1))];
    let statements = run(
        &g,
        changes,
        vec![
            (0, vec![text("1"), text("John"), text("john@example.com")]),
            (1, vec![text("2"), text("Jane"), text("jane@example.com")]),
        ],
        vec![0, 1],
        vec![],
    );
    assert_eq!(statements.len(), 2);
    assert_eq!(param_text(&statements[0].1, 1), "John");
    assert_eq!(param_text(&statements[1].1, 1), "Jane");
}

#[test]
fn insert_only_processes_inserted_rows() {
    // An insert change whose row is no longer marked inserted is skipped.
    let g = make_generator("users", vec!["id", "name", "email"], vec!["id"], db("mysql"));
    let changes = vec![insert_change(RowID::Existing(0))];
    let statements = run(
        &g,
        changes,
        vec![(0, vec![text("1"), text("John"), text("j@e.com")])],
        vec![], // no longer inserted
        vec![],
    );
    assert!(statements.is_empty());
}

#[test]
fn insert_multi_row_batch() {
    let g = make_generator("users", vec!["id", "name"], vec!["id"], db("mysql"));
    let stmt = g
        .insert_statement_multi_row(
            &["id".into(), "name".into()],
            &[vec![text("1"), text("a")], vec![text("2"), text("b")]],
        )
        .unwrap();
    assert_eq!(
        stmt.sql,
        "INSERT INTO `users` (`id`, `name`) VALUES (?, ?), (?, ?)"
    );
    assert_eq!(stmt.parameters.len(), 4);
    assert_eq!(param_text(&stmt.parameters, 0), "1");
    assert_eq!(param_text(&stmt.parameters, 3), "b");
}

#[test]
fn multi_row_insert_mismatched_width_is_nil() {
    let g = make_generator("users", vec!["id", "name"], vec!["id"], db("mysql"));
    assert!(g.insert_statement_multi_row(&["id".into(), "name".into()], &[vec![text("1")]]).is_none());
}

// ---- UPDATE ----

#[test]
fn simple_update_mysql() {
    let g = make_generator("users", vec!["id", "name", "email"], vec!["id"], db("mysql"));
    let mut change = RowChange::new(RowID::Existing(0), ChangeType::Update);
    change.cell_changes.push(CellChange::new(1, "name", text("John"), text("Johnny")));
    change.original_row = Some(vec![text("1"), text("John"), text("john@example.com")]);

    let statements = run(&g, vec![change], vec![], vec![], vec![]);
    assert_eq!(statements.len(), 1);
    let (sql, params) = &statements[0];
    assert!(sql.contains("UPDATE"));
    assert!(sql.contains("SET"));
    assert!(sql.contains("`name` = ?"));
    assert!(sql.contains("WHERE"));
    assert!(sql.contains("`id` = ?"));
    assert_eq!(params.len(), 2);
    assert_eq!(param_text(params, 0), "Johnny");
    assert_eq!(param_text(params, 1), "1");
}

#[test]
fn update_multiple_columns() {
    let g = make_generator("users", vec!["id", "name", "email"], vec!["id"], db("mysql"));
    let mut change = RowChange::new(RowID::Existing(0), ChangeType::Update);
    change.cell_changes.push(CellChange::new(1, "name", text("John"), text("Johnny")));
    change.cell_changes.push(CellChange::new(2, "email", text("john@example.com"), text("johnny@example.com")));
    change.original_row = Some(vec![text("1"), text("John"), text("john@example.com")]);

    let statements = run(&g, vec![change], vec![], vec![], vec![]);
    assert_eq!(statements.len(), 1);
    let (sql, params) = &statements[0];
    assert!(sql.contains("`name` = ?"));
    assert!(sql.contains("`email` = ?"));
    assert_eq!(params.len(), 3);
}

#[test]
fn update_with_null_new_value() {
    let g = make_generator("users", vec!["id", "name", "email"], vec!["id"], db("mysql"));
    let mut change = RowChange::new(RowID::Existing(0), ChangeType::Update);
    change.cell_changes.push(CellChange::new(2, "email", text("john@example.com"), CellValue::Null));
    change.original_row = Some(vec![text("1"), text("John"), text("john@example.com")]);

    let statements = run(&g, vec![change], vec![], vec![], vec![]);
    assert_eq!(statements.len(), 1);
    param_null(&statements[0].1, 0);
}

#[test]
fn update_with_default_value() {
    let g = make_generator("users", vec!["id", "name", "email"], vec!["id"], db("mysql"));
    let mut change = RowChange::new(RowID::Existing(0), ChangeType::Update);
    change.cell_changes.push(CellChange::new(1, "name", text("John"), text("__DEFAULT__")));
    change.original_row = Some(vec![text("1"), text("John"), text("john@example.com")]);

    let statements = run(&g, vec![change], vec![], vec![], vec![]);
    assert_eq!(statements.len(), 1);
    assert!(statements[0].0.contains("`name` = DEFAULT"));
    assert_eq!(statements[0].1.len(), 1); // only the PK bind
}

#[test]
fn update_with_sql_function() {
    let g = make_generator("users", vec!["id", "name", "email"], vec!["id"], db("mysql"));
    let mut change = RowChange::new(RowID::Existing(0), ChangeType::Update);
    change.cell_changes.push(CellChange::new(1, "name", text("John"), text("NOW()")));
    change.original_row = Some(vec![text("1"), text("John"), text("john@example.com")]);

    let statements = run(&g, vec![change], vec![], vec![], vec![]);
    assert_eq!(statements.len(), 1);
    let (sql, params) = &statements[0];
    assert!(sql.contains("NOW()"));
    assert_eq!(params.len(), 1); // only the PK bind
}

#[test]
fn update_pk_from_original_row() {
    // Editing a PK column: the WHERE uses the pre-edit value.
    let g = make_generator("users", vec!["id", "name"], vec!["id"], db("mysql"));
    let mut change = RowChange::new(RowID::Existing(0), ChangeType::Update);
    change.cell_changes.push(CellChange::new(0, "id", text("1"), text("99")));
    change.original_row = Some(vec![text("1"), text("John")]);

    let statements = run(&g, vec![change], vec![], vec![], vec![]);
    assert_eq!(statements.len(), 1);
    let (sql, params) = &statements[0];
    assert!(sql.contains("`id` = ?"));
    assert_eq!(param_text(params, 0), "99"); // SET binds first
    assert_eq!(param_text(params, 1), "1"); // WHERE binds the original
}

#[test]
fn update_null_pk_skips_statement() {
    let g = make_generator("users", vec!["id", "name"], vec!["id"], db("mysql"));
    let mut change = RowChange::new(RowID::Existing(0), ChangeType::Update);
    change.cell_changes.push(CellChange::new(1, "name", text("a"), text("b")));
    change.original_row = Some(vec![CellValue::Null, text("a")]);

    let statements = run(&g, vec![change], vec![], vec![], vec![]);
    assert!(statements.is_empty());
}

#[test]
fn generated_column_dropped_from_update() {
    let mut g = make_generator("users", vec!["id", "name", "full"], vec!["id"], db("mysql"));
    g.generated_columns = BTreeSet::from(["full".to_string()]);
    let mut change = RowChange::new(RowID::Existing(0), ChangeType::Update);
    change.cell_changes.push(CellChange::new(1, "name", text("a"), text("b")));
    change.cell_changes.push(CellChange::new(2, "full", text("x"), text("y")));
    change.original_row = Some(vec![text("1"), text("a"), text("x")]);

    let statements = run(&g, vec![change], vec![], vec![], vec![]);
    assert_eq!(statements.len(), 1);
    assert!(!statements[0].0.contains("`full`"));
    assert!(statements[0].0.contains("`name`"));
}

#[test]
fn update_of_only_generated_column_produces_nothing() {
    let mut g = make_generator("users", vec!["id", "name", "full"], vec!["id"], db("mysql"));
    g.generated_columns = BTreeSet::from(["full".to_string()]);
    let mut change = RowChange::new(RowID::Existing(0), ChangeType::Update);
    change.cell_changes.push(CellChange::new(2, "full", text("x"), text("y")));
    change.original_row = Some(vec![text("1"), text("a"), text("x")]);

    let statements = run(&g, vec![change], vec![], vec![], vec![]);
    assert!(statements.is_empty());
}

// ---- No PK: match every column ----

#[test]
fn update_no_primary_key() {
    let g = make_generator("users", vec!["name", "email"], vec![], db("mysql"));
    let mut change = RowChange::new(RowID::Existing(0), ChangeType::Update);
    change.cell_changes.push(CellChange::new(1, "email", text("a@e"), text("b@e")));
    change.original_row = Some(vec![text("John"), text("a@e")]);

    let statements = run(&g, vec![change], vec![], vec![], vec![]);
    assert_eq!(statements.len(), 1);
    let (sql, params) = &statements[0];
    assert!(sql.contains("`name` = ?"));
    assert!(sql.contains("`email` = ?"));
    assert_eq!(params.len(), 3);
    assert_eq!(param_text(params, 0), "b@e");
    assert_eq!(param_text(params, 1), "John");
    assert_eq!(param_text(params, 2), "a@e");
}

#[test]
fn update_no_pk_with_null() {
    let g = make_generator("users", vec!["name", "email"], vec![], db("mysql"));
    let mut change = RowChange::new(RowID::Existing(0), ChangeType::Update);
    change.cell_changes.push(CellChange::new(1, "email", CellValue::Null, text("b@e")));
    change.original_row = Some(vec![text("John"), CellValue::Null]);

    let statements = run(&g, vec![change], vec![], vec![], vec![]);
    assert_eq!(statements.len(), 1);
    assert!(statements[0].0.contains("`email` IS NULL"));
    assert!(statements[0].0.contains("`name` = ?"));
    assert_eq!(statements[0].1.len(), 2); // SET + name match
}

#[test]
fn update_no_pk_missing_original_row() {
    let g = make_generator("users", vec!["name", "email"], vec![], db("mysql"));
    let mut change = RowChange::new(RowID::Existing(0), ChangeType::Update);
    change.cell_changes.push(CellChange::new(1, "email", text("a"), text("b")));
    change.original_row = None;

    let statements = run(&g, vec![change], vec![], vec![], vec![]);
    assert!(statements.is_empty());
}

#[test]
fn delete_requires_original_row() {
    let g = make_generator("users", vec!["id", "name"], vec!["id"], db("mysql"));
    let mut change = RowChange::new(RowID::Existing(0), ChangeType::Delete);
    change.original_row = None;
    let statements = run(&g, vec![change], vec![], vec![], vec![0]);
    assert!(statements.is_empty());
}

// ---- DELETE: batching and shapes ----

#[test]
fn batch_delete_with_pk() {
    let g = make_generator("users", vec!["id", "name"], vec!["id"], db("mysql"));
    let mut c1 = RowChange::new(RowID::Existing(0), ChangeType::Delete);
    c1.original_row = Some(vec![text("1"), text("a")]);
    let mut c2 = RowChange::new(RowID::Existing(1), ChangeType::Delete);
    c2.original_row = Some(vec![text("2"), text("b")]);

    let statements = run(&g, vec![c1, c2], vec![], vec![], vec![0, 1]);
    assert_eq!(statements.len(), 1);
    let (sql, params) = &statements[0];
    assert_eq!(sql, "DELETE FROM `users` WHERE `id` = ? OR `id` = ?");
    assert_eq!(params.len(), 2);
    assert_eq!(param_text(params, 0), "1");
    assert_eq!(param_text(params, 1), "2");
}

#[test]
fn delete_with_null_pk_skips_row() {
    let g = make_generator("users", vec!["id", "name"], vec!["id"], db("mysql"));
    let mut change = RowChange::new(RowID::Existing(0), ChangeType::Delete);
    change.original_row = Some(vec![CellValue::Null, text("a")]);
    let statements = run(&g, vec![change], vec![], vec![], vec![0]);
    assert!(statements.is_empty());
}

#[test]
fn delete_no_pk_includes_null_match() {
    let g = make_generator("users", vec!["name", "email"], vec![], db("mysql"));
    let mut change = RowChange::new(RowID::Existing(0), ChangeType::Delete);
    change.original_row = Some(vec![text("John"), CellValue::Null]);
    let statements = run(&g, vec![change], vec![], vec![], vec![0]);
    assert_eq!(statements.len(), 1);
    let (sql, params) = &statements[0];
    assert!(sql.contains("`name` = ?"));
    assert!(sql.contains("`email` IS NULL"));
    assert_eq!(params.len(), 1);
}

#[test]
fn delete_all_rows_statement() {
    let g = make_generator("users", vec!["id"], vec!["id"], db("mysql"));
    assert_eq!(g.delete_all_rows_statement(), "DELETE FROM `users`");
}

#[test]
fn delete_before_insert() {
    // Delete frees a unique value; insert takes it. Order is load-bearing.
    let g = make_generator("users", vec!["id", "name"], vec!["id"], db("mysql"));
    let mut del = RowChange::new(RowID::Existing(0), ChangeType::Delete);
    del.original_row = Some(vec![text("1"), text("old")]);
    del.sequence = 0;
    let mut ins = RowChange::new(RowID::Existing(1), ChangeType::Insert);
    ins.sequence = 1;

    let inserted: BTreeMap<RowID, Vec<CellValue>> =
        BTreeMap::from([(RowID::Existing(1), vec![text("1"), text("new")])]);
    let mut deleted = std::collections::BTreeSet::new();
    deleted.insert(RowID::Existing(0));
    let mut inserted_ids = std::collections::BTreeSet::new();
    inserted_ids.insert(RowID::Existing(1));

    let statements = g.generate_statements(&[ins, del], &inserted, &deleted, &inserted_ids);
    assert_eq!(statements.len(), 2);
    assert!(statements[0].sql.starts_with("DELETE FROM"));
    assert!(statements[1].sql.starts_with("INSERT INTO"));
}

#[test]
fn mixed_order_is_preserved() {
    let g = make_generator("users", vec!["id", "name"], vec!["id"], db("mysql"));
    let mut del1 = RowChange::new(RowID::Existing(0), ChangeType::Delete);
    del1.original_row = Some(vec![text("1"), text("a")]);
    del1.sequence = 0;
    let mut ins = RowChange::new(RowID::Existing(1), ChangeType::Insert);
    ins.sequence = 1;
    let mut del2 = RowChange::new(RowID::Existing(2), ChangeType::Delete);
    del2.original_row = Some(vec![text("3"), text("c")]);
    del2.sequence = 2;

    let inserted: BTreeMap<RowID, Vec<CellValue>> =
        BTreeMap::from([(RowID::Existing(1), vec![text("2"), text("b")])]);
    let mut deleted = std::collections::BTreeSet::new();
    deleted.insert(RowID::Existing(0));
    deleted.insert(RowID::Existing(2));
    let mut inserted_ids = std::collections::BTreeSet::new();
    inserted_ids.insert(RowID::Existing(1));

    let statements = g.generate_statements(&[del1, ins, del2], &inserted, &deleted, &inserted_ids);
    assert_eq!(statements.len(), 3);
    assert!(statements[0].sql.starts_with("DELETE FROM"));
    assert!(statements[1].sql.starts_with("INSERT INTO"));
    assert!(statements[2].sql.starts_with("DELETE FROM"));
}

#[test]
fn sequence_order_beats_array_order() {
    // changes arrive in array order [insert(seq1), delete(seq0)] — the
    // delete must still come first because of its sequence.
    let g = make_generator("users", vec!["id", "name"], vec!["id"], db("mysql"));
    let mut del = RowChange::new(RowID::Existing(0), ChangeType::Delete);
    del.original_row = Some(vec![text("1"), text("a")]);
    del.sequence = 0;
    let mut ins = RowChange::new(RowID::Existing(1), ChangeType::Insert);
    ins.sequence = 1;

    let inserted: BTreeMap<RowID, Vec<CellValue>> =
        BTreeMap::from([(RowID::Existing(1), vec![text("1"), text("b")])]);
    let mut deleted = std::collections::BTreeSet::new();
    deleted.insert(RowID::Existing(0));
    let mut inserted_ids = std::collections::BTreeSet::new();
    inserted_ids.insert(RowID::Existing(1));

    let statements = g.generate_statements(&[ins, del], &inserted, &deleted, &inserted_ids);
    assert!(statements[0].sql.starts_with("DELETE FROM"));
}

// ---- Parameter styles per engine ----

#[test]
fn engine_default_parameter_styles() {
    let cases = [
        ("postgresql", "$1"),
        ("redshift", "$1"),
        ("duckdb", "$1"),
        ("mysql", "?"),
        ("sqlite", "?"),
        ("mssql", "?"),
    ];
    for (engine, placeholder) in cases {
        let g = make_generator("users", vec!["id", "name"], vec!["id"], db(engine));
        let mut change = RowChange::new(RowID::Existing(0), ChangeType::Update);
        change.cell_changes.push(CellChange::new(1, "name", text("a"), text("b")));
        change.original_row = Some(vec![text("1"), text("a")]);
        let statements = run(&g, vec![change], vec![], vec![], vec![]);
        assert_eq!(statements.len(), 1, "{engine}");
        assert!(statements[0].0.contains(placeholder), "{engine}: {}", statements[0].0);
    }
}

#[test]
fn dollar_style_update_placeholder_numbering() {
    let g = make_generator("users", vec!["id", "name", "email"], vec!["id"], db("postgresql"));
    let mut change = RowChange::new(RowID::Existing(0), ChangeType::Update);
    change.cell_changes.push(CellChange::new(1, "name", text("a"), text("b")));
    change.cell_changes.push(CellChange::new(2, "email", text("c"), text("d")));
    change.original_row = Some(vec![text("1"), text("a"), text("c")]);
    let statements = run(&g, vec![change], vec![], vec![], vec![]);
    let (sql, _) = &statements[0];
    assert!(sql.contains("\"name\" = $1"));
    assert!(sql.contains("\"email\" = $2"));
    assert!(sql.contains("\"id\" = $3"));
}

// ---- MSSQL: bracket quoting ----

#[test]
fn insert_bracket_quoting() {
    let g = make_generator("users", vec!["id", "name"], vec!["id"], db("mssql"));
    let statements = run(&g, vec![insert_change(RowID::Existing(0))], vec![(0, vec![text("1"), text("a")])], vec![0], vec![]);
    assert!(statements[0].0.contains("[users]"));
    assert!(statements[0].0.contains("[id]"));
    assert!(statements[0].0.contains("[name]"));
}

#[test]
fn update_bracket_quoting() {
    let g = make_generator("users", vec!["id", "name"], vec!["id"], db("mssql"));
    let mut change = RowChange::new(RowID::Existing(0), ChangeType::Update);
    change.cell_changes.push(CellChange::new(1, "name", text("a"), text("b")));
    change.original_row = Some(vec![text("1"), text("a")]);
    let statements = run(&g, vec![change], vec![], vec![], vec![]);
    assert!(statements[0].0.contains("[name] = ?"));
    assert!(statements[0].0.contains("[id] = ?"));
}

#[test]
fn bracket_escaping_doubles_closing_bracket() {
    let quoted = quote_identifier_from_dialect(&SqlDialect::mssql(), "we]ird");
    assert_eq!(quoted, "[we]]ird]");
}

#[test]
fn ansi_quoting_doubles_embedded_quote() {
    let quoted = quote_identifier_from_dialect(&SqlDialect::ansi(), "we\"ird");
    assert_eq!(quoted, "\"we\"\"ird\"");
}

#[test]
fn backtick_quoting_doubles_embedded_backtick() {
    let quoted = quote_identifier_from_dialect(&SqlDialect::mysql(), "we`ird");
    assert_eq!(quoted, "`we``ird`");
}

// ---- String literal escaping (display/inlining paths) ----

#[test]
fn ansi_escaping_doubles_quotes_and_strips_null() {
    assert_eq!(
        sqlpro_core::sqlgen::escape_string_literal("O'Reilly\0"),
        "O''Reilly"
    );
}

#[test]
fn mysql_escaping_backslash_first() {
    assert_eq!(
        sqlpro_core::sqlgen::escape_string_literal_backslash("C:\\temp\nnext"),
        "C:\\\\temp\\nnext"
    );
    assert_eq!(sqlpro_core::sqlgen::escape_string_literal_backslash("a\u{1A}b"), "a\\Zb");
    assert_eq!(sqlpro_core::sqlgen::escape_string_literal_backslash("a\tb"), "a\\tb");
}

// ---- Schema qualification ----

#[test]
fn schema_qualifies_the_insert() {
    let mut g = make_generator("users", vec!["id"], vec!["id"], db("postgresql"));
    g.schema_name = Some("public".into());
    let statements = run(&g, vec![insert_change(RowID::Existing(0))], vec![(0, vec![text("1")])], vec![0], vec![]);
    assert!(statements[0].0.contains("INSERT INTO \"public\".\"users\""));
}

#[test]
fn no_schema_keeps_unqualified_name() {
    let g = make_generator("users", vec!["id"], vec!["id"], db("postgresql"));
    let statements = run(&g, vec![insert_change(RowID::Existing(0))], vec![(0, vec![text("1")])], vec![0], vec![]);
    assert!(statements[0].0.contains("INSERT INTO \"users\""));
}

// ---- Composite PK ----

#[test]
fn update_composite_pk_basic() {
    let g = make_generator("order_items", vec!["order_id", "line", "qty"], vec!["order_id", "line"], db("mysql"));
    let mut change = RowChange::new(RowID::Existing(0), ChangeType::Update);
    change.cell_changes.push(CellChange::new(2, "qty", text("1"), text("5")));
    change.original_row = Some(vec![text("10"), text("2"), text("1")]);
    let statements = run(&g, vec![change], vec![], vec![], vec![]);
    assert_eq!(statements.len(), 1);
    let (sql, params) = &statements[0];
    assert!(sql.contains("`order_id` = ?"));
    assert!(sql.contains("`line` = ?"));
    assert!(sql.contains(" AND "));
    assert_eq!(params.len(), 3);
    assert_eq!(param_text(params, 0), "5");
    assert_eq!(param_text(params, 1), "10");
    assert_eq!(param_text(params, 2), "2");
}

#[test]
fn delete_composite_pk_parenthesized_row_clause() {
    let g = make_generator("order_items", vec!["order_id", "line"], vec!["order_id", "line"], db("mysql"));
    let mut change = RowChange::new(RowID::Existing(0), ChangeType::Delete);
    change.original_row = Some(vec![text("10"), text("2")]);
    let statements = run(&g, vec![change], vec![], vec![], vec![0]);
    assert_eq!(statements.len(), 1);
    assert_eq!(
        statements[0].0,
        "DELETE FROM `order_items` WHERE (`order_id` = ? AND `line` = ?)"
    );
}

#[test]
fn composite_pk_null_value_skips_update() {
    let g = make_generator("order_items", vec!["order_id", "line", "qty"], vec!["order_id", "line"], db("mysql"));
    let mut change = RowChange::new(RowID::Existing(0), ChangeType::Update);
    change.cell_changes.push(CellChange::new(2, "qty", text("1"), text("5")));
    change.original_row = Some(vec![CellValue::Null, text("2"), text("1")]);
    let statements = run(&g, vec![change], vec![], vec![], vec![]);
    assert!(statements.is_empty());
}

#[test]
fn no_pk_update_fallback() {
    let g = make_generator("t", vec!["a", "b"], vec![], db("sqlite"));
    let mut change = RowChange::new(RowID::Existing(0), ChangeType::Update);
    change.cell_changes.push(CellChange::new(0, "a", text("1"), text("2")));
    change.original_row = Some(vec![text("1"), text("x")]);
    let statements = run(&g, vec![change], vec![], vec![], vec![]);
    assert_eq!(statements.len(), 1);
    assert!(statements[0].0.contains("UPDATE \"t\" SET \"a\" = ? WHERE \"a\" = ? AND \"b\" = ?"));
}

#[test]
fn delete_no_pk_multiple_rows_single_statement() {
    let g = make_generator("t", vec!["a"], vec![], db("mysql"));
    let mut c1 = RowChange::new(RowID::Existing(0), ChangeType::Delete);
    c1.original_row = Some(vec![text("1")]);
    let mut c2 = RowChange::new(RowID::Existing(1), ChangeType::Delete);
    c2.original_row = Some(vec![text("2")]);
    let statements = run(&g, vec![c1, c2], vec![], vec![], vec![0, 1]);
    assert_eq!(statements.len(), 1);
    assert_eq!(statements[0].0, "DELETE FROM `t` WHERE `a` = ? OR `a` = ?");
}

// ---- Row match policy (keyless text columns) ----

#[test]
fn keyless_update_wraps_lossy_columns() {
    let mut g = make_generator("t", vec!["a", "b"], vec![], db("mssql"));
    g.row_match_policy.text_columns = BTreeSet::from(["b".to_string()]);
    let mut change = RowChange::new(RowID::Existing(0), ChangeType::Update);
    change.cell_changes.push(CellChange::new(0, "a", text("1"), text("2")));
    change.original_row = Some(vec![text("1"), text("x")]);
    let statements = run(&g, vec![change], vec![], vec![], vec![]);
    assert_eq!(statements.len(), 1);
    assert!(statements[0].0.contains("[a] = ?"));
    assert!(statements[0].0.contains("CAST([b] AS NVARCHAR(MAX)) = ?"));
}

#[test]
fn keyed_update_is_unwrapped() {
    let mut g = make_generator("t", vec!["id", "b"], vec!["id"], db("mssql"));
    g.row_match_policy.text_columns = BTreeSet::from(["b".to_string()]);
    let mut change = RowChange::new(RowID::Existing(0), ChangeType::Update);
    change.cell_changes.push(CellChange::new(1, "b", text("x"), text("y")));
    change.original_row = Some(vec![text("1"), text("x")]);
    let statements = run(&g, vec![change], vec![], vec![], vec![]);
    assert_eq!(statements.len(), 1);
    assert!(statements[0].0.contains("[id] = ?"));
    assert!(!statements[0].0.contains("CAST"));
}

#[test]
fn keyless_delete_wraps_lossy_columns() {
    let mut g = make_generator("t", vec!["a", "b"], vec![], db("mysql"));
    g.row_match_policy.text_columns = BTreeSet::from(["b".to_string()]);
    let mut change = RowChange::new(RowID::Existing(0), ChangeType::Delete);
    change.original_row = Some(vec![text("1"), text("x")]);
    let statements = run(&g, vec![change], vec![], vec![], vec![0]);
    assert_eq!(statements.len(), 1);
    assert!(statements[0].0.contains("CONCAT(`b`) = ?"));
}

#[test]
fn keyless_delete_with_exclusions_is_not_batched() {
    let mut g = make_generator("t", vec!["a", "b"], vec![], db("mysql"));
    g.row_match_policy.excluded_columns = BTreeSet::from(["b".to_string()]);
    let mut c1 = RowChange::new(RowID::Existing(0), ChangeType::Delete);
    c1.original_row = Some(vec![text("1"), text("x")]);
    let mut c2 = RowChange::new(RowID::Existing(1), ChangeType::Delete);
    c2.original_row = Some(vec![text("2"), text("y")]);
    let statements = run(&g, vec![c1, c2], vec![], vec![], vec![0, 1]);
    // One row per statement, never OR-batched.
    assert_eq!(statements.len(), 2);
    assert!(statements[0].0.contains("WHERE `a` = ?"));
}

#[test]
fn batch_delete_chunks_at_bind_parameter_cap() {
    // sqlite caps at 32766; use a 2-param composite PK and force chunking by
    // shrinking the cap through mssql (2100). 2000 rows × 1 param each
    // chunk once.
    let g = make_generator("t", vec!["id"], vec!["id"], db("mssql"));
    let mut changes = Vec::new();
    for i in 0..2500 {
        let mut c = RowChange::new(RowID::Existing(i), ChangeType::Delete);
        c.original_row = Some(vec![text(&i.to_string())]);
        changes.push(c);
    }
    let mut deleted = std::collections::BTreeSet::new();
    for i in 0..2500 {
        deleted.insert(RowID::Existing(i));
    }
    let statements = g.generate_statements(&changes, &BTreeMap::new(), &deleted, &BTreeSet::new());
    assert_eq!(statements.len(), 2); // 2100 cap → 2100 + 400
    assert_eq!(statements[0].parameters.len(), 2100);
    assert_eq!(statements[1].parameters.len(), 400);
}

// ---- Redshift (dollar style + ansi quotes, like its PG parentage) ----

#[test]
fn redshift_insert_placeholders_and_quoting() {
    let g = make_generator("users", vec!["id", "name"], vec!["id"], db("redshift"));
    let statements = run(&g, vec![insert_change(RowID::Existing(0))], vec![(0, vec![text("1"), text("a")])], vec![0], vec![]);
    assert!(statements[0].0.contains("\"users\""));
    assert!(statements[0].0.contains("$1"));
}

// ---- Empty changes ----

#[test]
fn empty_changes_produce_nothing() {
    let g = make_generator("users", vec!["id"], vec!["id"], db("mysql"));
    let statements = run(&g, vec![], vec![], vec![], vec![]);
    assert!(statements.is_empty());
}

#[test]
fn full_workflow_integration() {
    // Insert, edit, delete in one save.
    let g = make_generator("users", vec!["id", "name"], vec!["id"], db("mysql"));

    let mut update = RowChange::new(RowID::Existing(0), ChangeType::Update);
    update.cell_changes.push(CellChange::new(1, "name", text("alice"), text("alice2")));
    update.original_row = Some(vec![text("1"), text("alice")]);
    update.sequence = 0;

    let mut delete = RowChange::new(RowID::Existing(1), ChangeType::Delete);
    delete.original_row = Some(vec![text("2"), text("bob")]);
    delete.sequence = 1;

    let mut insert = RowChange::new(RowID::Existing(2), ChangeType::Insert);
    insert.sequence = 2;

    let inserted: BTreeMap<RowID, Vec<CellValue>> =
        BTreeMap::from([(RowID::Existing(2), vec![text("3"), text("carol")])]);
    let mut deleted = std::collections::BTreeSet::new();
    deleted.insert(RowID::Existing(1));
    let mut inserted_ids = std::collections::BTreeSet::new();
    inserted_ids.insert(RowID::Existing(2));

    let statements = g.generate_statements(
        &[update, delete, insert],
        &inserted,
        &deleted,
        &inserted_ids,
    );
    assert_eq!(statements.len(), 3);
    assert!(statements[0].sql.starts_with("UPDATE"));
    assert!(statements[1].sql.starts_with("DELETE FROM"));
    assert!(statements[2].sql.starts_with("INSERT INTO"));
}
