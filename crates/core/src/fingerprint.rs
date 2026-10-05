//! Query fingerprinting: a process-stable grouping key for query history.
//!
//! Port of TablePro's `SQLQueryFingerprint`: normalize a query the way
//! `pg_stat_statements` / MySQL digest do, then take the first 8 bytes of
//! SHA-256 of the normalized shape as the digest. Identifier case is
//! deliberately preserved (folding it would merge distinct tables on
//! case-sensitive servers), but quoted identifiers are unquoted.

use sha2::{Digest, Sha256};

const MAX_DIGEST_LENGTH: usize = 20_000;

/// Replace dollar-quoted bodies ($$…$$, $tag$…$tag$) with `''` before
/// tokenizing.
fn strip_dollar_quotes(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' {
            if let Some(tag_len) = scan_dollar_tag(&chars[i..]) {
                let tag: Vec<char> = chars[i..i + tag_len].to_vec();
                out.push('\'');
                out.push('\'');
                i += tag_len;
                while i + tag_len <= chars.len() {
                    if chars[i..i + tag_len] == tag[..] {
                        i += tag_len;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn scan_dollar_tag(chars: &[char]) -> Option<usize> {
    if chars.first() != Some(&'$') {
        return None;
    }
    let mut len = 1;
    while len < chars.len() {
        if chars[len] == '$' {
            return Some(len + 1);
        }
        if !(chars[len].is_alphanumeric() || chars[len] == '_') {
            return None;
        }
        len += 1;
    }
    None
}

/// Normalize `query` into its digest shape.
pub fn normalize(query: &str) -> String {
    // Truncate first, like max_digest_length.
    let truncated: String = query.chars().take(MAX_DIGEST_LENGTH).collect();

    // Dollar-quoted bodies ($$…$$, $tag$…$tag$) collapse to ''.
    let no_dollar = strip_dollar_quotes(&truncated);

    let mut out = String::with_capacity(no_dollar.len());
    let chars: Vec<char> = no_dollar.chars().collect();
    let mut i = 0;
    let mut last_literal = false;
    let mut prev_significant: Option<char> = None;

    let keywords_fold = |token: &str| -> String { token.to_uppercase() };

    while i < chars.len() {
        let c = chars[i];

        // Comments
        if c == '-' && i + 1 < chars.len() && chars[i + 1] == '-' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(chars.len());
            continue;
        }

        // Whitespace
        if c.is_whitespace() {
            i += 1;
            continue;
        }

        // Single-quoted string → ?
        if c == '\'' {
            i += 1;
            while i < chars.len() {
                if chars[i] == '\'' {
                    if i + 1 < chars.len() && chars[i + 1] == '\'' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            out = collapse_literal(out, &mut last_literal);
            prev_significant = Some('?');
            continue;
        }

        // Double-quoted identifier: a string only in the MySQL dialect
        // family; here we treat it as an identifier (keep inner text,
        // unquoted, original case).
        if c == '"' || c == '`' {
            let quote = c;
            i += 1;
            let mut ident = String::new();
            while i < chars.len() {
                if chars[i] == quote {
                    if i + 1 < chars.len() && chars[i + 1] == quote {
                        ident.push(quote);
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                ident.push(chars[i]);
                i += 1;
            }
            if needs_space_between(prev_significant) {
                out.push(' ');
            }
            out.push_str(&ident);
            last_literal = false;
            prev_significant = ident.chars().last();
            continue;
        }

        // Number → ?
        if c.is_ascii_digit() {
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '.') {
                i += 1;
            }
            out = collapse_literal(out, &mut last_literal);
            prev_significant = Some('?');
            continue;
        }

        // Word (keyword or identifier)
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let token: String = chars[start..i].iter().collect();
            let upper = token.to_uppercase();
            if matches!(
                upper.as_str(),
                "NULL" | "TRUE" | "FALSE" | "CURRENT_TIMESTAMP" | "CURRENT_DATE" | "CURRENT_TIME"
            ) {
                out = collapse_literal(out, &mut last_literal);
                prev_significant = Some('?');
            } else {
                // Keywords fold to upper; identifiers keep original case.
                let rendered = if looks_like_keyword(&upper) {
                    keywords_fold(&token)
                } else {
                    token
                };
                if needs_space_between(prev_significant) {
                    out.push(' ');
                }
                out.push_str(&rendered);

                // IN / VALUES followed purely by a literal list collapse to
                // `IN (...)` / `VALUES (...)` (MySQL digest spelling).
                if upper == "IN" || upper == "VALUES" {
                    if let Some(after_close) = scan_pure_literal_list(&chars, i) {
                        out.push_str(" (...)");
                        i = after_close;
                        last_literal = false;
                        prev_significant = Some(')');
                        continue;
                    }
                }

                last_literal = false;
                prev_significant = rendered.chars().last();
            }
            continue;
        }

        // Sign suppression: a -/+ right after ( , ; a keyword or an
        // operator (but not ]) is a sign and swallowed with its number.
        if (c == '-' || c == '+') && prev_significant.is_some() && prev_significant != Some(']') {
            let prev = prev_significant.unwrap();
            let after_open = matches!(prev, '(' | ',' | ';');
            let after_operator = matches!(prev, '=' | '<' | '>' | '+' | '-' | '*' | '/' | '%');
            if after_open || after_operator || prev.is_alphanumeric() || prev == '_' || prev == '?' {
                // part of an operator or a signed literal
                if after_open || after_operator {
                    // operator character itself already pushed; swallow the sign
                    i += 1;
                    continue;
                }
            }
        }

        if needs_space_before(c, prev_significant) {
            out.push(' ');
        }
        out.push(c);
        last_literal = false;
        prev_significant = Some(c);
        i += 1;
    }

    out
}

fn looks_like_keyword(token: &str) -> bool {
    matches!(
        token,
        "SELECT" | "FROM" | "WHERE" | "INSERT" | "INTO" | "VALUES" | "UPDATE" | "SET" | "DELETE"
            | "JOIN" | "LEFT" | "RIGHT" | "INNER" | "OUTER" | "ON" | "AND" | "OR" | "NOT" | "IN"
            | "GROUP" | "BY" | "ORDER" | "HAVING" | "LIMIT" | "OFFSET" | "AS" | "IS" | "LIKE"
            | "BETWEEN" | "CREATE" | "DROP" | "ALTER" | "TABLE" | "INDEX" | "VIEW" | "DISTINCT"
            | "CASE" | "WHEN" | "THEN" | "ELSE" | "END" | "WITH" | "UNION" | "ALL" | "EXISTS"
            | "ASC" | "DESC" | "NULL" | "RETURNING"
    )
}

/// Consecutive literals collapse to one `?`.
fn collapse_literal(mut out: String, last_literal: &mut bool) -> String {
    if *last_literal {
        // already a ? at the end
    } else {
        if out
            .chars()
            .last()
            .map(|c| !matches!(c, ' ' | '(' | ',' | ';'))
            .unwrap_or(false)
        {
            out.push(' ');
        }
        out.push('?');
    }
    *last_literal = true;
    out
}

fn needs_space_between(prev: Option<char>) -> bool {
    match prev {
        None => false,
        Some(c) => !matches!(c, '(' | '.' | '['),
    }
}

fn needs_space_before(c: char, prev: Option<char>) -> bool {
    if matches!(c, ',' | ')' | ';' | '.' | ']' | '[') {
        return false;
    }
    needs_space_between(prev)
}

/// After `from_ix` (just past an IN/VALUES keyword), decide whether the next
/// non-space token is a parenthesized list containing ONLY literals
/// (numbers, strings, `?`, NULL/TRUE/FALSE). Returns the index just past
/// the closing paren if so.
fn scan_pure_literal_list(chars: &[char], from_ix: usize) -> Option<usize> {
    let mut j = from_ix;
    while j < chars.len() && chars[j].is_whitespace() {
        j += 1;
    }
    if j >= chars.len() || chars[j] != '(' {
        return None;
    }
    let mut depth = 0usize;
    let mut items = 0usize;
    let mut pending_item_chars = false;
    let mut saw_comma = false;
    j += 1;
    while j < chars.len() {
        let c = chars[j];
        match c {
            '(' => {
                depth += 1;
                j += 1;
            }
            ')' => {
                if depth == 0 {
                    // trailing item must have been a literal
                    if pending_item_chars || saw_comma {
                        items += 1;
                    }
                    return if items > 0 { Some(j + 1) } else { None };
                }
                depth -= 1;
                j += 1;
            }
            ',' => {
                if depth == 0 {
                    items += 1;
                    saw_comma = true;
                    pending_item_chars = false;
                }
                j += 1;
            }
            '\'' => {
                // string literal
                j += 1;
                while j < chars.len() {
                    if chars[j] == '\'' {
                        if j + 1 < chars.len() && chars[j + 1] == '\'' {
                            j += 2;
                            continue;
                        }
                        break;
                    }
                    j += 1;
                }
                pending_item_chars = true;
                j += 1;
            }
            '?' | '0'..='9' | '.' | '-' | '+' => {
                pending_item_chars = true;
                j += 1;
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = j;
                while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
                let word: String = chars[start..j].iter().collect();
                if matches!(
                    word.to_uppercase().as_str(),
                    "NULL" | "TRUE" | "FALSE"
                ) {
                    pending_item_chars = true;
                } else {
                    return None; // a non-literal item: leave the list alone
                }
            }
            c if c.is_whitespace() => j += 1,
            _ => return None, // operators/identifiers: not a pure literal list
        }
    }
    None
}

/// The digest: first 8 bytes of SHA-256 of the normalized shape, as i64.
pub fn fingerprint_hash(query: &str) -> i64 {
    let normalized = normalize(query);
    let digest = Sha256::digest(normalized.as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    i64::from_be_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ported from SQLQueryFingerprintTests behaviors.

    #[test]
    fn same_shape_same_hash() {
        assert_eq!(
            fingerprint_hash("SELECT * FROM users WHERE id = 1"),
            fingerprint_hash("SELECT * FROM users WHERE id = 42")
        );
    }

    #[test]
    fn different_tables_different_hash() {
        assert_ne!(
            fingerprint_hash("SELECT * FROM users WHERE id = 1"),
            fingerprint_hash("SELECT * FROM customers WHERE id = 1")
        );
    }

    #[test]
    fn identifier_case_is_preserved() {
        assert_ne!(
            fingerprint_hash("SELECT * FROM Users"),
            fingerprint_hash("SELECT * FROM users")
        );
    }

    #[test]
    fn keyword_case_is_folded() {
        assert_eq!(
            fingerprint_hash("select * from users where id = 1"),
            fingerprint_hash("SELECT * FROM users WHERE id = 1")
        );
    }

    #[test]
    fn quoted_identifiers_merge_with_plain() {
        assert_eq!(
            fingerprint_hash("SELECT * FROM `users`"),
            fingerprint_hash("SELECT * FROM users")
        );
        assert_eq!(
            fingerprint_hash("SELECT * FROM \"users\""),
            fingerprint_hash("SELECT * FROM users")
        );
    }

    #[test]
    fn consecutive_literals_collapse() {
        assert_eq!(
            fingerprint_hash("INSERT INTO t VALUES (1, 2, 3)"),
            fingerprint_hash("INSERT INTO t VALUES (?, ?, ?)")
        );
    }

    #[test]
    fn comments_and_whitespace_are_dropped() {
        assert_eq!(
            fingerprint_hash("SELECT 1 -- trailing\nFROM t"),
            fingerprint_hash("SELECT ? FROM t")
        );
    }

    #[test]
    fn strings_collapse() {
        assert_eq!(
            fingerprint_hash("SELECT * FROM t WHERE name = 'bob'"),
            fingerprint_hash("SELECT * FROM t WHERE name = 'alice'")
        );
    }

    #[test]
    fn null_true_false_are_literals() {
        assert_eq!(
            fingerprint_hash("SELECT * FROM t WHERE a = NULL"),
            fingerprint_hash("SELECT * FROM t WHERE a = 1")
        );
    }

    #[test]
    fn stable_across_calls() {
        let q = "SELECT a, b FROM orders WHERE total > 100 AND status = 'open'";
        assert_eq!(fingerprint_hash(q), fingerprint_hash(q));
    }
}
