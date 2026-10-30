//! Minimal parser for Valve's text KeyValues format (.acf / .vdf files).

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Obj(Vec<(String, Value)>),
}

impl Value {
    /// Case-insensitive child lookup (Steam isn't consistent about key casing).
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(items) => items
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v),
            Value::Str(_) => None,
        }
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            Value::Str(s) => Some(s),
            Value::Obj(_) => None,
        }
    }

    pub fn entries(&self) -> &[(String, Value)] {
        match self {
            Value::Obj(items) => items,
            Value::Str(_) => &[],
        }
    }
}

#[derive(Debug, PartialEq)]
enum Token {
    Str(String),
    Open,
    Close,
}

fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '{' => {
                chars.next();
                tokens.push(Token::Open);
            }
            '}' => {
                chars.next();
                tokens.push(Token::Close);
            }
            '/' => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    // Line comment
                    for c in chars.by_ref() {
                        if c == '\n' {
                            break;
                        }
                    }
                } else {
                    return Err("unexpected '/'".into());
                }
            }
            '[' => {
                // Platform conditional like [$WIN32]; ignore it.
                for c in chars.by_ref() {
                    if c == ']' {
                        break;
                    }
                }
            }
            '"' => {
                chars.next();
                let mut s = String::new();
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some('\\') => s.push('\\'),
                            Some('"') => s.push('"'),
                            Some(other) => {
                                s.push('\\');
                                s.push(other);
                            }
                            None => return Err("unterminated string".into()),
                        },
                        Some(c) => s.push(c),
                        None => return Err("unterminated string".into()),
                    }
                }
                tokens.push(Token::Str(s));
            }
            _ => {
                // Unquoted token
                let mut s = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() || c == '{' || c == '}' || c == '"' {
                        break;
                    }
                    s.push(c);
                    chars.next();
                }
                tokens.push(Token::Str(s));
            }
        }
    }
    Ok(tokens)
}

fn parse_obj(tokens: &mut std::iter::Peekable<std::vec::IntoIter<Token>>, top: bool) -> Result<Vec<(String, Value)>, String> {
    let mut items = Vec::new();
    loop {
        match tokens.next() {
            None if top => return Ok(items),
            None => return Err("unexpected end of file".into()),
            Some(Token::Close) if !top => return Ok(items),
            Some(Token::Close) => return Err("unexpected '}'".into()),
            Some(Token::Open) => return Err("unexpected '{'".into()),
            Some(Token::Str(key)) => match tokens.next() {
                Some(Token::Str(v)) => items.push((key, Value::Str(v))),
                Some(Token::Open) => {
                    let child = parse_obj(tokens, false)?;
                    items.push((key, Value::Obj(child)));
                }
                _ => return Err(format!("missing value for key '{key}'")),
            },
        }
    }
}

/// Parses a document; the returned object holds the top-level keys (e.g. "AppState").
pub fn parse(input: &str) -> Result<Value, String> {
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    let mut tokens = tokenize(input)?.into_iter().peekable();
    Ok(Value::Obj(parse_obj(&mut tokens, true)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_manifest() {
        let doc = parse(
            r#""AppState"
{
	"appid"		"1091500"
	"name"		"Cyberpunk \"2077\""
	// comment
	"InstalledDepots"
	{
		"1091501"
		{
			"manifest"		"3524634124112875626"
		}
	}
}"#,
        )
        .unwrap();
        let app = doc.get("appstate").unwrap();
        assert_eq!(app.str("AppID"), Some("1091500"));
        assert_eq!(app.str("name"), Some("Cyberpunk \"2077\""));
        let depots = app.get("InstalledDepots").unwrap().entries();
        assert_eq!(depots[0].0, "1091501");
        assert_eq!(depots[0].1.str("manifest"), Some("3524634124112875626"));
    }

    #[test]
    fn rejects_truncated() {
        assert!(parse("\"a\" { \"b\" \"c\"").is_err());
    }
}
