//! CSV / delimited data: field escaping, a streaming parser, dialect
//! detection, and type inference.
//!
//! Ported from TablePro's `PluginRowWriters.csvField`, `CSVStreamingParser`,
//! `CSVDialect.detect`, and `CSVTypeInferrer`, preserving the exact rules
//! their tests pin down.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::value::CellValue;

/// How a CSV field's quoting is decided (TablePro's `CSVQuoteHandling`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QuoteHandling {
    Always,
    Never,
    #[default]
    AsNeeded,
}

/// Options for turning a cell value into a CSV field (TablePro's
/// `CSVExportOptions` write-side subset).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvFieldOptions {
    /// Flatten `\n`/`\r` inside a value to spaces (off by default).
    pub convert_line_break_to_space: bool,
    /// Neutralize spreadsheet formula leads (`=`, `+`, `-`, `@` — and a
    /// leading tab/CR, which Excel strips before parsing) by prefixing `'`.
    pub sanitize_formulas: bool,
    pub quote_handling: QuoteHandling,
}

impl Default for CsvFieldOptions {
    fn default() -> Self {
        Self {
            convert_line_break_to_space: false,
            sanitize_formulas: true,
            quote_handling: QuoteHandling::AsNeeded,
        }
    }
}

/// Turn one value into one CSV field. Ported from
/// `PluginRowWriters.csvField`.
pub fn csv_field(value: &str, delimiter: char, options: &CsvFieldOptions) -> String {
    let mut processed = value.to_string();

    if options.convert_line_break_to_space {
        processed = processed.replace(['\n', '\r'], " ");
    }

    let mut was_neutralised = false;
    if options.sanitize_formulas {
        // Excel strips a leading tab/CR before parsing, so "\t=1+1" reaches
        // the formula engine as "=1+1" — a leading tab or CR counts too.
        if processed.starts_with(['=', '+', '-', '@', '\t', '\r']) {
            processed = format!("'{processed}");
            was_neutralised = true;
        }
    }

    let needs_quoting = match options.quote_handling {
        QuoteHandling::Always => true,
        QuoteHandling::Never => false,
        QuoteHandling::AsNeeded => {
            was_neutralised
                || processed.contains(delimiter)
                || processed.contains('"')
                || processed.contains('\n')
                || processed.contains('\r')
                || processed.contains('\t')
        }
    };

    if needs_quoting {
        format!("\"{}\"", processed.replace('"', "\"\""))
    } else {
        processed
    }
}

/// A byte-level, RFC 4180 flavored CSV row scanner. Quote-opening only at
/// field start; `""` doubling; distinct escape chars consume the next byte
/// literally; rows end at raw CR or LF outside quotes (CRLF is one row end).
#[derive(Debug, Clone)]
pub struct CsvStreamingParser {
    pub delimiter: char,
    /// Escape character distinct from the quote char, if any.
    pub escape: Option<char>,
}

impl Default for CsvStreamingParser {
    fn default() -> Self {
        Self { delimiter: ',', escape: None }
    }
}

impl CsvStreamingParser {
    /// Split the input into rows, each row a list of decoded fields.
    /// Empty field → `""`; there is no NULL at the parser level.
    pub fn parse(&self, input: &str) -> Vec<Vec<String>> {
        let chars: Vec<char> = input.chars().collect();
        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut row: Vec<String> = Vec::new();
        let mut field = String::new();
        let mut field_has_content = false; // for quote-at-field-start rule
        let mut inside_quotes = false;
        let mut i = 0;

        while i < chars.len() {
            let c = chars[i];

            if inside_quotes {
                if c == '"' {
                    if self.escape.is_none() {
                        // doubled quote inside quotes is a literal quote
                        if i + 1 < chars.len() && chars[i + 1] == '"' {
                            field.push('"');
                            i += 2;
                            continue;
                        }
                        inside_quotes = false;
                        i += 1;
                        continue;
                    }
                    // distinct escape char: consume the next byte literally
                    if i + 1 < chars.len() && chars[i + 1] == '"' {
                        field.push('"');
                        i += 2;
                        continue;
                    }
                    inside_quotes = false;
                    i += 1;
                    continue;
                }
                field.push(c);
                i += 1;
                continue;
            }

            if c == '"' && field.is_empty() && !field_has_content {
                inside_quotes = true;
                field_has_content = true;
                i += 1;
                continue;
            }

            if self.escape.is_some() && Some(c) == self.escape && i + 1 < chars.len() {
                field.push(chars[i + 1]);
                field_has_content = true;
                i += 2;
                continue;
            }

            if c == self.delimiter {
                row.push(std::mem::take(&mut field));
                field_has_content = false;
                i += 1;
                continue;
            }

            if c == '\n' || c == '\r' {
                row.push(std::mem::take(&mut field));
                field_has_content = false;
                rows.push(std::mem::take(&mut row));
                if c == '\r' && i + 1 < chars.len() && chars[i + 1] == '\n' {
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }

            field.push(c);
            field_has_content = true;
            i += 1;
        }

        // Trailing partial row (only when there is content or a pending
        // field). A fully empty trailing area adds nothing.
        if !field.is_empty() || field_has_content || !row.is_empty() {
            row.push(field);
            rows.push(row);
        }

        rows
    }
}

/// The detected shape of a delimited file (TablePro's `CSVDialect`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelimitedDialect {
    pub delimiter: char,
    pub encoding_is_utf8: bool,
}

/// Sniff a delimited file's encoding and delimiter.
///
/// Without a BOM: probe UTF-8 validity; valid → UTF-8, else Windows-1252.
/// Delimiter: count `, \t ; |` over a sample outside double quotes and take
/// the max; ties go to the file-extension default.
pub fn detect_dialect(input: &str, file_extension: Option<&str>) -> DelimitedDialect {
    let encoding_is_utf8 = std::str::from_utf8(input.as_bytes()).is_ok();

    let sample: Vec<char> = input.chars().take(64 * 1024).collect();
    let mut counts: [usize; 4] = [0; 4]; // ',' '\t' ';' '|'
    let candidates = [',', '\t', ';', '|'];
    let mut inside_quotes = false;
    let mut i = 0;
    while i < sample.len() {
        let c = sample[i];
        if c == '"' {
            if inside_quotes && i + 1 < sample.len() && sample[i + 1] == '"' {
                i += 2;
                continue;
            }
            inside_quotes = !inside_quotes;
        } else if !inside_quotes {
            if let Some(ix) = candidates.iter().position(|&d| d == c) {
                counts[ix] += 1;
            }
        }
        i += 1;
    }

    let max = counts.iter().copied().max().unwrap_or(0);
    let mut delimiter = ',';
    if max > 0 {
        let extension_default = match file_extension {
            Some("tsv") | Some("tab") => '\t',
            Some("psv") => '|',
            _ => ',',
        };
        let best = candidates
            .iter()
            .zip(counts)
            .filter(|(_, n)| *n == max)
            .map(|(d, _)| *d)
            .find(|d| *d == extension_default)
            .unwrap_or_else(|| {
                candidates
                    .iter()
                    .zip(counts)
                    .find(|(_, n)| *n == max)
                    .map(|(d, _)| *d)
                    .unwrap()
            });
        delimiter = best;
    }

    DelimitedDialect { delimiter, encoding_is_utf8 }
}

/// Infer a column's type from up to 200 non-empty sampled values (TablePro's
/// `CSVTypeInferrer`). Order matters: integer before real; boolean accepts
/// the single letters t/f/y/n case-insensitively; dates are ISO-8601.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferredType {
    Integer,
    Real,
    Boolean,
    Date,
    Text,
}

pub fn infer_column_type(values: &[&str]) -> InferredType {
    let sample: Vec<&str> = values
        .iter()
        .filter(|v| !v.is_empty())
        .take(200)
        .copied()
        .collect();
    if sample.is_empty() {
        return InferredType::Text;
    }

    if sample.iter().all(|v| v.parse::<i64>().is_ok()) {
        return InferredType::Integer;
    }
    if sample.iter().all(|v| v.parse::<f64>().is_ok()) {
        return InferredType::Real;
    }
    if sample.iter().all(|v| {
        matches!(
            v.to_lowercase().as_str(),
            "true" | "false" | "yes" | "no" | "t" | "f" | "y" | "n"
        )
    }) {
        return InferredType::Boolean;
    }
    if sample.iter().all(|v| {
        chrono::DateTime::parse_from_rfc3339(v).is_ok()
            || chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d").is_ok()
    }) {
        return InferredType::Date;
    }
    InferredType::Text
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultExportFormat {
    Csv,
    Json,
}

impl ResultExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Json => "json",
        }
    }
}

/// Encode one result set without losing NULL/binary distinctions.
pub fn encode_result(
    format: ResultExportFormat,
    columns: &[String],
    rows: &[Vec<CellValue>],
) -> std::result::Result<Vec<u8>, serde_json::Error> {
    match format {
        ResultExportFormat::Csv => {
            let options = CsvFieldOptions::default();
            let mut out = String::new();
            let header = columns
                .iter()
                .map(|name| csv_field(name, ',', &options))
                .collect::<Vec<_>>()
                .join(",");
            out.push_str(&header);
            out.push('\n');
            for row in rows {
                let values = row
                    .iter()
                    .map(|value| match value {
                        CellValue::Null => String::new(),
                        CellValue::Text(text) => csv_field(text, ',', &options),
                        CellValue::Bytes(bytes) => {
                            let hex = bytes
                                .iter()
                                .map(|byte| format!("{byte:02X}"))
                                .collect::<String>();
                            csv_field(&format!("0x{hex}"), ',', &options)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                out.push_str(&values);
                out.push('\n');
            }
            Ok(out.into_bytes())
        }
        ResultExportFormat::Json => {
            let records = rows
                .iter()
                .map(|row| {
                    let mut record = serde_json::Map::new();
                    for (index, column) in columns.iter().enumerate() {
                        let value = match row.get(index) {
                            None | Some(CellValue::Null) => serde_json::Value::Null,
                            Some(CellValue::Text(text)) => {
                                serde_json::Value::String(text.clone())
                            }
                            Some(CellValue::Bytes(bytes)) => {
                                let hex = bytes
                                    .iter()
                                    .map(|byte| format!("{byte:02X}"))
                                    .collect::<String>();
                                serde_json::Value::String(format!("0x{hex}"))
                            }
                        };
                        record.insert(column.clone(), value);
                    }
                    serde_json::Value::Object(record)
                })
                .collect::<Vec<_>>();
            serde_json::to_vec_pretty(&records)
        }
    }
}

/// Atomically create an export file without overwriting an existing path.
///
/// When `preferred` already exists, `-2`, `-3`, ... is appended before the
/// extension. The temporary file lives beside the destination so `rename` is
/// atomic on the target filesystem.
pub fn write_export_unique(preferred: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    let destination = unique_path(preferred);
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;

    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("sqlpro-export");
    let temp = parent.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));

    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, &destination)?;
        Ok(destination.clone())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn unique_path(preferred: &Path) -> PathBuf {
    if !preferred.exists() {
        return preferred.to_path_buf();
    }

    let parent = preferred.parent().unwrap_or_else(|| Path::new("."));
    let stem = preferred
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("sqlpro-export");
    let extension = preferred.extension().and_then(|value| value.to_str());

    for suffix in 2usize.. {
        let file_name = match extension {
            Some(extension) => format!("{stem}-{suffix}.{extension}"),
            None => format!("{stem}-{suffix}"),
        };
        let candidate = parent.join(file_name);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(value: &str) -> String {
        csv_field(value, ',', &CsvFieldOptions::default())
    }

    // Ported from PluginRowWriters / CsvRowConverterTests semantics.

    #[test]
    fn plain_values_pass_through() {
        assert_eq!(field("hello"), "hello");
        assert_eq!(field(""), "");
    }

    #[test]
    fn comma_quoting() {
        assert_eq!(field("a,b"), "\"a,b\"");
    }

    #[test]
    fn quote_escape() {
        assert_eq!(field("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn newline_quoting() {
        assert_eq!(field("line1\nline2"), "\"line1\nline2\"");
    }

    #[test]
    fn formula_leads_are_neutralised() {
        assert_eq!(field("=1+1"), "\"'=1+1\"");
        assert_eq!(field("+SUM(A1)"), "\"'+SUM(A1)\"");
        assert_eq!(field("-1"), "\"'-1\"");
        assert_eq!(field("@cmd"), "\"'@cmd\"");
        // Leading tab counts as a formula lead: Excel strips it first.
        assert_eq!(field("\t=1+1"), "\"'\t=1+1\"");
    }

    #[test]
    fn sanitizing_can_be_turned_off() {
        let options = CsvFieldOptions { sanitize_formulas: false, ..Default::default() };
        assert_eq!(csv_field("=1+1", ',', &options), "=1+1");
    }

    #[test]
    fn line_breaks_become_spaces_when_asked() {
        let options = CsvFieldOptions { convert_line_break_to_space: true, ..Default::default() };
        assert_eq!(csv_field("a\nb", ',', &options), "a b");
    }

    #[test]
    fn quote_never_passes_raw() {
        let options = CsvFieldOptions { quote_handling: QuoteHandling::Never, ..Default::default() };
        assert_eq!(csv_field("a,b", ',', &options), "a,b");
    }

    #[test]
    fn quote_always_wraps() {
        let options = CsvFieldOptions { quote_handling: QuoteHandling::Always, ..Default::default() };
        assert_eq!(csv_field("plain", ',', &options), "\"plain\"");
    }

    #[test]
    fn other_delimiters_count_for_quoting() {
        assert_eq!(field("a;b"), "a;b");
        assert_eq!(csv_field("a;b", ';', &CsvFieldOptions::default()), "\"a;b\"");
    }

    // ---- Parser (ported from CSVImportPluginTests) ----

    #[test]
    fn splits_basic_rows() {
        let parser = CsvStreamingParser::default();
        assert_eq!(
            parser.parse("a,b,c\n1,2,3\n"),
            vec![vec!["a", "b", "c"], vec!["1", "2", "3"]]
        );
    }

    #[test]
    fn quoted_fields_with_embedded_delimiter_and_newline() {
        let parser = CsvStreamingParser::default();
        assert_eq!(
            parser.parse("\"a,b\",\"line1\nline2\"\n"),
            vec![vec!["a,b", "line1\nline2"]]
        );
    }

    #[test]
    fn doubled_quotes_inside_quoted_field() {
        let parser = CsvStreamingParser::default();
        assert_eq!(parser.parse("\"he said \"\"hi\"\"\""), vec![vec!["he said \"hi\""]]);
    }

    #[test]
    fn crlf_is_one_row_end() {
        let parser = CsvStreamingParser::default();
        assert_eq!(parser.parse("a,b\r\n1,2\r\n"), vec![vec!["a", "b"], vec!["1", "2"]]);
    }

    #[test]
    fn trailing_partial_row_included() {
        let parser = CsvStreamingParser::default();
        assert_eq!(parser.parse("a,b\nc"), vec![vec!["a", "b"], vec!["c"]]);
    }

    #[test]
    fn empty_lines_produce_empty_single_field_rows() {
        let parser = CsvStreamingParser::default();
        assert_eq!(parser.parse("\n\n"), vec![vec![""], vec![""]]);
    }

    // ---- Detection (ported from CSVDialect/DelimitedDialectDetector) ----

    #[test]
    fn detects_semicolon() {
        let d = detect_dialect("a;b;c\n1;2;3\n", None);
        assert_eq!(d.delimiter, ';');
    }

    #[test]
    fn extension_breaks_ties() {
        // one comma and one tab: the .tsv extension wins
        let d = detect_dialect("a\tb,c\n", Some("tsv"));
        assert_eq!(d.delimiter, '\t');
    }

    #[test]
    fn quotes_do_not_count_delimiters() {
        let d = detect_dialect("\"a;b\",c\n", None);
        assert_eq!(d.delimiter, ',');
    }

    // ---- Type inference (ported from CSVTypeInferrer / ImportFieldTypeMapping) ----

    #[test]
    fn integer_column() {
        assert_eq!(infer_column_type(&["1", "2", "-5"]), InferredType::Integer);
    }

    #[test]
    fn real_column() {
        assert_eq!(infer_column_type(&["1.5", "2", "1e5"]), InferredType::Real);
    }

    #[test]
    fn scientific_is_not_integer() {
        assert_eq!(infer_column_type(&["1e5", "2"]), InferredType::Real);
    }

    #[test]
    fn boolean_single_letters() {
        assert_eq!(infer_column_type(&["t", "F", "y", "N"]), InferredType::Boolean);
        assert_eq!(infer_column_type(&["yes", "no", "true", "false"]), InferredType::Boolean);
    }

    #[test]
    fn iso_dates() {
        assert_eq!(infer_column_type(&["2023-01-15"]), InferredType::Date);
        assert_eq!(infer_column_type(&["2023-01-15T10:30:00Z"]), InferredType::Date);
    }

    #[test]
    fn mixed_falls_back_to_text() {
        assert_eq!(infer_column_type(&["1", "abc"]), InferredType::Text);
    }

    #[test]
    fn empty_sample_is_text() {
        assert_eq!(infer_column_type(&["", ""]), InferredType::Text);
    }

    #[test]
    fn result_csv_preserves_null_and_binary_distinctions() {
        let bytes = encode_result(
            ResultExportFormat::Csv,
            &["name".into(), "payload".into(), "missing".into()],
            &[vec![
                CellValue::text("=formula"),
                CellValue::bytes([0x00, 0xAF]),
                CellValue::Null,
            ]],
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            "name,payload,missing\n\"'=formula\",0x00AF,\n"
        );
    }

    #[test]
    fn result_json_uses_objects_and_real_nulls() {
        let bytes = encode_result(
            ResultExportFormat::Json,
            &["id".into(), "value".into()],
            &[vec![CellValue::text("1"), CellValue::Null]],
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value, serde_json::json!([{ "id": "1", "value": null }]));
    }

    #[test]
    fn atomic_export_never_overwrites_an_existing_file() {
        let root = std::env::temp_dir().join(format!("sqlpro-export-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let preferred = root.join("results.csv");
        std::fs::write(&preferred, b"original").unwrap();

        let written = write_export_unique(&preferred, b"replacement").unwrap();
        assert_eq!(written.file_name().unwrap(), "results-2.csv");
        assert_eq!(std::fs::read(&preferred).unwrap(), b"original");
        assert_eq!(std::fs::read(&written).unwrap(), b"replacement");

        std::fs::remove_dir_all(root).unwrap();
    }
}
