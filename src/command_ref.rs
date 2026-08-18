use crate::config::{validate_shelf_name, validate_team_name};
use crate::database::validate_command_name;
use crate::Result;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommandRef {
    Local {
        shelf: String,
        name: String,
    },
    Shared {
        team: String,
        shelf: String,
        name: String,
    },
}

impl CommandRef {
    pub(crate) fn parse(value: &str) -> Result<Self> {
        let parts: Vec<&str> = value.split('/').collect();
        let reference = match parts.as_slice() {
            ["local", shelf, name] => {
                validate_shelf_name(shelf)?;
                validate_command_name(name)?;
                Self::Local {
                    shelf: (*shelf).to_string(),
                    name: (*name).to_string(),
                }
            }
            ["shared", team, shelf, name] => {
                validate_team_name(team)?;
                validate_shelf_name(shelf)?;
                validate_command_name(name)?;
                Self::Shared {
                    team: (*team).to_string(),
                    shelf: (*shelf).to_string(),
                    name: (*name).to_string(),
                }
            }
            _ => {
                return Err(format!(
                    "Invalid command reference '{value}'. Use local/<shelf>/<name> or shared/<team>/<shelf>/<name>."
                )
                .into())
            }
        };
        Ok(reference)
    }

    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Local { name, .. } | Self::Shared { name, .. } => name,
        }
    }
}

impl fmt::Display for CommandRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local { shelf, name } => write!(formatter, "local/{shelf}/{name}"),
            Self::Shared { team, shelf, name } => write!(formatter, "shared/{team}/{shelf}/{name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CommandRef;

    #[test]
    fn parses_and_formats_local_and_shared_references() {
        for value in ["local/media/find-gif", "shared/search/runbooks/reindex"] {
            assert_eq!(CommandRef::parse(value).unwrap().to_string(), value);
        }
    }

    #[test]
    fn rejects_ambiguous_references() {
        assert!(CommandRef::parse("media/find-gif").is_err());
        assert!(CommandRef::parse("local/media/FindGif").is_err());
        assert!(CommandRef::parse("shared/team/shelf/name/extra").is_err());
    }
}
