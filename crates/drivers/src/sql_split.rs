//! Split a SQL script into individual statements.
//!
//! A tiny scanner, not a parser: it understands single- and double-quoted
//! strings, line/bracket comments, and PostgreSQL dollar-quoted blocks —
//! enough to split on top-level semicolons. The real grammar work
//! (highlighting, context analysis) belongs to a tree-sitter layer, as in
//! TablePro's `SQLFileParser` + `TableProGrammars` split.

/// Split `sql` into statements, dropping empty statements and comments.
pub fn split_statements(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = sql.chars().collect();
    let mut i = 0;
    let mut saw_content = false;

    while i < chars.len() {
        let c = chars[i];

        // Line comment — skipped entirely
        if c == '-' && i + 1 < chars.len() && chars[i + 1] == '-' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        // Bracket comment — skipped entirely
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            i += 2;
            while i < chars.len() {
                if chars[i] == '*' && i + 1 < chars.len() && chars[i + 1] == '/' {
                    i += 2;
                    break;
                }
                i += 1;
            }
            continue;
        }

        // Single-quoted string (doubled quote is an escape)
        if c == '\'' {
            saw_content = true;
            current.push(c);
            i += 1;
            while i < chars.len() {
                current.push(chars[i]);
                if chars[i] == '\'' {
                    if i + 1 < chars.len() && chars[i + 1] == '\'' {
                        current.push(chars[i + 1]);
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }

        // Double-quoted identifier
        if c == '"' {
            saw_content = true;
            current.push(c);
            i += 1;
            while i < chars.len() {
                current.push(chars[i]);
                if chars[i] == '"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }

        // PostgreSQL dollar-quoted string: $tag$ ... $tag$
        if c == '$' {
            if let Some((tag, consumed)) = scan_dollar_tag(&chars[i..]) {
                saw_content = true;
                current.extend(&chars[i..i + consumed]);
                let mut j = i + consumed;
                while j < chars.len() {
                    current.push(chars[j]);
                    if j + 1 >= tag.len()
                        && chars[j + 1 - tag.len()..j + 1] == tag[..]
                    {
                        j += 1;
                        break;
                    }
                    j += 1;
                }
                i = j;
                continue;
            }
        }

        if c == ';' {
            if saw_content {
                statements.push(current.trim().to_string());
            }
            current = String::new();
            saw_content = false;
            i += 1;
            continue;
        }

        if !c.is_whitespace() {
            saw_content = true;
        }
        current.push(c);
        i += 1;
    }

    if saw_content {
        let trimmed = current.trim();
        if !trimmed.is_empty() {
            statements.push(trimmed.to_string());
        }
    }
    statements
}

/// Scan a `$tag$` opening marker starting at `chars[0]` ('`$`'). Returns the
/// full tag (including dollars) and how many chars it spans.
fn scan_dollar_tag(chars: &[char]) -> Option<(Vec<char>, usize)> {
    if chars.first() != Some(&'$') {
        return None;
    }
    let mut tag = vec!['$'];
    let mut i = 1;
    while i < chars.len() {
        let c = chars[i];
        if c.is_alphanumeric() || c == '_' {
            tag.push(c);
            i += 1;
        } else if c == '$' {
            tag.push('$');
            return Some((tag, i + 1));
        } else {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_top_level_semicolons() {
        let sql = "SELECT 1; SELECT 2;\n";
        assert_eq!(split_statements(sql), vec!["SELECT 1", "SELECT 2"]);
    }

    #[test]
    fn keeps_semicolons_inside_strings() {
        let sql = "SELECT ';' AS a; SELECT 2;";
        assert_eq!(
            split_statements(sql),
            vec!["SELECT ';' AS a", "SELECT 2"]
        );
    }

    #[test]
    fn ignores_comments() {
        let sql = "-- hello;\nSELECT 1 /* x;y */; ";
        assert_eq!(split_statements(sql), vec!["SELECT 1"]);
    }

    #[test]
    fn handles_dollar_quotes() {
        let sql = "SELECT $$a;b$$; SELECT 2;";
        assert_eq!(split_statements(sql), vec!["SELECT $$a;b$$", "SELECT 2"]);
    }
}
