//! Split a SQL script into individual statements.
//!
//! A tiny scanner, not a parser: it understands single-, double-, and
//! backtick-quoted strings/identifiers, line/block comments, and PostgreSQL
//! dollar-quoted blocks — enough to split on top-level semicolons. The real
//! grammar work (highlighting, context analysis) belongs to a tree-sitter
//! layer, as in TablePro's `SQLFileParser` + `TableProGrammars` split.

/// Split `sql` into statements, dropping empty statements and comments.
pub fn split_statements(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = sql.chars().collect();
    let mut i = 0;
    let mut saw_content = false;

    while i < chars.len() {
        let c = chars[i];

        // Removing a comment must not accidentally join tokens on either side.
        if c == '-' && i + 1 < chars.len() && chars[i + 1] == '-' {
            push_comment_boundary(&mut current, &mut saw_content);
            i += 2;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            if i < chars.len() {
                push_whitespace(&mut current);
                i += 1;
            }
            continue;
        }

        // PostgreSQL permits nested block comments. Supporting them is also
        // harmless for engines that stop at the first closing delimiter.
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            push_comment_boundary(&mut current, &mut saw_content);
            i += 2;
            let mut depth = 1usize;
            while i < chars.len() && depth > 0 {
                if chars[i] == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*'
                    && i + 1 < chars.len()
                    && chars[i + 1] == '/'
                {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }

        // MySQL accepts backslash escapes in quoted text by default. Doubled
        // quote delimiters are accepted by the SQL engines as well.
        if matches!(c, '\'' | '"' | '`') {
            saw_content = true;
            current.push(c);
            i += 1;
            while i < chars.len() {
                let quoted = chars[i];
                current.push(quoted);
                if quoted == '\\' {
                    if i + 1 < chars.len() {
                        current.push(chars[i + 1]);
                        i += 2;
                    } else {
                        i += 1;
                    }
                    continue;
                }
                if quoted == c {
                    if i + 1 < chars.len() && chars[i + 1] == c {
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

        // PostgreSQL dollar-quoted string: $tag$ ... $tag$.
        if c == '$' {
            if let Some((tag, consumed)) = scan_dollar_tag(&chars[i..]) {
                saw_content = true;
                current.extend(&chars[i..i + consumed]);
                let mut j = i + consumed;
                while j < chars.len() {
                    current.push(chars[j]);
                    if j + 1 >= tag.len() && chars[j + 1 - tag.len()..j + 1] == tag[..] {
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
            current.clear();
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

fn push_comment_boundary(current: &mut String, saw_content: &mut bool) {
    if *saw_content {
        push_whitespace(current);
    }
}

fn push_whitespace(current: &mut String) {
    if !current.chars().last().is_some_and(char::is_whitespace) {
        current.push(' ');
    }
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
    fn ignores_comments_without_joining_surrounding_tokens() {
        let sql = "SELECT/* outer /* nested; */ still outer */1; -- hello;\nSELECT 2";
        assert_eq!(split_statements(sql), vec!["SELECT 1", "SELECT 2"]);
    }

    #[test]
    fn handles_dollar_quotes() {
        let sql = "SELECT $$a;b$$; SELECT 2;";
        assert_eq!(split_statements(sql), vec!["SELECT $$a;b$$", "SELECT 2"]);
    }

    #[test]
    fn keeps_semicolons_inside_backtick_identifiers() {
        let sql = "SELECT `odd;name` FROM `table`; SELECT 2";
        assert_eq!(
            split_statements(sql),
            vec!["SELECT `odd;name` FROM `table`", "SELECT 2"]
        );
    }

    #[test]
    fn handles_mysql_backslash_escapes() {
        // MySQL: \' is an escaped quote (string continues); \\ is a literal
        // backslash after which the next quote really does close the string.
        // Backtick identifiers honor backslash escapes the same way, so
        // `a\`;b` is one identifier containing a semicolon.
        let sql = r#"SELECT 'it\';s fine', `a\`;b`; SELECT 2"#;
        assert_eq!(
            split_statements(sql),
            vec![r#"SELECT 'it\';s fine', `a\`;b`"#, "SELECT 2"]
        );
    }

    #[test]
    fn doubled_backslash_still_lets_the_string_close() {
        // A literal escaped backslash does NOT protect the following quote.
        let sql = r#"SELECT 'it\\'; SELECT 2"#;
        assert_eq!(split_statements(sql), vec![r"SELECT 'it\\'", "SELECT 2"]);
    }
}
