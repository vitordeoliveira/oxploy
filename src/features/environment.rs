//! A service's environment variables, written like a `.env` file: one `KEY=value` per
//! line. oxploy passes them to cargo while it builds and to the binary when it runs, and
//! the app decides which ones it reads.

use std::collections::HashMap;
use std::fmt;

/// The most bytes a service's variables may take, as written.
pub const MAX_LEN: usize = 64 * 1024;

/// The variable oxploy sets itself, to the port Caddy sends the app's traffic to.
const RESERVED: &str = "PORT";

/// One variable.
#[derive(Debug, PartialEq, Eq)]
pub struct Variable {
    pub key: String,
    pub value: String,
}

/// Why variables could not be read, and on which line.
#[derive(Debug, PartialEq, Eq)]
pub struct EnvironmentError {
    /// Counted from 1, or 0 for a problem with the whole text.
    pub line: usize,
    pub problem: String,
}

impl fmt::Display for EnvironmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            0 => f.write_str(&self.problem),
            line => write!(f, "Line {line}: {}", self.problem),
        }
    }
}

/// Reads variables written like a `.env` file. Blank lines and lines starting with `#`
/// are skipped, a leading `export ` is allowed, and quotes around a value are removed.
pub fn parse(text: &str) -> Result<Vec<Variable>, EnvironmentError> {
    if text.len() > MAX_LEN {
        return Err(EnvironmentError {
            line: 0,
            problem: format!("The variables take more than {} KiB", MAX_LEN / 1024),
        });
    }
    let mut variables = Vec::new();
    let mut lines_by_key = HashMap::new();
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let problem = |problem: String| EnvironmentError {
            line: number,
            problem,
        };
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            return Err(problem("Write it as KEY=value".to_owned()));
        };
        let key = key.trim();
        if !is_key(key) {
            return Err(problem(format!(
                "{key:?} is not a variable name: use letters, digits and _, not starting with a digit"
            )));
        }
        if key == RESERVED {
            return Err(problem(
                "oxploy sets PORT itself, to the port Caddy sends traffic to".to_owned(),
            ));
        }
        if let Some(first) = lines_by_key.insert(key.to_owned(), number) {
            return Err(problem(format!("{key} is already set on line {first}")));
        }
        variables.push(Variable {
            key: key.to_owned(),
            value: unquote(value.trim()).to_owned(),
        });
    }
    Ok(variables)
}

/// Returns whether `key` can name an environment variable.
fn is_key(key: &str) -> bool {
    let mut chars = key.chars();
    key.len() <= 128
        && chars
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Removes one pair of matching single or double quotes around a value.
fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(text: &str) -> Vec<(String, String)> {
        parse(text)
            .unwrap()
            .into_iter()
            .map(|variable| (variable.key, variable.value))
            .collect()
    }

    #[test]
    fn reads_a_dotenv_file() {
        let text = "# Database\nDATABASE_URL=postgres://db/shop?ssl=true\n\nexport RUST_LOG = info\nGREETING=\"hello world\"\nEMPTY=\n";
        assert_eq!(
            pairs(text),
            [
                ("DATABASE_URL".to_owned(), "postgres://db/shop?ssl=true".to_owned()),
                ("RUST_LOG".to_owned(), "info".to_owned()),
                ("GREETING".to_owned(), "hello world".to_owned()),
                ("EMPTY".to_owned(), String::new()),
            ]
        );
    }

    #[test]
    fn names_the_line_with_a_problem() {
        let error = |text| parse(text).unwrap_err().to_string();
        assert_eq!(error("A=1\njust text"), "Line 2: Write it as KEY=value");
        assert!(error("1A=x").starts_with("Line 1: \"1A\" is not a variable name"));
        assert_eq!(error("A=1\n\nA=2"), "Line 3: A is already set on line 1");
        assert!(error("PORT=8080").contains("oxploy sets PORT itself"));
    }

    #[test]
    fn keeps_quotes_that_do_not_match() {
        assert_eq!(pairs("A='it\"s"), [("A".to_owned(), "'it\"s".to_owned())]);
    }
}
