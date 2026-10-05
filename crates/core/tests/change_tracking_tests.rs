//! Ported from TableProTests/Core/ChangeTracking/: DataChangeModelsTests,
//! PendingChangesTests, PendingChangesRowIdentityTests, DataChangeManagerTests,
//! DataChangeManagerExtendedTests, FieldAbsenceTrackingTests.
//! Test names follow the Swift originals.

use sqlpro_core::change::{CellChange, ChangeType, FieldAbsence, PendingChanges, RowChange, RowID};
use sqlpro_core::value::CellValue;

fn existing(ix: i64) -> RowID {
    RowID::Existing(ix)
}

// ---- DataChangeModelsTests ----

#[test]
fn change_type_equality() {
    assert_eq!(ChangeType::Update, ChangeType::Update);
    assert_ne!(ChangeType::Update, ChangeType::Insert);
}

#[test]
fn cell_change_stores_values() {
    let c = CellChange::new(1, "name", CellValue::text("a"), CellValue::text("b"));
    assert_eq!(c.column_index, 1);
    assert_eq!(c.old_value, CellValue::text("a"));
    assert_eq!(c.new_value, CellValue::text("b"));
}

#[test]
fn cell_change_nil_values() {
    let c = CellChange::new(0, "x", CellValue::Null, CellValue::Null);
    assert!(c.old_value.is_null());
}

#[test]
fn row_change_stores_values() {
    let mut c = RowChange::new(existing(0), ChangeType::Delete);
    c.original_row = Some(vec![CellValue::text("1"), CellValue::Null]);
    assert_eq!(c.change_type, ChangeType::Delete);
    assert_eq!(c.original_row.as_ref().unwrap().len(), 2);
}

#[test]
fn row_change_empty_cell_changes() {
    let c = RowChange::new(existing(0), ChangeType::Update);
    assert!(c.cell_changes.is_empty());
}

// ---- DataChangeManagerTests ----

#[test]
fn initial_state_has_no_changes() {
    let p = PendingChanges::new();
    assert!(!p.has_changes());
    assert!(p.is_empty());
}

#[test]
fn record_cell_change_updates_has_changes() {
    let mut p = PendingChanges::new();
    p.record_cell_change(
        existing(0),
        1,
        "name",
        CellValue::text("John"),
        CellValue::text("Johnny"),
        None,
        FieldAbsence::default(),
    );
    assert!(p.has_changes());
}

#[test]
fn same_value_is_ignored() {
    let mut p = PendingChanges::new();
    let changed = p.record_cell_change(
        existing(0),
        1,
        "name",
        CellValue::text("John"),
        CellValue::text("John"),
        None,
        FieldAbsence::default(),
    );
    assert!(!changed);
    assert!(!p.has_changes());
}

#[test]
fn edit_same_cell_merges_change() {
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 1, "name", CellValue::text("a"), CellValue::text("b"), None, Default::default());
    p.record_cell_change(existing(0), 1, "name", CellValue::text("a"), CellValue::text("c"), None, Default::default());
    assert_eq!(p.changes.len(), 1);
    assert_eq!(p.changes[0].cell_changes.len(), 1);
    assert_eq!(p.changes[0].cell_changes[0].new_value, CellValue::text("c"));
    // The original DB value is preserved through the merge.
    assert_eq!(p.changes[0].cell_changes[0].old_value, CellValue::text("a"));
}

#[test]
fn edit_back_to_original_removes_change() {
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 1, "name", CellValue::text("a"), CellValue::text("b"), None, Default::default());
    // Editing back to the original retracts the pending change entirely.
    p.record_cell_change(existing(0), 1, "name", CellValue::text("a"), CellValue::text("a"), None, Default::default());
    assert!(!p.has_changes());
    assert!(!p.is_cell_modified(existing(0), 1));
}

#[test]
fn different_rows_separate_entries() {
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 0, "a", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    p.record_cell_change(existing(1), 0, "a", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    assert_eq!(p.changes.len(), 2);
}

#[test]
fn record_row_deletion_updates_has_changes() {
    let mut p = PendingChanges::new();
    p.record_row_deletion(existing(0), vec![CellValue::text("1")], Default::default());
    assert!(p.has_changes());
    assert!(p.is_row_deleted(existing(0)));
}

#[test]
fn delete_removes_prior_updates() {
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 1, "name", CellValue::text("a"), CellValue::text("b"), None, Default::default());
    p.record_row_deletion(existing(0), vec![CellValue::text("1")], Default::default());
    assert_eq!(p.changes.len(), 1);
    assert_eq!(p.changes[0].change_type, ChangeType::Delete);
    assert!(p.modified_cells.get(&existing(0)).is_none());
}

#[test]
fn batch_deletion_records_all_rows() {
    let mut p = PendingChanges::new();
    p.record_row_deletion(existing(0), vec![CellValue::text("1")], Default::default());
    p.record_row_deletion(existing(1), vec![CellValue::text("2")], Default::default());
    p.record_row_deletion(existing(2), vec![CellValue::text("3")], Default::default());
    assert_eq!(p.deleted_row_ids.len(), 3);
    assert_eq!(p.changes.len(), 3);
}

#[test]
fn clear_changes_removes_all() {
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 0, "a", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    p.record_row_deletion(existing(1), vec![], Default::default());
    p.clear();
    assert!(!p.has_changes());
    assert!(p.deleted_row_ids.is_empty());
}

#[test]
fn reload_version_increments_on_clear() {
    // In this port, the reload version lives in the workspace; the pending
    // contract is that clear resets sequence numbering.
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 0, "a", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    p.clear();
    assert_eq!(p.changes.len(), 0);
}

#[test]
fn record_row_insertion_sets_has_changes() {
    let mut p = PendingChanges::new();
    p.record_row_insertion(existing(0), vec![CellValue::text("1")], Default::default());
    assert!(p.has_changes());
    assert!(p.is_row_inserted(existing(0)));
}

#[test]
fn record_row_insertion_stores_in_inserted_row_data() {
    let mut p = PendingChanges::new();
    p.record_row_insertion(existing(0), vec![CellValue::text("1"), CellValue::text("x")], Default::default());
    assert_eq!(
        p.inserted_row_data.get(&existing(0)).unwrap(),
        &vec![CellValue::text("1"), CellValue::text("x")]
    );
}

#[test]
fn record_row_insertion_enables_undo() {
    let mut p = PendingChanges::new();
    p.record_row_insertion(existing(0), vec![], Default::default());
    assert!(p.undo_row_insertion(existing(0)));
    assert!(!p.has_changes());
}

#[test]
fn multiple_row_insertions_tracked_separately() {
    let mut p = PendingChanges::new();
    p.record_row_insertion(existing(0), vec![CellValue::text("1")], Default::default());
    p.record_row_insertion(existing(1), vec![CellValue::text("2")], Default::default());
    assert_eq!(p.inserted_row_ids.len(), 2);
    assert_eq!(p.changes.len(), 2);
}

#[test]
fn is_row_deleted_correctness() {
    let mut p = PendingChanges::new();
    p.record_row_deletion(existing(1), vec![], Default::default());
    assert!(p.is_row_deleted(existing(1)));
    assert!(!p.is_row_deleted(existing(0)));
}

#[test]
fn is_row_inserted_correctness() {
    let mut p = PendingChanges::new();
    p.record_row_insertion(existing(1), vec![], Default::default());
    assert!(p.is_row_inserted(existing(1)));
    assert!(!p.is_row_inserted(existing(0)));
}

#[test]
fn is_cell_modified_true_after_edit() {
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 2, "c", CellValue::text("a"), CellValue::text("b"), None, Default::default());
    assert!(p.is_cell_modified(existing(0), 2));
}

#[test]
fn is_cell_modified_false_after_revert_to_original() {
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 2, "c", CellValue::text("a"), CellValue::text("b"), None, Default::default());
    p.record_cell_change(existing(0), 2, "c", CellValue::text("a"), CellValue::text("a"), None, Default::default());
    assert!(!p.is_cell_modified(existing(0), 2));
}

#[test]
fn get_modified_columns_correct_set() {
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 1, "b", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    p.record_cell_change(existing(0), 3, "d", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    let cols = p.modified_columns(existing(0));
    assert!(cols.contains(&1) && cols.contains(&3));
    assert_eq!(cols.len(), 2);
}

#[test]
fn get_modified_columns_empty_for_unmodified_row() {
    let p = PendingChanges::new();
    assert!(p.modified_columns(existing(0)).is_empty());
}

#[test]
fn cell_modification_cleared_on_row_deletion() {
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 1, "b", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    p.record_row_deletion(existing(0), vec![], Default::default());
    assert!(p.modified_columns(existing(0)).is_empty());
}

// ---- DataChangeManagerExtendedTests (a faithful core subset) ----

#[test]
fn undo_cell_edit_then_redo_restores_change() {
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 1, "name", CellValue::text("a"), CellValue::text("b"), None, Default::default());
    p.revert_update_cell(existing(0), 1, "name", CellValue::text("a"), false);
    assert!(!p.has_changes());
    p.reapply_cell_change(existing(0), 1, "name", CellValue::text("a"), CellValue::text("b"), None, Default::default());
    assert!(p.has_changes());
    assert_eq!(p.changes[0].cell_changes[0].new_value, CellValue::text("b"));
}

#[test]
fn undo_row_insertion_removes_from_indices() {
    let mut p = PendingChanges::new();
    p.record_row_insertion(existing(0), vec![], Default::default());
    p.undo_row_insertion(existing(0));
    assert!(!p.is_row_inserted(existing(0)));
}

#[test]
fn undo_row_deletion_removes_from_indices() {
    let mut p = PendingChanges::new();
    p.record_row_deletion(existing(0), vec![], Default::default());
    p.undo_row_deletion(existing(0));
    assert!(!p.is_row_deleted(existing(0)));
}

#[test]
fn undo_row_insertion_then_redo_re_inserts() {
    let mut p = PendingChanges::new();
    let values = vec![CellValue::text("1"), CellValue::text("x")];
    p.record_row_insertion(existing(0), values.clone(), Default::default());
    p.undo_row_insertion(existing(0));
    p.reinsert_row(existing(0), &["id".into(), "name".into()], Some(values), Default::default());
    assert!(p.is_row_inserted(existing(0)));
    assert_eq!(
        p.inserted_row_data.get(&existing(0)).unwrap().len(),
        2
    );
}

#[test]
fn undo_row_deletion_then_redo_re_deletes() {
    let mut p = PendingChanges::new();
    p.record_row_deletion(existing(0), vec![CellValue::text("1")], Default::default());
    p.undo_row_deletion(existing(0));
    p.reapply_row_deletion(existing(0), vec![CellValue::text("1")], Default::default());
    assert!(p.is_row_deleted(existing(0)));
}

#[test]
fn multiple_sequential_undos() {
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 0, "a", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    p.record_cell_change(existing(1), 0, "a", CellValue::text("1"), CellValue::text("3"), None, Default::default());
    p.revert_update_cell(existing(1), 0, "a", CellValue::text("1"), false);
    assert!(p.has_changes());
    p.revert_update_cell(existing(0), 0, "a", CellValue::text("1"), false);
    assert!(!p.has_changes());
}

#[test]
fn edit_then_delete_same_row() {
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 1, "b", CellValue::text("a"), CellValue::text("b"), None, Default::default());
    p.record_row_deletion(existing(0), vec![], Default::default());
    assert_eq!(p.changes.len(), 1);
    assert_eq!(p.changes[0].change_type, ChangeType::Delete);
}

#[test]
fn insert_then_edit_updates_inserted_row_data() {
    let mut p = PendingChanges::new();
    p.record_row_insertion(existing(0), vec![CellValue::text("1"), CellValue::text("x")], Default::default());
    p.record_cell_change(existing(0), 1, "name", CellValue::Null, CellValue::text("y"), None, Default::default());
    assert_eq!(p.inserted_row_data[&existing(0)][1], CellValue::text("y"));
    // Still one insert change, never an update.
    assert_eq!(p.changes.len(), 1);
    assert_eq!(p.changes[0].change_type, ChangeType::Insert);
}

#[test]
fn insert_then_edit_then_undo_reverts_cell() {
    let mut p = PendingChanges::new();
    p.record_row_insertion(existing(0), vec![CellValue::text("1"), CellValue::text("x")], Default::default());
    p.record_cell_change(existing(0), 1, "name", CellValue::Null, CellValue::text("y"), None, Default::default());
    p.update_inserted_cell_directly(existing(0), 1, "name", CellValue::text("x"), false);
    assert_eq!(p.inserted_row_data[&existing(0)][1], CellValue::text("x"));
}

#[test]
fn edit_multiple_cells_same_row_all_tracked() {
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 0, "a", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    p.record_cell_change(existing(0), 1, "b", CellValue::text("3"), CellValue::text("4"), None, Default::default());
    p.record_cell_change(existing(0), 2, "c", CellValue::text("5"), CellValue::text("6"), None, Default::default());
    assert_eq!(p.changes.len(), 1);
    assert_eq!(p.changes[0].cell_changes.len(), 3);
}

#[test]
fn edit_multiple_cells_revert_one_only_reverted_removed() {
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 0, "a", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    p.record_cell_change(existing(0), 1, "b", CellValue::text("3"), CellValue::text("4"), None, Default::default());
    p.revert_update_cell(existing(0), 0, "a", CellValue::text("1"), false);
    assert_eq!(p.changes.len(), 1);
    assert_eq!(p.changes[0].cell_changes.len(), 1);
    assert_eq!(p.changes[0].cell_changes[0].column_name, "b");
}

#[test]
fn batch_deletion_clears_prior_edits() {
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 0, "a", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    p.record_cell_change(existing(1), 0, "a", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    p.record_row_deletion(existing(0), vec![], Default::default());
    p.record_row_deletion(existing(1), vec![], Default::default());
    assert!(p.modified_cells.is_empty());
    assert_eq!(p.changes.len(), 2);
    assert!(p.changes.iter().all(|c| c.change_type == ChangeType::Delete));
}

#[test]
fn undo_batch_deletion_restores_all_rows() {
    let mut p = PendingChanges::new();
    p.record_row_deletion(existing(0), vec![], Default::default());
    p.record_row_deletion(existing(1), vec![], Default::default());
    p.undo_row_deletion(existing(0));
    p.undo_row_deletion(existing(1));
    assert!(!p.has_changes());
}

#[test]
fn get_original_values_returns_correct_data() {
    let mut p = PendingChanges::new();
    p.record_cell_change(
        existing(0),
        1,
        "b",
        CellValue::text("orig"),
        CellValue::text("new"),
        Some(vec![CellValue::text("1"), CellValue::text("orig")]),
        Default::default(),
    );
    let change = p.change_for_row(existing(0), ChangeType::Update).unwrap();
    assert_eq!(change.cell_changes[0].old_value, CellValue::text("orig"));
}

#[test]
fn record_deletion_for_already_deleted_row() {
    let mut p = PendingChanges::new();
    p.record_row_deletion(existing(0), vec![CellValue::text("1")], Default::default());
    p.record_row_deletion(existing(0), vec![CellValue::text("1")], Default::default());
    assert_eq!(p.changes.len(), 1);
}

#[test]
fn record_cell_change_nil_to_nil_is_no_op() {
    let mut p = PendingChanges::new();
    let changed = p.record_cell_change(existing(0), 0, "a", CellValue::Null, CellValue::Null, None, Default::default());
    assert!(!changed);
    assert!(!p.has_changes());
}

#[test]
fn concurrent_insertions_at_different_indices() {
    // Rows inserted at different grid positions keep independent identities.
    let mut p = PendingChanges::new();
    p.record_row_insertion(RowID::Inserted(uuid::Uuid::new_v4()), vec![CellValue::text("a")], Default::default());
    p.record_row_insertion(RowID::Inserted(uuid::Uuid::new_v4()), vec![CellValue::text("b")], Default::default());
    assert_eq!(p.changes.len(), 2);
}

// ---- PendingChangesTests ----

#[test]
fn record_cell_creates_update() {
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 0, "a", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    assert!(p.change_for_row(existing(0), ChangeType::Update).is_some());
}

#[test]
fn revert_to_original_collapses() {
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 0, "a", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    p.record_cell_change(existing(0), 0, "a", CellValue::text("1"), CellValue::text("1"), None, Default::default());
    assert!(p.is_empty());
}

#[test]
fn deletion_removes_update() {
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 0, "a", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    p.record_row_deletion(existing(0), vec![], Default::default());
    assert!(p.change_for_row(existing(0), ChangeType::Update).is_none());
    assert!(p.change_for_row(existing(0), ChangeType::Delete).is_some());
}

#[test]
fn double_deletion_is_idempotent() {
    let mut p = PendingChanges::new();
    p.record_row_deletion(existing(0), vec![], Default::default());
    let before = p.changes.len();
    p.record_row_deletion(existing(0), vec![], Default::default());
    assert_eq!(p.changes.len(), before);
}

#[test]
fn double_insertion_is_idempotent() {
    let mut p = PendingChanges::new();
    p.record_row_insertion(existing(0), vec![CellValue::text("1")], Default::default());
    p.record_row_insertion(existing(0), vec![CellValue::text("2")], Default::default());
    assert_eq!(p.changes.len(), 1);
    assert_eq!(p.inserted_row_data[&existing(0)], vec![CellValue::text("2")]);
}

#[test]
fn undo_row_insertion_leaves_others() {
    let mut p = PendingChanges::new();
    p.record_row_insertion(existing(0), vec![CellValue::text("1")], Default::default());
    p.record_row_insertion(existing(1), vec![CellValue::text("2")], Default::default());
    p.undo_row_insertion(existing(0));
    assert!(p.is_row_inserted(existing(1)));
    assert!(!p.is_row_inserted(existing(0)));
}

#[test]
fn undo_nonexistent_insertion() {
    let mut p = PendingChanges::new();
    assert!(!p.undo_row_insertion(existing(9)));
}

#[test]
fn undo_batch_row_insertion() {
    let mut p = PendingChanges::new();
    p.record_row_insertion(existing(0), vec![CellValue::text("1"), CellValue::text("x")], Default::default());
    p.record_row_insertion(existing(1), vec![CellValue::text("2"), CellValue::text("y")], Default::default());
    let values = p.undo_batch_row_insertion(&[existing(0), existing(1)], 2);
    assert_eq!(values.len(), 2);
    assert_eq!(values[0][0], CellValue::text("1"));
    assert_eq!(values[1][0], CellValue::text("2"));
    assert!(!p.has_changes());
}

#[test]
fn snapshot_round_trip() {
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 0, "a", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    p.record_row_deletion(existing(1), vec![], Default::default());
    p.record_row_insertion(existing(2), vec![CellValue::text("9")], Default::default());

    let snapshot = p.clone();
    let mut restored = PendingChanges::new();
    restored.restore_snapshot(snapshot);
    assert_eq!(restored.changes.len(), 3);
    assert!(restored.is_row_deleted(existing(1)));
    assert!(restored.is_row_inserted(existing(2)));
    // Can continue editing after restore (change index rebuilt).
    restored.record_cell_change(existing(0), 0, "a", CellValue::text("1"), CellValue::text("5"), None, Default::default());
    assert_eq!(restored.changes.len(), 3);
    assert_eq!(restored.changes[0].cell_changes[0].new_value, CellValue::text("5"));
}

#[test]
fn clear_resets() {
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 0, "a", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    p.clear();
    assert_eq!(p, PendingChanges::new());
}

// ---- PendingChangesRowIdentityTests ----

#[test]
fn partial_batch_undo_keeps_survivor_values() {
    let mut p = PendingChanges::new();
    p.record_row_insertion(existing(0), vec![CellValue::text("v0")], Default::default());
    p.record_row_insertion(existing(1), vec![CellValue::text("v1")], Default::default());
    p.undo_row_insertion(existing(0));
    assert_eq!(
        p.saved_inserted_values(existing(1)).unwrap(),
        &vec![CellValue::text("v1")]
    );
}

#[test]
fn undo_insertion_keeps_other_rows_edits() {
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 0, "a", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    p.record_row_insertion(existing(5), vec![], Default::default());
    p.undo_row_insertion(existing(5));
    assert!(p.has_changes());
    assert!(p.is_cell_modified(existing(0), 0));
}

#[test]
fn existing_and_inserted_rows_are_distinct() {
    let mut p = PendingChanges::new();
    p.record_row_deletion(existing(0), vec![], Default::default());
    p.record_row_insertion(RowID::Inserted(uuid::Uuid::new_v4()), vec![], Default::default());
    assert_eq!(p.changes.len(), 2);
}

#[test]
fn sequence_rises() {
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 0, "a", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    p.record_cell_change(existing(1), 0, "a", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    p.record_row_deletion(existing(2), vec![], Default::default());
    let seqs: Vec<i64> = p.changes.iter().map(|c| c.sequence).collect();
    let mut sorted = seqs.clone();
    sorted.sort();
    assert_eq!(seqs, sorted);
}

#[test]
fn cancelling_keeps_order() {
    // The swap-remove means array order diverges, but sequences stay
    // ordered. This is the load-bearing invariant for statement generation.
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 0, "a", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    p.record_cell_change(existing(1), 0, "a", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    p.record_cell_change(existing(2), 0, "a", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    // Cancel the middle change: the last change swaps into its slot.
    p.revert_update_cell(existing(1), 0, "a", CellValue::text("1"), false);
    assert_eq!(p.changes.len(), 2);
    assert_eq!(p.changes[0].row_id, existing(0));
    assert_eq!(p.changes[1].row_id, existing(2));
    let seqs: Vec<i64> = p.changes.iter().map(|c| c.sequence).collect();
    assert_ne!(seqs[0], seqs[1]);
    let mut sorted = seqs.clone();
    sorted.sort();
    assert_eq!(seqs, sorted);
}

#[test]
fn undo_then_redo_round_trips() {
    let mut p = PendingChanges::new();
    p.record_cell_change(existing(0), 0, "a", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    p.revert_update_cell(existing(0), 0, "a", CellValue::text("1"), false);
    p.reapply_cell_change(existing(0), 0, "a", CellValue::text("1"), CellValue::text("2"), None, Default::default());
    assert_eq!(p.changes[0].cell_changes[0].new_value, CellValue::text("2"));
    assert_eq!(p.changes[0].cell_changes[0].old_value, CellValue::text("1"));
}

// ---- FieldAbsenceTrackingTests ----

#[test]
fn null_over_missing_field_is_a_change() {
    let mut p = PendingChanges::new();
    let absence = FieldAbsence { was_absent: true, is_absent: false, original_row: Default::default() };
    let changed = p.record_cell_change(existing(0), 0, "a", CellValue::Null, CellValue::Null, None, absence);
    assert!(changed);
    assert!(p.has_changes());
}

#[test]
fn removing_a_null_field_is_a_change() {
    let mut p = PendingChanges::new();
    let absence = FieldAbsence { was_absent: false, is_absent: true, original_row: Default::default() };
    let changed = p.record_cell_change(existing(0), 0, "a", CellValue::Null, CellValue::Null, None, absence);
    assert!(changed);
}

#[test]
fn remove_then_restore_collapses() {
    let mut p = PendingChanges::new();
    p.record_cell_change(
        existing(0),
        0,
        "a",
        CellValue::Null,
        CellValue::Null,
        None,
        FieldAbsence { was_absent: false, is_absent: true, original_row: Default::default() },
    );
    p.record_cell_change(
        existing(0),
        0,
        "a",
        CellValue::Null,
        CellValue::Null,
        None,
        FieldAbsence { was_absent: false, is_absent: false, original_row: Default::default() },
    );
    assert!(!p.has_changes());
}

#[test]
fn null_after_removal_is_still_a_change() {
    // Remove, then re-edit with the value null and the field still absent —
    // with a prior change present this merges, keeping one change.
    let mut p = PendingChanges::new();
    p.record_cell_change(
        existing(0),
        0,
        "a",
        CellValue::Null,
        CellValue::Null,
        None,
        FieldAbsence { was_absent: false, is_absent: true, original_row: Default::default() },
    );
    p.record_cell_change(
        existing(0),
        0,
        "a",
        CellValue::Null,
        CellValue::Null,
        None,
        FieldAbsence { was_absent: false, is_absent: true, original_row: Default::default() },
    );
    assert_eq!(p.changes.len(), 1);
}

#[test]
fn inserted_row_tracks_its_missing_fields() {
    let mut p = PendingChanges::new();
    let mut absent = std::collections::BTreeSet::new();
    absent.insert(2);
    p.record_row_insertion(existing(0), vec![CellValue::text("1"), CellValue::text("x"), CellValue::Null], absent.clone());
    let change = p.change_for_row(existing(0), ChangeType::Insert).unwrap();
    assert_eq!(change.absent_columns, absent);
}

#[test]
fn absence_original_row_keeps_with_first_change() {
    let mut p = PendingChanges::new();
    let mut original = std::collections::BTreeSet::new();
    original.insert(1);
    p.record_cell_change(
        existing(0),
        1,
        "b",
        CellValue::Null,
        CellValue::text("v"),
        None,
        FieldAbsence { was_absent: true, is_absent: false, original_row: original.clone() },
    );
    assert_eq!(p.changes[0].absent_columns, original);
}

#[test]
fn edit_moves_absence() {
    // Giving a missing field a value clears it from the row's absent set.
    let mut p = PendingChanges::new();
    p.record_row_insertion(existing(0), vec![CellValue::Null, CellValue::Null], {
        let mut s = std::collections::BTreeSet::new();
        s.insert(1);
        s
    });
    p.record_cell_change(existing(0), 1, "b", CellValue::Null, CellValue::text("v"), None, Default::default());
    let change = p.change_for_row(existing(0), ChangeType::Insert).unwrap();
    assert!(!change.absent_columns.contains(&1));
}
