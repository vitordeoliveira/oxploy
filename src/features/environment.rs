//! A service's environment variables, written like a `.env` file: one `KEY=value` per
//! line. oxploy passes them to cargo while it builds and to the binary when it runs, and
//! the app decides which ones it reads.
//!
//! A value can use `${NAME}` for a variable oxploy sets, such as
//! `DATABASE_URL=sqlite:${OXPLOY_STORAGE_DIR}/data.db`, or for one set on an earlier
//! line.

use std::collections::HashMap;
use std::fmt;

/// The most bytes a service's variables may take, as written.
pub const MAX_LEN: usize = 64 * 1024;

/// The variable with the port Caddy sends the app's traffic to.
pub const PORT_VARIABLE: &str = "PORT";

/// The variable with the service's storage folder, which survives deploys.
pub const STORAGE_VARIABLE: &str = "OXPLOY_STORAGE_DIR";

/// The variable with the token oxploy calls the app's MCP server with, set while the
/// app serves one. Values can use it either way: it is empty while the app serves none.
pub const MCP_TOKEN_VARIABLE: &str = "OXPLOY_MCP_TOKEN";

/// The variables oxploy sets for every app, which values can use.
const PROVIDED: [&str; 3] = [PORT_VARIABLE, STORAGE_VARIABLE, MCP_TOKEN_VARIABLE];

/// One variable.
#[derive(Debug, PartialEq, Eq)]
pub struct Variable {
    pub key: String,
    pub value: String,
    /// Whether `${NAME}` in the value is replaced. Single quotes around a value turn it
    /// off, as in a `.env` file.
    pub expands: bool,
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

/// A variable oxploy sets for the app besides [`PORT_VARIABLE`] and
/// [`STORAGE_VARIABLE`], such as the one a database connects through. Lines may use it
/// as `${NAME}` but not set it.
pub struct SetElsewhere<'a> {
    pub name: &'a str,
    /// What sets it, to end "NAME is set by …".
    pub by: String,
}

/// Reads variables written like a `.env` file. Blank lines and lines starting with `#`
/// are skipped, a leading `export ` is allowed, and quotes around a value are removed.
/// `elsewhere` lists the other variables oxploy sets.
pub fn parse(text: &str, elsewhere: &[SetElsewhere<'_>]) -> Result<Vec<Variable>, EnvironmentError> {
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
        if key == PORT_VARIABLE {
            return Err(problem(
                "oxploy sets PORT itself, to the port Caddy sends traffic to".to_owned(),
            ));
        }
        if key.starts_with("OXPLOY_") {
            return Err(problem(format!(
                "{key} starts with OXPLOY_, which is for the variables oxploy sets"
            )));
        }
        if let Some(set) = elsewhere.iter().find(|set| set.name == key) {
            return Err(problem(format!("{key} is set by {}", set.by)));
        }
        let value = value.trim();
        let expands = !(value.len() >= 2 && value.starts_with('\'') && value.ends_with('\''));
        let value = unquote(value);
        if expands {
            for name in references(value).map_err(problem)? {
                let provided = PROVIDED.contains(&name) || elsewhere.iter().any(|set| set.name == name);
                if !provided && !lines_by_key.contains_key(name) {
                    return Err(problem(format!(
                        "${{{name}}} is not set on an earlier line, and oxploy does not set it"
                    )));
                }
            }
        }
        if let Some(first) = lines_by_key.insert(key.to_owned(), number) {
            return Err(problem(format!("{key} is already set on line {first}")));
        }
        variables.push(Variable {
            key: key.to_owned(),
            value: value.to_owned(),
            expands,
        });
    }
    Ok(variables)
}

/// Replaces `${NAME}` in the values with what oxploy sets, in `provided`, or with an
/// earlier variable, and returns the variables to pass on.
pub fn resolve(variables: Vec<Variable>, provided: &[(&str, String)]) -> Vec<(String, String)> {
    let mut known: HashMap<String, String> = provided
        .iter()
        .map(|(name, value)| ((*name).to_owned(), value.clone()))
        .collect();
    let mut resolved = Vec::with_capacity(variables.len());
    for variable in variables {
        let value = if variable.expands {
            expand(&variable.value, &known)
        } else {
            variable.value
        };
        known.insert(variable.key.clone(), value.clone());
        resolved.push((variable.key, value));
    }
    resolved
}

/// Returns the names a value uses as `${NAME}`, or why it cannot be read.
fn references(value: &str) -> Result<Vec<&str>, String> {
    let mut names = Vec::new();
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let end = after.find('}').ok_or("${ is not closed with }")?;
        let name = &after[..end];
        if !is_key(name) {
            return Err(format!("${{{name}}} is not a variable name"));
        }
        names.push(name);
        rest = &after[end + 1..];
    }
    Ok(names)
}

/// Replaces each `${NAME}` in `value` with its value in `known`.
fn expand(value: &str, known: &HashMap<String, String>) -> String {
    let mut expanded = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        expanded.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            break;
        };
        match known.get(&after[..end]) {
            Some(value) => expanded.push_str(value),
            None => expanded.push_str(&rest[start..start + 2 + end + 1]),
        }
        rest = &after[end + 1..];
    }
    expanded.push_str(rest);
    expanded
}

/// Returns whether `key` can name an environment variable.
pub fn is_key(key: &str) -> bool {
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
        parse(text, &[])
            .unwrap()
            .into_iter()
            .map(|variable| (variable.key, variable.value))
            .collect()
    }

    fn resolved(text: &str) -> Vec<(String, String)> {
        let provided = [
            (PORT_VARIABLE, "10000".to_owned()),
            (STORAGE_VARIABLE, "/srv/shop".to_owned()),
        ];
        resolve(parse(text, &[]).unwrap(), &provided)
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
        let error = |text| parse(text, &[]).unwrap_err().to_string();
        assert_eq!(error("A=1\njust text"), "Line 2: Write it as KEY=value");
        assert!(error("1A=x").starts_with("Line 1: \"1A\" is not a variable name"));
        assert_eq!(error("A=1\n\nA=2"), "Line 3: A is already set on line 1");
        assert!(error("PORT=8080").contains("oxploy sets PORT itself"));
        assert!(error("OXPLOY_STORAGE_DIR=/tmp").contains("starts with OXPLOY_"));
    }

    #[test]
    fn uses_but_does_not_set_what_a_database_sets() {
        let database = [SetElsewhere {
            name: "DATABASE_URL",
            by: "the database data.db in the Storage tab".to_owned(),
        }];
        assert!(parse("BACKUP_URL=${DATABASE_URL}", &database).is_ok());
        assert_eq!(
            parse("DATABASE_URL=sqlite:x.db", &database).unwrap_err().to_string(),
            "Line 1: DATABASE_URL is set by the database data.db in the Storage tab"
        );
    }

    #[test]
    fn uses_what_oxploy_sets_and_earlier_lines() {
        let text = "DATABASE_URL=sqlite:${OXPLOY_STORAGE_DIR}/data.db\nHOST=127.0.0.1\nPUBLIC_URL=http://${HOST}:${PORT}/\nLITERAL='${HOST}'";
        assert_eq!(
            resolved(text),
            [
                ("DATABASE_URL".to_owned(), "sqlite:/srv/shop/data.db".to_owned()),
                ("HOST".to_owned(), "127.0.0.1".to_owned()),
                ("PUBLIC_URL".to_owned(), "http://127.0.0.1:10000/".to_owned()),
                ("LITERAL".to_owned(), "${HOST}".to_owned()),
            ]
        );
    }

    #[test]
    fn passes_the_mcp_token_under_the_name_an_app_reads() {
        let variables = parse("MCP_TOKEN=${OXPLOY_MCP_TOKEN}", &[]).unwrap();
        let on = resolve(variables, &[(MCP_TOKEN_VARIABLE, "3f9a".to_owned())]);
        assert_eq!(on, [("MCP_TOKEN".to_owned(), "3f9a".to_owned())]);

        // Turned off, the app gets an empty token, which serves no one.
        let variables = parse("MCP_TOKEN=${OXPLOY_MCP_TOKEN}", &[]).unwrap();
        let off = resolve(variables, &[(MCP_TOKEN_VARIABLE, String::new())]);
        assert_eq!(off, [("MCP_TOKEN".to_owned(), String::new())]);

        assert!(parse("OXPLOY_MCP_TOKEN=mine", &[]).is_err());
    }

    #[test]
    fn refuses_names_it_cannot_fill() {
        let error = |text| parse(text, &[]).unwrap_err().to_string();
        assert_eq!(
            error("A=${LATER}\nLATER=1"),
            "Line 1: ${LATER} is not set on an earlier line, and oxploy does not set it"
        );
        assert_eq!(error("A=${HOME"), "Line 1: ${ is not closed with }");
        assert_eq!(error("A=${1X}"), "Line 1: ${1X} is not a variable name");
        assert_eq!(pairs("PRICE=$5"), [("PRICE".to_owned(), "$5".to_owned())]);
    }

    #[test]
    fn keeps_quotes_that_do_not_match() {
        assert_eq!(pairs("A='it\"s"), [("A".to_owned(), "'it\"s".to_owned())]);
    }
}
