# TablePro → SQL Pro test porting inventory

Source: [TablePro](https://github.com/TableProApp/TablePro) `TableProTests/` — **2,189 files / ~27,800 test methods**, of which **~24,542 methods in ~1,874 files are pure-logic portable** to Rust (no AppKit/SwiftUI/XCUITest/Keychain/CloudKit).

This file is the tracking list. Ported suites live as Rust test modules alongside the code they test, with Swift test names preserved.

## Ported so far (189 Rust tests passing)

| Swift suite | Swift cases | Rust location |
|---|---|---|
| `Core/ChangeTracking/DataChangeManagerTests` (26), `DataChangeManagerExtendedTests` (49 subset), `PendingChangesTests` (18 subset), `PendingChangesRowIdentityTests` (7 subset), `FieldAbsenceTrackingTests` (subset), `DataChangeModelsTests` (subset) | ~130 | `crates/core/tests/change_tracking_tests.rs` (67) |
| `SQLStatementGeneratorTests` (41), `SQLStatementGeneratorCompositePKTests` (22 subset), `SQLStatementGeneratorNoPKTests` (9), `SQLStatementGeneratorParameterStyleTests` (11), `SQLStatementGeneratorMSSQLTests` (7), `SQLStatementGeneratorSchemaTests` (6 subset), `SQLStatementGeneratorOrderingTests` (5), `SQLStatementGeneratorRowMatchTests`/`RowMatchTextTests` (subset), `SQLStatementGeneratorBatchDeleteScaleTests` (subset), `SQLStatementGeneratorPKRegressionTests` (subset) | ~120 | `crates/core/tests/sql_generator_tests.rs` (57) |
| `Core/DataWrite/DataWriteExecutorTests` (21 subset) + full-pipeline integration | ~15 | `crates/core/tests/data_write_tests.rs` (10) |
| `Plugins/CSVImportPluginTests` (subset), `CSVExportBytesTests`/`EncodingTests` (subset), `Core/Utilities/CsvRowConverterTests` (subset), `CSVTypeInferrer` | ~35 | `crates/core/src/csv.rs` (inline, 24) |
| `Core/Storage/QueryHistoryStorageTests` (34 subset) + `SQLQueryFingerprintTests` (subset) | ~20 | `crates/core/src/history.rs` + `fingerprint.rs` (24 inline) |
| Driver E2E (SQLite pipeline, PG connection validation) | 6 | `crates/drivers/tests/sqlite.rs` |

## Next clusters, in port order (Swift counts)

1. `Core/Utilities/SQL/` — **606 tests**, all pure SQL text logic: `SQLFileParser` (29+22+6), `SQLStatementScanner` (43+17), `QueryClassifier` (27+51+13+19+8+7+3+21), `SQLQueryFingerprint` (22 — finish the port), `SelectSourceTableParser` (33), `TableEditStatementParser` (31), `SQLLimitDetector` (20), `SQLTokenCursor` (17), `SqlLexer` (15), `StatementBlank` (18), `SQLScriptText` (23), fold scanner/labels (32+9), `TextPrefixDecoder` (18), `InvisibleCharacterRemover` (18), more.
2. `Core/Utilities/` root — **782 tests**: `ConnectionURLParser` (197 + per-engine suites), `DatabaseURLScheme` (36), `SqlParameterExtractor` (31), `SQLRowToStatementConverter` (42), `JsonRowConverter` (31), `PasswordSourceResolver` (30), `InClauseConverter` (13), `FuzzyMatcher` (23, in Utilities/), `SidebarNameFilter` (14), `RowSortComparator` (10), `QualifiedSearchQuery` (12), `StringHexDump` (18), `StringSHA256` (5), `ByteOrderMark` (5), more.
3. Rest of `Core/ChangeTracking` + `Core/DataWrite` — remaining ~350 cases (UndoAction replay coverage, `RowChangeStatementFactoryCoverage` 23, `ServerOwned` 11, `RewindPlanner` 12, `SQLWriteBatchBudget` 13, `FieldAbsenceWrite` 20, bulk-delete confirmation 5).
4. CSV/TabularIO remainder — ~280 cases: full `CSVImportPlugin` (35+), encoding suites (21+3), `ExportFormatEscaping` (44), XLSX (17), JSON import (56), `RowImportRunner` (7).
5. `Database/` — 191 tests: native dump registry/scope/destination/service (89), CLI tool probes, restore diagnostics (23). Needs a `Process` runner abstraction (std::process is fine).
6. `Models/` — ~1,495 tests: `DatabaseType` (80), `TableRows` (63), `PaginationState` (53), `TableInfo` (54), `MultiRowEditState` (68), `SortState` (26), `TableFilter` (28), `CreateTableDraftBuilder` (27), more.
7. `Core/Storage/` — ~642 tests (skip the Keychain ones): `FilterSettingsStorage` (30), `FavoriteTablesStorage` (24), `SQLFavoriteStorage` (22+22+16+15), `RecentTablesStore` (15), `FileColumnLayoutPersister` (30), more.
8. `Plugins/` — 362 files of pure-logic codecs: Etcd command parser (101), Elasticsearch driver (86), MongoDB query builder (90), BigQuery (65), ClickHouse response classifier (30), MSSQL/Redis/Mongo statement generators, Kafka codecs, DynamoDB (PartiQL/SigV4).
9. `SOCKS/` (5), `RemoteSQLite/` (5), `TunnelCommand/` (6), `TeamLibrary/` (2), `Entra/` (1), `AWS/` (13 of 14), `Localization/` (4).
10. `Views/` policy/logic tests (~150 files) and `Services/` logic tests (~17 files).

## Porting notes (semantics that bit during porting)

- `PendingChanges.removeChangeAt` swaps the LAST element into the slot — array order diverges from edit order; `sequence` is the truth. Statement generation sorts by sequence.
- Keyed verification is `actual > expected` only; keyless is two-sided but only on real-count engines (mysql family, pg family, sqlite family, mssql, oracle).
- MySQL-family literal escaping: backslash FIRST, then `''`, `\n`, `\r`, `\t`, `\0`, `\b`, `\Z` (SUB truncates the Windows mysql client). ANSI engines: double `'`, strip NUL.
- CSV: quote-opening only at field start; `""` doubling when escape==quote; formula sanitization treats a leading tab/CR as a formula lead (Excel strips them first); a field whose line breaks were flattened stays force-quoted.
- Fingerprint: identifier case preserved, `"…"` is an identifier (string only for the MySQL family), `IN`/`VALUES` literal-only lists collapse to `(...)`, sign swallowed after `( , ;` keyword/operator (not `]`), digest = first 8 bytes of SHA-256.
- FTS query: every whitespace word becomes `"term"*`, ANDed, internal `"` doubled.
- Retention: 0 = unlimited; keep-newest delete is `ORDER BY executed_at DESC, id DESC LIMIT -1 OFFSET n`.
