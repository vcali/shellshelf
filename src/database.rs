use crate::keywords::extract_keywords;
use crate::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(crate) struct StoredCommand {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    pub(crate) command: String,
    #[serde(default)]
    pub(crate) description: Option<String>,
    pub(crate) keywords: Vec<String>,
}

impl StoredCommand {
    #[cfg(test)]
    pub(crate) fn new(command: String, description: Option<String>) -> Self {
        Self::with_name(command, description, None)
    }

    pub(crate) fn with_name(
        command: String,
        description: Option<String>,
        name: Option<String>,
    ) -> Self {
        let mut keywords = extract_keywords(&command);

        if let Some(description) = description.as_deref() {
            for keyword in extract_keywords(description) {
                if !keywords.contains(&keyword) {
                    keywords.push(keyword);
                }
            }
            keywords.sort();
        }

        Self {
            name,
            command,
            description,
            keywords,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AddCommandOutcome {
    Added,
    NamedExisting,
    Unchanged,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub(crate) struct CommandDatabase {
    pub(crate) commands: Vec<StoredCommand>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SaveCommandOutcome {
    Added,
    Updated,
    Duplicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct MergeDatabaseOutcome {
    pub(crate) duplicate_commands_removed: usize,
    pub(crate) descriptions_upgraded: usize,
}

impl CommandDatabase {
    pub(crate) fn new() -> Self {
        Self {
            commands: Vec::new(),
        }
    }

    pub(crate) fn load_from_file(path: &Path) -> Result<Self> {
        if path.exists() {
            let content = fs::read_to_string(path)?;
            let db: CommandDatabase = serde_json::from_str(&content)?;
            db.validate_names()?;
            Ok(db)
        } else {
            Ok(Self::new())
        }
    }

    pub(crate) fn save_to_file(&self, path: &Path) -> Result<()> {
        self.validate_names()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        fs::write(path, content)?;
        Ok(())
    }

    pub(crate) fn add_command(&mut self, command: String, description: Option<String>) -> bool {
        matches!(
            self.add_named_command(command, description, None),
            Ok(AddCommandOutcome::Added)
        )
    }

    pub(crate) fn add_named_command(
        &mut self,
        command: String,
        description: Option<String>,
        name: Option<String>,
    ) -> Result<AddCommandOutcome> {
        let name = normalize_name(name)?;
        if name.is_some() {
            crate::template::parameters(&command)?;
        }

        if let Some(existing_index) = self
            .commands
            .iter()
            .position(|existing| existing.command == command)
        {
            let existing_name = self.commands[existing_index].name.clone();
            return match (existing_name, name) {
                (None, Some(name)) => {
                    self.ensure_name_available(&name, Some(existing_index))?;
                    self.commands[existing_index].name = Some(name);
                    Ok(AddCommandOutcome::NamedExisting)
                }
                (Some(existing), Some(requested)) if existing.eq_ignore_ascii_case(&requested) => {
                    Ok(AddCommandOutcome::Unchanged)
                }
                (Some(existing), Some(requested)) => Err(format!(
                    "Command already has name '{existing}' and cannot also be named '{requested}'."
                )
                .into()),
                (_, None) => Ok(AddCommandOutcome::Unchanged),
            };
        }

        if let Some(name) = name.as_deref() {
            self.ensure_name_available(name, None)?;
        }
        self.commands
            .push(StoredCommand::with_name(command, description, name));
        Ok(AddCommandOutcome::Added)
    }

    pub(crate) fn save_command(
        &mut self,
        original_command: Option<&str>,
        command: String,
        description: Option<String>,
        name: Option<String>,
    ) -> Result<SaveCommandOutcome> {
        let requested_name = normalize_name(name)?;
        if let Some(original_command) = original_command {
            if let Some(index) = self
                .commands
                .iter()
                .position(|existing| existing.command == original_command)
            {
                let collides =
                    self.commands
                        .iter()
                        .enumerate()
                        .any(|(existing_index, existing)| {
                            existing_index != index && existing.command == command
                        });

                if collides {
                    return Ok(SaveCommandOutcome::Duplicate);
                }

                let name = requested_name.or_else(|| self.commands[index].name.clone());
                if let Some(name) = name.as_deref() {
                    self.ensure_name_available(name, Some(index))?;
                    crate::template::parameters(&command)?;
                }
                self.commands[index] = StoredCommand::with_name(command, description, name);
                return Ok(SaveCommandOutcome::Updated);
            }
        }

        Ok(
            match self.add_named_command(command, description, requested_name)? {
                AddCommandOutcome::Added => SaveCommandOutcome::Added,
                AddCommandOutcome::NamedExisting => SaveCommandOutcome::Updated,
                AddCommandOutcome::Unchanged => SaveCommandOutcome::Duplicate,
            },
        )
    }

    pub(crate) fn find_by_name(&self, name: &str) -> Option<&StoredCommand> {
        self.commands.iter().find(|command| {
            command
                .name
                .as_deref()
                .is_some_and(|stored| stored.eq_ignore_ascii_case(name))
        })
    }

    fn ensure_name_available(&self, name: &str, except_index: Option<usize>) -> Result<()> {
        if self.commands.iter().enumerate().any(|(index, command)| {
            Some(index) != except_index
                && command
                    .name
                    .as_deref()
                    .is_some_and(|stored| stored.eq_ignore_ascii_case(name))
        }) {
            return Err(format!("Command name '{name}' already exists in this shelf.").into());
        }
        Ok(())
    }

    fn validate_names(&self) -> Result<()> {
        for (index, command) in self.commands.iter().enumerate() {
            if let Some(name) = command.name.as_deref() {
                validate_command_name(name)?;
                crate::template::parameters(&command.command)?;
                self.ensure_name_available(name, Some(index))?;
            }
        }
        Ok(())
    }

    pub(crate) fn merged_with(&self, other: &Self) -> Result<(Self, MergeDatabaseOutcome)> {
        let mut merged = Self::new();
        let mut outcome = MergeDatabaseOutcome::default();

        for command in &self.commands {
            merge_command_into(&mut merged, command, &mut outcome)?;
        }

        for command in &other.commands {
            merge_command_into(&mut merged, command, &mut outcome)?;
        }

        merged.validate_names()?;
        Ok((merged, outcome))
    }

    pub(crate) fn search_in_shelf(&self, keywords: &[String], shelf: &str) -> Vec<&StoredCommand> {
        self.search_with_shelf_context(keywords, Some(shelf))
    }

    fn search_with_shelf_context(
        &self,
        keywords: &[String],
        shelf: Option<&str>,
    ) -> Vec<&StoredCommand> {
        let normalized_keywords: Vec<String> = keywords
            .iter()
            .map(|keyword| keyword.to_lowercase())
            .collect();
        let shelf_lower = shelf.map(str::to_lowercase);
        let shelf_keywords = shelf.map(extract_keywords).unwrap_or_default();

        self.commands
            .iter()
            .filter(|cmd| {
                let command_lower = cmd.command.to_lowercase();
                let description_lower = cmd.description.as_ref().map(|value| value.to_lowercase());
                let name_lower = cmd.name.as_ref().map(|value| value.to_lowercase());

                normalized_keywords.iter().all(|keyword| {
                    cmd.keywords.iter().any(|stored| stored.contains(keyword))
                        || command_lower.contains(keyword)
                        || description_lower
                            .as_ref()
                            .is_some_and(|description| description.contains(keyword))
                        || name_lower
                            .as_ref()
                            .is_some_and(|name| name.contains(keyword))
                        || shelf_lower
                            .as_ref()
                            .is_some_and(|shelf_name| shelf_name.contains(keyword))
                        || shelf_keywords.iter().any(|stored| stored.contains(keyword))
                })
            })
            .collect()
    }
}

fn merge_command_into(
    merged: &mut CommandDatabase,
    candidate: &StoredCommand,
    outcome: &mut MergeDatabaseOutcome,
) -> Result<()> {
    if let Some(existing) = merged
        .commands
        .iter_mut()
        .find(|existing| existing.command == candidate.command)
    {
        outcome.duplicate_commands_removed += 1;
        let merged_description = merge_descriptions(
            existing.description.as_deref(),
            candidate.description.as_deref(),
        );
        let merged_name = existing.name.clone().or_else(|| candidate.name.clone());
        let description_changed = existing.description != merged_description;
        let name_changed = existing.name != merged_name;
        if description_changed {
            outcome.descriptions_upgraded += 1;
        }
        if description_changed || name_changed {
            *existing =
                StoredCommand::with_name(existing.command.clone(), merged_description, merged_name);
        }
    } else {
        if let Some(name) = candidate.name.as_deref() {
            merged.ensure_name_available(name, None)?;
        }
        merged.commands.push(candidate.clone());
    }
    Ok(())
}

fn normalize_name(name: Option<String>) -> Result<Option<String>> {
    let name = name
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty());
    if let Some(name) = name.as_deref() {
        validate_command_name(name)?;
    }
    Ok(name)
}

pub(crate) fn validate_command_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 80
        && name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
        && name.chars().all(|ch| {
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_' | '.')
        });

    if valid {
        Ok(())
    } else {
        Err("Command names must be at most 80 characters and contain only lowercase letters, numbers, dots, underscores, and hyphens, starting with a letter or number.".into())
    }
}

fn merge_descriptions(primary: Option<&str>, secondary: Option<&str>) -> Option<String> {
    let normalize = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|trimmed| !trimmed.is_empty())
            .map(str::to_string)
    };

    match (normalize(primary), normalize(secondary)) {
        (None, None) => None,
        (Some(description), None) | (None, Some(description)) => Some(description),
        (Some(primary), Some(secondary)) if primary == secondary => Some(primary),
        (Some(primary), Some(secondary)) if secondary.len() > primary.len() => Some(secondary),
        (Some(primary), Some(_secondary)) => Some(primary),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AddCommandOutcome, CommandDatabase, MergeDatabaseOutcome, SaveCommandOutcome, StoredCommand,
    };
    use tempfile::TempDir;

    #[test]
    fn test_stored_command_new_includes_description_keywords() {
        let command = "curl https://example.com/releases".to_string();
        let stored = StoredCommand::new(command.clone(), Some("Upload release build".to_string()));

        assert_eq!(stored.command, command);
        assert_eq!(stored.description.as_deref(), Some("Upload release build"));
        assert!(stored.keywords.contains(&"upload".to_string()));
        assert!(stored.keywords.contains(&"release".to_string()));
        assert!(stored.keywords.contains(&"build".to_string()));
        assert!(stored.keywords.contains(&"example".to_string()));
    }

    #[test]
    fn test_command_database_new() {
        let db = CommandDatabase::new();
        assert!(db.commands.is_empty());
    }

    #[test]
    fn test_command_database_add_command() {
        let mut db = CommandDatabase::new();
        let command = "git log --oneline -20".to_string();

        db.add_command(command.clone(), Some("Recent history".to_string()));
        assert_eq!(db.commands.len(), 1);
        assert_eq!(db.commands[0].command, command);
        assert_eq!(
            db.commands[0].description.as_deref(),
            Some("Recent history")
        );
    }

    #[test]
    fn test_command_database_add_duplicate_command() {
        let mut db = CommandDatabase::new();
        let command = "git status".to_string();

        db.add_command(command.clone(), Some("First description".to_string()));
        db.add_command(command, Some("Second description".to_string()));

        assert_eq!(db.commands.len(), 1);
        assert_eq!(
            db.commands[0].description.as_deref(),
            Some("First description")
        );
    }

    #[test]
    fn named_add_enriches_legacy_duplicate_and_enforces_unique_names() {
        let mut db = CommandDatabase::new();
        db.add_command("git status".to_string(), Some("Status".to_string()));

        assert_eq!(
            db.add_named_command(
                "git status".to_string(),
                Some("Ignored".to_string()),
                Some("status".to_string()),
            )
            .unwrap(),
            AddCommandOutcome::NamedExisting
        );
        assert_eq!(db.commands[0].name.as_deref(), Some("status"));
        assert_eq!(db.commands[0].description.as_deref(), Some("Status"));
        assert!(db
            .add_named_command("git diff".to_string(), None, Some("STATUS".to_string()),)
            .is_err());
    }

    #[test]
    fn unnamed_commands_serialize_without_name_field() {
        let json = serde_json::to_value(CommandDatabase {
            commands: vec![StoredCommand::new("git status".to_string(), None)],
        })
        .unwrap();

        assert!(json["commands"][0].get("name").is_none());
    }

    #[test]
    fn test_command_database_save_command_updates_existing_entry() {
        let mut db = CommandDatabase::new();
        db.add_command(
            "curl https://example.com/old".to_string(),
            Some("Old".to_string()),
        );

        let outcome = db
            .save_command(
                Some("curl https://example.com/old"),
                "curl https://example.com/new".to_string(),
                Some("Updated".to_string()),
                None,
            )
            .unwrap();

        assert_eq!(outcome, SaveCommandOutcome::Updated);
        assert_eq!(db.commands.len(), 1);
        assert_eq!(db.commands[0].command, "curl https://example.com/new");
        assert_eq!(db.commands[0].description.as_deref(), Some("Updated"));
    }

    #[test]
    fn test_command_database_save_command_rejects_collision_when_updating() {
        let mut db = CommandDatabase::new();
        db.add_command("curl https://example.com/one".to_string(), None);
        db.add_command("curl https://example.com/two".to_string(), None);

        let outcome = db
            .save_command(
                Some("curl https://example.com/one"),
                "curl https://example.com/two".to_string(),
                None,
                None,
            )
            .unwrap();

        assert_eq!(outcome, SaveCommandOutcome::Duplicate);
        assert_eq!(db.commands.len(), 2);
    }

    #[test]
    fn test_command_database_search() {
        let mut db = CommandDatabase::new();

        db.add_command("git log --oneline --graph".to_string(), None);
        db.add_command("aws s3 ls s3://example-bucket".to_string(), None);
        db.add_command(
            "curl -X POST https://api.github.com/repos".to_string(),
            Some("Create repository".to_string()),
        );

        assert_eq!(
            db.search_in_shelf(&["graph".to_string()], "default").len(),
            1
        );
        assert_eq!(
            db.search_in_shelf(&["bucket".to_string()], "default").len(),
            1
        );
        assert_eq!(
            db.search_in_shelf(&["github".to_string()], "default").len(),
            1
        );
        assert_eq!(
            db.search_in_shelf(&["repository".to_string()], "default")
                .len(),
            1
        );
        assert_eq!(
            db.search_in_shelf(&["nonexistent".to_string()], "default")
                .len(),
            0
        );
    }

    #[test]
    fn test_command_database_search_case_insensitive() {
        let mut db = CommandDatabase::new();
        db.add_command(
            "AWS S3 LS s3://Example-Bucket".to_string(),
            Some("List artifacts".to_string()),
        );

        assert_eq!(db.search_in_shelf(&["aws".to_string()], "default").len(), 1);
        assert_eq!(
            db.search_in_shelf(&["bucket".to_string()], "default").len(),
            1
        );
        assert_eq!(
            db.search_in_shelf(&["ARTIFACTS".to_string()], "default")
                .len(),
            1
        );
    }

    #[test]
    fn test_command_database_search_matches_shelf_name_context() {
        let mut db = CommandDatabase::new();
        db.add_command("curl https://example.com/upload".to_string(), None);

        assert_eq!(
            db.search_in_shelf(&["media".to_string(), "upload".to_string()], "media")
                .len(),
            1
        );
        assert_eq!(db.search_in_shelf(&["media".to_string()], "media").len(), 1);
        assert_eq!(
            db.search_in_shelf(&["payments".to_string(), "upload".to_string()], "media")
                .len(),
            0
        );
    }

    #[test]
    fn test_command_database_search_matches_separator_heavy_shelf_names() {
        let mut db = CommandDatabase::new();
        db.add_command("curl https://example.com/health".to_string(), None);

        assert_eq!(
            db.search_in_shelf(&["media".to_string()], "media-tools")
                .len(),
            1
        );
        assert_eq!(
            db.search_in_shelf(&["tools".to_string()], "media_tools")
                .len(),
            1
        );
        assert_eq!(
            db.search_in_shelf(&["api".to_string()], "media.api").len(),
            1
        );
    }

    #[test]
    fn test_command_database_save_and_load() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test_commands.json");

        let mut db = CommandDatabase::new();
        db.add_command("git log --oneline".to_string(), Some("Example".to_string()));
        db.add_command("aws sts get-caller-identity".to_string(), None);

        db.save_to_file(&file_path).unwrap();
        assert!(file_path.exists());

        let loaded_db = CommandDatabase::load_from_file(&file_path).unwrap();
        assert_eq!(loaded_db.commands.len(), 2);
        assert_eq!(loaded_db, db);
    }

    #[test]
    fn test_command_database_load_nonexistent_file() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("nonexistent.json");

        let db = CommandDatabase::load_from_file(&file_path).unwrap();
        assert!(db.commands.is_empty());
    }

    #[test]
    fn test_command_database_loads_entries_without_description() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("legacy_commands.json");
        std::fs::write(
            &file_path,
            r#"{
  "commands": [
    {
      "command": "git status",
      "keywords": ["git", "status"]
    }
  ]
}"#,
        )
        .unwrap();

        let db = CommandDatabase::load_from_file(&file_path).unwrap();

        assert_eq!(db.commands.len(), 1);
        assert_eq!(db.commands[0].description, None);
    }

    #[test]
    fn test_search_partial_keyword_match() {
        let mut db = CommandDatabase::new();
        db.add_command("curl https://api.github.com/repositories".to_string(), None);

        assert_eq!(
            db.search_in_shelf(&["repo".to_string()], "default").len(),
            1
        );
        assert_eq!(db.search_in_shelf(&["hub".to_string()], "default").len(), 1);
    }

    #[test]
    fn test_command_database_merged_with_deduplicates_and_upgrades_description() {
        let local = CommandDatabase {
            commands: vec![
                StoredCommand::new(
                    "curl https://example.com".to_string(),
                    Some("Short".to_string()),
                ),
                StoredCommand::new("git status".to_string(), None),
            ],
        };
        let remote = CommandDatabase {
            commands: vec![
                StoredCommand::new(
                    "curl https://example.com".to_string(),
                    Some("Longer curl description".to_string()),
                ),
                StoredCommand::new("aws s3 ls".to_string(), Some("List buckets".to_string())),
            ],
        };

        let (merged, outcome) = local.merged_with(&remote).unwrap();

        assert_eq!(
            merged.commands,
            vec![
                StoredCommand::new(
                    "curl https://example.com".to_string(),
                    Some("Longer curl description".to_string()),
                ),
                StoredCommand::new("git status".to_string(), None),
                StoredCommand::new("aws s3 ls".to_string(), Some("List buckets".to_string())),
            ]
        );
        assert_eq!(
            outcome,
            MergeDatabaseOutcome {
                duplicate_commands_removed: 1,
                descriptions_upgraded: 1,
            }
        );
    }

    #[test]
    fn test_command_database_merged_with_prefers_first_description_when_lengths_match() {
        let local = CommandDatabase {
            commands: vec![StoredCommand::new(
                "curl https://example.com".to_string(),
                Some("Local text".to_string()),
            )],
        };
        let remote = CommandDatabase {
            commands: vec![StoredCommand::new(
                "curl https://example.com".to_string(),
                Some("Remote txt".to_string()),
            )],
        };

        let (merged, outcome) = local.merged_with(&remote).unwrap();

        assert_eq!(merged.commands.len(), 1);
        assert_eq!(
            merged.commands[0].description.as_deref(),
            Some("Local text")
        );
        assert_eq!(
            outcome,
            MergeDatabaseOutcome {
                duplicate_commands_removed: 1,
                descriptions_upgraded: 0,
            }
        );
    }

    #[test]
    fn merge_rejects_same_name_on_different_commands() {
        let local = CommandDatabase {
            commands: vec![StoredCommand::with_name(
                "git status".to_string(),
                None,
                Some("status".to_string()),
            )],
        };
        let remote = CommandDatabase {
            commands: vec![StoredCommand::with_name(
                "git diff".to_string(),
                None,
                Some("status".to_string()),
            )],
        };

        assert!(local.merged_with(&remote).is_err());
    }
}
