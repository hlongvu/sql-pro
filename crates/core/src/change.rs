//! Change tracking: pending, uncommitted edits to a result set.
//!
//! Faithful port of TablePro's `DataChangeModels.swift` + `PendingChanges.swift`.
//! The load-bearing subtleties are preserved exactly:
//!
//! - `sequence` stamps the order the user worked in, because a cancelled change
//!   is removed by *swapping the last element into its slot* (O(1)), so array
//!   order diverges from edit order. Statement generation sorts by sequence —
//!   a row deleted to free a unique value, and a new row taking that value,
//!   only works if the DELETE runs first.
//! - Editing a value back to its original *retracts* the pending change.
//! - DELETE supersedes UPDATE; edits to an inserted row never become UPDATEs.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::value::CellValue;

/// Row identity in the grid, NOT a database key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RowID {
    Existing(i64),
    Inserted(Uuid),
}

impl RowID {
    pub fn is_inserted(&self) -> bool {
        matches!(self, RowID::Inserted(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeType {
    Update,
    Insert,
    Delete,
}

/// Which side of a cell edit has no field at all, for an engine that tells a
/// missing field from NULL. A missing field reads `Null`, so the value alone
/// cannot say. Every other engine keeps the default, where the field is there
/// on both sides.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldAbsence {
    /// The cell had no field before the edit.
    pub was_absent: bool,
    /// The edit takes the field out of the row.
    pub is_absent: bool,
    /// The columns the row had no field for before any of its edits, kept
    /// with its first change so a rewind can leave them missing.
    pub original_row: std::collections::BTreeSet<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellChange {
    #[serde(default = "Uuid::new_v4")]
    pub id: Uuid,
    pub column_index: usize,
    pub column_name: String,
    pub old_value: CellValue,
    pub new_value: CellValue,
    pub old_is_absent: bool,
    pub new_is_absent: bool,
}

impl CellChange {
    pub fn new(
        column_index: usize,
        column_name: impl Into<String>,
        old_value: CellValue,
        new_value: CellValue,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            column_index,
            column_name: column_name.into(),
            old_value,
            new_value,
            old_is_absent: false,
            new_is_absent: false,
        }
    }

    /// Whether the edit leaves the cell as it was read: the same value, and
    /// the field there or missing on both sides.
    pub fn restores_original(&self) -> bool {
        self.old_value == self.new_value && self.old_is_absent == self.new_is_absent
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowChange {
    #[serde(default = "Uuid::new_v4")]
    pub id: Uuid,
    pub row_id: RowID,
    pub change_type: ChangeType,
    pub cell_changes: Vec<CellChange>,
    /// The row as read from the server.
    pub original_row: Option<Vec<CellValue>>,
    /// The columns the row has no field for: as it was read for an update or
    /// a delete, and as it stands for an insert. Empty on every engine that
    /// cannot tell a missing field from NULL.
    pub absent_columns: std::collections::BTreeSet<usize>,
    /// The order the user made this change in.
    pub sequence: i64,
}

impl RowChange {
    pub fn new(row_id: RowID, change_type: ChangeType) -> Self {
        Self {
            id: Uuid::new_v4(),
            row_id,
            change_type,
            cell_changes: Vec::new(),
            original_row: None,
            absent_columns: Default::default(),
            sequence: 0,
        }
    }

    /// The columns the row has no field for once this update is written:
    /// those it lacked, less the ones an edit gave a value, plus the ones an
    /// edit removed.
    pub fn absent_columns_after_update(&self) -> std::collections::BTreeSet<usize> {
        let mut absent = self.absent_columns.clone();
        for cell_change in &self.cell_changes {
            if cell_change.new_is_absent {
                absent.insert(cell_change.column_index);
            } else {
                absent.remove(&cell_change.column_index);
            }
        }
        absent
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct RowChangeKey {
    row_id: RowID,
    change_type: ChangeType,
}

/// Value type holding all uncommitted edits to a result set.
///
/// Owns the consistency invariants between `changes`, `change_index`,
/// `deleted_row_ids`, `inserted_row_ids`, `modified_cells`, and
/// `inserted_row_data`. Callers mutate through methods that maintain the
/// cross-collection state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PendingChanges {
    pub changes: Vec<RowChange>,
    pub deleted_row_ids: std::collections::BTreeSet<RowID>,
    /// Stamped onto every change so statement generation can recover the
    /// order the user worked in.
    next_sequence: i64,
    pub inserted_row_ids: std::collections::BTreeSet<RowID>,
    pub modified_cells: std::collections::BTreeMap<RowID, std::collections::BTreeSet<usize>>,
    pub inserted_row_data: std::collections::BTreeMap<RowID, Vec<CellValue>>,
    change_index: std::collections::BTreeMap<RowChangeKey, usize>,
}

impl PendingChanges {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn has_changes(&self) -> bool {
        !self.is_empty()
    }

    // ---- Read ----------------------------------------------------------

    pub fn is_row_deleted(&self, row_id: RowID) -> bool {
        self.deleted_row_ids.contains(&row_id)
    }

    pub fn is_row_inserted(&self, row_id: RowID) -> bool {
        self.inserted_row_ids.contains(&row_id)
    }

    pub fn is_cell_modified(&self, row_id: RowID, column_index: usize) -> bool {
        self.modified_cells
            .get(&row_id)
            .map(|cols| cols.contains(&column_index))
            .unwrap_or(false)
    }

    pub fn modified_columns(&self, row_id: RowID) -> std::collections::BTreeSet<usize> {
        self.modified_cells.get(&row_id).cloned().unwrap_or_default()
    }

    pub fn change_for_row(&self, row_id: RowID, change_type: ChangeType) -> Option<&RowChange> {
        let idx = *self
            .change_index
            .get(&RowChangeKey { row_id, change_type })?;
        self.changes.get(idx)
    }

    // ---- Mutate (recording user edits) ---------------------------------

    /// Record a cell edit. Returns whether the edit changed anything (a
    /// no-op returns false so the caller can skip undo registration).
    ///
    /// A field going missing, or coming back, is a change even when the
    /// value reads `Null` on both sides.
    pub fn record_cell_change(
        &mut self,
        row_id: RowID,
        column_index: usize,
        column_name: &str,
        old_value: CellValue,
        new_value: CellValue,
        original_row: Option<Vec<CellValue>>,
        absence: FieldAbsence,
    ) -> bool {
        if old_value == new_value && absence.was_absent == absence.is_absent {
            return self.rollback_cell_if_matches_original(
                row_id,
                column_index,
                &new_value,
                absence.is_absent,
            );
        }

        let cell_change = CellChange {
            id: Uuid::new_v4(),
            column_index,
            column_name: column_name.to_string(),
            old_value,
            new_value: new_value.clone(),
            old_is_absent: absence.was_absent,
            new_is_absent: absence.is_absent,
        };

        if let Some(&insert_idx) = self
            .change_index
            .get(&RowChangeKey { row_id, change_type: ChangeType::Insert })
        {
            self.update_inserted_cell(
                insert_idx,
                column_index,
                column_name,
                new_value.clone(),
                absence.is_absent,
            );
            return true;
        }

        let update_key = RowChangeKey { row_id, change_type: ChangeType::Update };
        if let Some(&update_idx) = self.change_index.get(&update_key) {
            self.merge_update_cell(update_idx, cell_change);
        } else {
            let row = RowChange {
                id: Uuid::new_v4(),
                row_id,
                change_type: ChangeType::Update,
                cell_changes: vec![cell_change],
                original_row,
                absent_columns: absence.original_row,
                sequence: 0,
            };
            self.append_change(row);
            self.modified_cells.entry(row_id).or_default().insert(column_index);
        }
        true
    }

    pub fn record_row_deletion(
        &mut self,
        row_id: RowID,
        original_row: Vec<CellValue>,
        absent_columns: std::collections::BTreeSet<usize>,
    ) {
        if self.deleted_row_ids.contains(&row_id) {
            return;
        }
        self.remove_change(row_id, ChangeType::Update);
        self.modified_cells.remove(&row_id);
        let mut change = RowChange::new(row_id, ChangeType::Delete);
        change.original_row = Some(original_row);
        change.absent_columns = absent_columns;
        self.append_change(change);
        self.deleted_row_ids.insert(row_id);
    }

    pub fn record_row_insertion(
        &mut self,
        row_id: RowID,
        values: Vec<CellValue>,
        absent_columns: std::collections::BTreeSet<usize>,
    ) {
        if self.inserted_row_ids.contains(&row_id) {
            self.inserted_row_data.insert(row_id, values);
            if let Some(&insert_idx) = self
                .change_index
                .get(&RowChangeKey { row_id, change_type: ChangeType::Insert })
            {
                self.changes[insert_idx].absent_columns = absent_columns;
            }
            return;
        }
        self.inserted_row_data.insert(row_id, values);
        let mut change = RowChange::new(row_id, ChangeType::Insert);
        change.absent_columns = absent_columns;
        self.append_change(change);
        self.inserted_row_ids.insert(row_id);
    }

    // ---- Mutate (cancelling pending edits) ------------------------------

    pub fn undo_row_deletion(&mut self, row_id: RowID) -> bool {
        if !self.deleted_row_ids.contains(&row_id) {
            return false;
        }
        self.remove_change(row_id, ChangeType::Delete);
        self.deleted_row_ids.remove(&row_id);
        true
    }

    pub fn undo_row_insertion(&mut self, row_id: RowID) -> bool {
        if !self.inserted_row_ids.contains(&row_id) {
            return false;
        }
        self.remove_change(row_id, ChangeType::Insert);
        self.inserted_row_ids.remove(&row_id);
        self.inserted_row_data.remove(&row_id);
        true
    }

    /// Undo a batch of inserted rows. Returns the saved values for each row
    /// in the same order.
    ///
    /// `inserted_row_data` holds the whole row. `cell_changes` holds only
    /// the columns the user typed, so rebuilding from it drops the untouched
    /// ones and slides the rest left.
    pub fn undo_batch_row_insertion(&mut self, row_ids: &[RowID], column_count: usize) -> Vec<Vec<CellValue>> {
        let valid_rows: Vec<RowID> = row_ids
            .iter()
            .copied()
            .filter(|id| self.inserted_row_ids.contains(id))
            .collect();

        let row_values: Vec<Vec<CellValue>> = valid_rows
            .iter()
            .map(|row_id| {
                self.inserted_row_data
                    .get(row_id)
                    .cloned()
                    .unwrap_or_else(|| vec![CellValue::Null; column_count])
            })
            .collect();

        for row_id in valid_rows {
            self.undo_row_insertion(row_id);
        }
        row_values
    }

    // ---- Replay (driven by undo/redo) -----------------------------------

    pub fn reapply_row_deletion(
        &mut self,
        row_id: RowID,
        original_row: Vec<CellValue>,
        absent_columns: std::collections::BTreeSet<usize>,
    ) {
        self.record_row_deletion(row_id, original_row, absent_columns);
    }

    /// Re-apply a cell edit during undo replay (skips undo registration).
    /// `original_db_value` is the cell's value in the unmodified database
    /// row; it must be preserved so a later collapse compares correctly.
    pub fn reapply_cell_change(
        &mut self,
        row_id: RowID,
        column_index: usize,
        column_name: &str,
        original_db_value: CellValue,
        new_value: CellValue,
        original_row: Option<Vec<CellValue>>,
        absence: FieldAbsence,
    ) {
        let cell_change = CellChange {
            id: Uuid::new_v4(),
            column_index,
            column_name: column_name.to_string(),
            old_value: original_db_value,
            new_value: new_value.clone(),
            old_is_absent: absence.was_absent,
            new_is_absent: absence.is_absent,
        };

        if let Some(&insert_idx) = self
            .change_index
            .get(&RowChangeKey { row_id, change_type: ChangeType::Insert })
        {
            self.update_inserted_cell(
                insert_idx,
                column_index,
                column_name,
                new_value.clone(),
                absence.is_absent,
            );
            return;
        }

        let update_key = RowChangeKey { row_id, change_type: ChangeType::Update };
        if let Some(&update_idx) = self.change_index.get(&update_key) {
            self.merge_update_cell(update_idx, cell_change);
        } else {
            let row = RowChange {
                id: Uuid::new_v4(),
                row_id,
                change_type: ChangeType::Update,
                cell_changes: vec![cell_change],
                original_row,
                absent_columns: absence.original_row,
                sequence: 0,
            };
            self.append_change(row);
            self.modified_cells.entry(row_id).or_default().insert(column_index);
        }
    }

    /// Replace an inserted row's cell value during undo replay (no undo).
    pub fn update_inserted_cell_directly(
        &mut self,
        row_id: RowID,
        column_index: usize,
        column_name: &str,
        new_value: CellValue,
        is_absent: bool,
    ) {
        let Some(&insert_idx) = self
            .change_index
            .get(&RowChangeKey { row_id, change_type: ChangeType::Insert })
        else {
            return;
        };
        self.update_inserted_cell(insert_idx, column_index, column_name, new_value, is_absent);
    }

    /// Restore a cell's value during undo replay when an existing change
    /// matches.
    pub fn revert_update_cell(
        &mut self,
        row_id: RowID,
        column_index: usize,
        column_name: &str,
        previous_value: CellValue,
        previous_is_absent: bool,
    ) {
        let Some(&update_idx) = self
            .change_index
            .get(&RowChangeKey { row_id, change_type: ChangeType::Update })
        else {
            return;
        };
        let Some(cell_idx) = self.changes[update_idx]
            .cell_changes
            .iter()
            .position(|c| c.column_index == column_index)
        else {
            return;
        };

        let original = self.changes[update_idx].cell_changes[cell_idx].clone();
        if previous_value == original.old_value && previous_is_absent == original.old_is_absent {
            self.changes[update_idx].cell_changes.remove(cell_idx);
            self.remove_modified_cell(row_id, column_index);
            if self.changes[update_idx].cell_changes.is_empty() {
                self.remove_change_at(update_idx);
            }
        } else {
            self.changes[update_idx].cell_changes[cell_idx] = CellChange {
                id: Uuid::new_v4(),
                column_index,
                column_name: column_name.to_string(),
                old_value: original.old_value,
                new_value: previous_value,
                old_is_absent: original.old_is_absent,
                new_is_absent: previous_is_absent,
            };
        }
    }

    /// Insert a synthetic `.insert` RowChange for undo replay (e.g. after
    /// redoing a deletion's undo).
    pub fn reinsert_row(
        &mut self,
        row_id: RowID,
        columns: &[String],
        saved_values: Option<Vec<CellValue>>,
        absent_columns: std::collections::BTreeSet<usize>,
    ) {
        self.inserted_row_ids.insert(row_id);
        let cell_changes: Vec<CellChange> = columns
            .iter()
            .enumerate()
            .map(|(index, column_name)| CellChange {
                id: Uuid::new_v4(),
                column_index: index,
                column_name: column_name.clone(),
                old_value: CellValue::Null,
                new_value: saved_values
                    .as_ref()
                    .and_then(|v| v.get(index).cloned())
                    .unwrap_or(CellValue::Null),
                old_is_absent: false,
                new_is_absent: absent_columns.contains(&index),
            })
            .collect();
        let mut change = RowChange::new(row_id, ChangeType::Insert);
        change.cell_changes = cell_changes;
        change.absent_columns = absent_columns;
        self.append_change(change);
        if let Some(saved) = saved_values {
            self.inserted_row_data.insert(row_id, saved);
        }
    }

    /// Insert a batch of rows (for undo replay of a batch deletion's undo).
    pub fn reinsert_batch(
        &mut self,
        row_ids: &[RowID],
        row_values: Vec<Vec<CellValue>>,
        row_absent_columns: &[std::collections::BTreeSet<usize>],
        columns: &[String],
    ) {
        for (offset, (row_id, values)) in row_ids.iter().zip(row_values).enumerate() {
            let absent = row_absent_columns.get(offset).cloned().unwrap_or_default();
            let cell_changes: Vec<CellChange> = values
                .iter()
                .enumerate()
                .map(|(col_index, value)| CellChange {
                    id: Uuid::new_v4(),
                    column_index: col_index,
                    column_name: columns.get(col_index).cloned().unwrap_or_default(),
                    old_value: CellValue::Null,
                    new_value: value.clone(),
                    old_is_absent: false,
                    new_is_absent: absent.contains(&col_index),
                })
                .collect();
            let mut change = RowChange::new(*row_id, ChangeType::Insert);
            change.cell_changes = cell_changes;
            change.absent_columns = absent;
            self.append_change(change);
            self.inserted_row_ids.insert(*row_id);
            self.inserted_row_data.insert(*row_id, values);
        }
    }

    pub fn saved_inserted_values(&self, row_id: RowID) -> Option<&Vec<CellValue>> {
        self.inserted_row_data.get(&row_id)
    }

    pub fn inserted_absent_columns(&self, row_id: RowID) -> std::collections::BTreeSet<usize> {
        self.change_for_row(row_id, ChangeType::Insert)
            .map(|c| c.absent_columns.clone())
            .unwrap_or_default()
    }

    // ---- Reset ----------------------------------------------------------

    /// Restore from a snapshot (TablePro's `restore(from: TabChangeSnapshot)`).
    /// The change index is rebuilt; sequence numbering continues past the
    /// highest restored sequence.
    pub fn restore_snapshot(&mut self, snapshot: PendingChanges) {
        let max_sequence = snapshot.changes.iter().map(|c| c.sequence).max().unwrap_or(-1);
        *self = snapshot;
        self.next_sequence = max_sequence + 1;
    }

    pub fn snapshot(&self) -> PendingChanges {
        self.clone()
    }

    pub fn clear(&mut self) {
        self.next_sequence = 0;
        self.changes.clear();
        self.change_index.clear();
        self.deleted_row_ids.clear();
        self.inserted_row_ids.clear();
        self.modified_cells.clear();
        self.inserted_row_data.clear();
    }

    // ---- Internals ------------------------------------------------------

    fn append_change(&mut self, mut change: RowChange) {
        change.sequence = self.next_sequence;
        self.next_sequence += 1;
        self.changes.push(change.clone());
        self.change_index
            .insert(RowChangeKey { row_id: change.row_id, change_type: change.change_type }, self.changes.len() - 1);
    }

    fn remove_change(&mut self, row_id: RowID, change_type: ChangeType) -> bool {
        let key = RowChangeKey { row_id, change_type };
        let Some(&array_index) = self.change_index.get(&key) else {
            return false;
        };
        self.remove_change_at(array_index);
        true
    }

    /// Swap-remove: the last element moves into the slot. This is why
    /// `sequence` exists — array order stops matching edit order here.
    fn remove_change_at(&mut self, array_index: usize) {
        let removed = self.changes[array_index].clone();
        self.change_index.remove(&RowChangeKey {
            row_id: removed.row_id,
            change_type: removed.change_type,
        });

        let last_index = self.changes.len() - 1;
        if array_index != last_index {
            let moved = self.changes[last_index].clone();
            self.changes.swap(array_index, last_index);
            self.change_index.insert(
                RowChangeKey { row_id: moved.row_id, change_type: moved.change_type },
                array_index,
            );
        }
        self.changes.pop();
    }

    fn remove_modified_cell(&mut self, row_id: RowID, column_index: usize) {
        if let Some(cols) = self.modified_cells.get_mut(&row_id) {
            cols.remove(&column_index);
            if cols.is_empty() {
                self.modified_cells.remove(&row_id);
            }
        }
    }

    fn update_inserted_cell(
        &mut self,
        insert_idx: usize,
        column_index: usize,
        column_name: &str,
        new_value: CellValue,
        is_absent: bool,
    ) {
        let row_id = self.changes[insert_idx].row_id;
        if let Some(stored) = self.inserted_row_data.get_mut(&row_id) {
            if column_index < stored.len() {
                stored[column_index] = new_value.clone();
            }
        }
        if is_absent {
            self.changes[insert_idx].absent_columns.insert(column_index);
        } else {
            self.changes[insert_idx].absent_columns.remove(&column_index);
        }

        let replacement = CellChange {
            id: Uuid::new_v4(),
            column_index,
            column_name: column_name.to_string(),
            old_value: CellValue::Null,
            new_value,
            old_is_absent: false,
            new_is_absent: is_absent,
        };
        match self.changes[insert_idx]
            .cell_changes
            .iter()
            .position(|c| c.column_index == column_index)
        {
            Some(cell_idx) => self.changes[insert_idx].cell_changes[cell_idx] = replacement,
            None => self.changes[insert_idx].cell_changes.push(replacement),
        }
    }

    fn merge_update_cell(&mut self, update_idx: usize, cell_change: CellChange) {
        let row_id = self.changes[update_idx].row_id;
        let Some(cell_idx) = self.changes[update_idx]
            .cell_changes
            .iter()
            .position(|c| c.column_index == cell_change.column_index)
        else {
            let column_index = cell_change.column_index;
            self.changes[update_idx].cell_changes.push(cell_change);
            self.modified_cells.entry(row_id).or_default().insert(column_index);
            return;
        };

        let original = self.changes[update_idx].cell_changes[cell_idx].clone();
        let merged = CellChange {
            id: Uuid::new_v4(),
            column_index: cell_change.column_index,
            column_name: cell_change.column_name.clone(),
            old_value: original.old_value,
            new_value: cell_change.new_value,
            old_is_absent: original.old_is_absent,
            new_is_absent: cell_change.new_is_absent,
        };
        let restores_original = merged.restores_original();
        let column_index = merged.column_index;
        self.changes[update_idx].cell_changes[cell_idx] = merged;

        if !restores_original {
            return;
        }
        self.changes[update_idx].cell_changes.remove(cell_idx);
        self.remove_modified_cell(row_id, column_index);
        if self.changes[update_idx].cell_changes.is_empty() {
            self.remove_change_at(update_idx);
        }
    }

    fn rollback_cell_if_matches_original(
        &mut self,
        row_id: RowID,
        column_index: usize,
        restored_value: &CellValue,
        restored_is_absent: bool,
    ) -> bool {
        let update_key = RowChangeKey { row_id, change_type: ChangeType::Update };
        let Some(&update_idx) = self.change_index.get(&update_key) else {
            return false;
        };
        let Some(cell_idx) = self.changes[update_idx]
            .cell_changes
            .iter()
            .position(|c| c.column_index == column_index)
        else {
            return false;
        };
        let cell = &self.changes[update_idx].cell_changes[cell_idx];
        if cell.old_value != *restored_value || cell.old_is_absent != restored_is_absent {
            return false;
        }
        self.changes[update_idx].cell_changes.remove(cell_idx);
        self.remove_modified_cell(row_id, column_index);
        if self.changes[update_idx].cell_changes.is_empty() {
            self.remove_change_at(update_idx);
        }
        true
    }
}
