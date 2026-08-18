use crate::browse::local_shelves_root;
use crate::cli::build_cli;
use crate::command_ref::CommandRef;
use crate::config::{
    force_sync_personal_storage, force_sync_shared_storage, get_local_data_file_path,
    get_team_data_file_path, list_all_team_shelves, list_local_shelves, list_team_shelves,
    load_all_team_commands, load_team_commands, personal_repository_required_message,
    resolve_active_shelf, resolve_config, resolve_config_path, resolve_data_file_path,
    resolve_personal_storage_context, resolve_shared_storage_context_with_options,
    shared_repository_required_message, write_config, DefaultSharedReadTarget,
    GithubPersonalRepoConfig, GithubSharedRepoConfig, PersonalRepoConfig, PersonalStorageContext,
    SharedRepoConfig, SharedStorageContext, ShellshelfConfig,
    DEFAULT_PERSONAL_REPO_SYNC_CHECK_INTERVAL_MINUTES,
};
use crate::database::{AddCommandOutcome, CommandDatabase, StoredCommand};
use crate::github::{
    complete_background_github_repo_sync, normalize_github_repo_input,
    DEFAULT_GITHUB_REPO_AUTO_UPDATE_INTERVAL_MINUTES,
};
use crate::personal_repo::{
    bootstrap_personal_repo, personal_repo_sync_warning, sync_all_personal_shelves,
    sync_personal_local_shelf, PersonalRepoBootstrapMode, PersonalRepoBootstrapOutcome,
    PersonalRepoSyncWarning,
};
use crate::postman_import::{import_postman_collection, PostmanImportOutcome};
use crate::shared_repo_publish::{
    prepare_publish_branch, publish_pull_request, restore_managed_checkout_to_base_branch,
    sanitize_branch_component, PreparedPublishBranch, PublishPullRequestPlan,
};
use crate::web::run_web_server;
use crate::Result;
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const DEFAULT_LIST_LIMIT: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
enum OutputSectionSource {
    Local { shelf: String },
    SharedTeam { team: String, shelf: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OutputSection {
    source: OutputSectionSource,
    entries: Vec<OutputEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OutputEntry {
    name: Option<String>,
    command: String,
    description: Option<String>,
    template: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SharedReadTarget {
    Team(String),
    AllTeams,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DefaultReadPlan {
    include_local: bool,
    shared_target: Option<SharedReadTarget>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct OutputSummary {
    hidden_local_duplicates: usize,
    hidden_due_to_limit: usize,
    active_limit: Option<usize>,
    search_limit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SharedPublishContext {
    prepared_branch: PreparedPublishBranch,
    plan: PublishPullRequestPlan,
    repo_root: PathBuf,
    restore_managed_checkout: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShelfSection {
    title: String,
    shelves: Vec<String>,
}

impl OutputSection {
    fn local(shelf: impl Into<String>, entries: Vec<OutputEntry>) -> Self {
        Self {
            source: OutputSectionSource::Local {
                shelf: shelf.into(),
            },
            entries,
        }
    }

    fn shared_team(
        team: impl Into<String>,
        shelf: impl Into<String>,
        entries: Vec<OutputEntry>,
    ) -> Self {
        Self {
            source: OutputSectionSource::SharedTeam {
                team: team.into(),
                shelf: shelf.into(),
            },
            entries,
        }
    }

    fn title(&self) -> String {
        match &self.source {
            OutputSectionSource::Local { shelf } => format!("Local / {shelf}"),
            OutputSectionSource::SharedTeam { team, shelf } => {
                format!("Shared / {team} / {shelf}")
            }
        }
    }

    fn is_shared(&self) -> bool {
        matches!(self.source, OutputSectionSource::SharedTeam { .. })
    }
}

impl OutputEntry {
    fn from_command(command: &StoredCommand) -> Self {
        Self {
            name: command.name.clone(),
            command: command.command.clone(),
            description: command.description.clone(),
            template: None,
        }
    }

    fn from_owned_command(command: StoredCommand) -> Self {
        Self {
            name: command.name,
            command: command.command,
            description: command.description,
            template: None,
        }
    }
}

#[derive(Debug, Serialize)]
struct JsonResults {
    schema_version: u8,
    results: Vec<JsonCommandResult>,
    summary: JsonSummary,
}

#[derive(Debug, Serialize)]
struct JsonCommandResult {
    #[serde(rename = "ref")]
    command_ref: Option<String>,
    name: Option<String>,
    description: Option<String>,
    command: String,
    parameters: Vec<String>,
    source: JsonCommandSource,
    shelf: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    template: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum JsonCommandSource {
    Local,
    Shared { team: String },
}

#[derive(Debug, Serialize)]
struct JsonSummary {
    hidden_duplicates: usize,
    hidden_by_limit: usize,
}

#[derive(Debug, Serialize)]
struct JsonSingleResult {
    schema_version: u8,
    result: JsonCommandResult,
}

#[derive(Debug, Serialize)]
struct JsonShelfResults<'a> {
    schema_version: u8,
    results: Vec<JsonShelfResult<'a>>,
}

#[derive(Debug, Serialize)]
struct JsonShelfResult<'a> {
    source: &'a str,
    team: Option<&'a str>,
    shelf: &'a str,
}

#[derive(Debug, Serialize)]
struct JsonAddResult {
    schema_version: u8,
    operation: &'static str,
    status: &'static str,
    result: JsonCommandResult,
    pull_request_url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PublishOutcome {
    pull_request_url: Option<String>,
}

impl From<DefaultSharedReadTarget> for SharedReadTarget {
    fn from(value: DefaultSharedReadTarget) -> Self {
        match value {
            DefaultSharedReadTarget::Team(team) => Self::Team(team),
            DefaultSharedReadTarget::AllTeams => Self::AllTeams,
        }
    }
}

pub fn run() -> Result<()> {
    if std::env::args_os().len() == 1 {
        let mut cmd = build_cli();
        cmd.print_help()?;
        println!();
        return Ok(());
    }

    let matches = build_cli().get_matches();
    if let Some(github_repo) = matches.get_one::<String>("background-sync-repo") {
        let checkout_path = matches
            .get_one::<String>("background-sync-checkout")
            .ok_or("Internal background sync requires a checkout path.")?;
        return complete_background_github_repo_sync(github_repo, Path::new(checkout_path));
    }

    let config_path = resolve_config_path(&matches);
    validate_matches(&matches)?;
    let config = resolve_config(&matches)?;
    let add_repo = matches.get_one::<String>("add-repo");
    let add_personal_repo = matches.get_one::<String>("add-personal-repo");
    let personal_repo_bootstrap = matches
        .get_one::<String>("personal-repo-bootstrap")
        .map(String::as_str)
        .unwrap_or("auto");
    let add_command = matches.get_one::<String>("add");
    let force_sync = matches.get_flag("force-sync");
    let force_sync_personal = matches.get_flag("force-sync-personal");
    let import_postman_path = matches.get_one::<String>("import-postman");
    let list_commands = matches.get_flag("list");
    let json_output = matches.get_flag("json");
    let sync_personal = matches.get_flag("sync-personal");
    let search_keywords: Option<Vec<String>> = matches
        .get_many::<String>("keywords")
        .map(|keywords| keywords.cloned().collect());

    if let Some(repo_input) = add_repo {
        return configure_shared_repo(&config_path, &config, repo_input);
    }
    if let Some(repo_input) = add_personal_repo {
        return configure_personal_repo(
            &config_path,
            &config,
            repo_input,
            parse_personal_repo_bootstrap_mode(personal_repo_bootstrap)?,
        );
    }

    if force_sync {
        let shared_context = resolve_shared_storage_context_with_options(&matches, &config, true)?;
        return run_force_sync(shared_context.as_ref());
    }
    if force_sync_personal {
        let personal_context = resolve_personal_storage_context(&config, true)?;
        return run_personal_force_sync(personal_context.as_ref());
    }
    if sync_personal {
        let personal_context = resolve_personal_storage_context(&config, true)?;
        return run_personal_sync(personal_context.as_ref());
    }

    if let Some(reference) = matches
        .get_one::<String>("get")
        .or_else(|| matches.get_one::<String>("render"))
    {
        let command_ref = CommandRef::parse(reference)?;
        let shared_context = if matches!(&command_ref, CommandRef::Shared { .. }) {
            resolve_shared_storage_context_with_options(&matches, &config, false)?
        } else {
            None
        };
        return get_or_render_command(
            &matches,
            shared_context.as_ref(),
            command_ref,
            matches.get_one::<String>("render").is_some(),
        );
    }

    let all_teams = matches.get_flag("all-teams");
    let shared_context = resolve_shared_storage_context_with_options(&matches, &config, false)?;
    let personal_context = resolve_personal_storage_context(&config, false)?;

    emit_personal_repo_sync_warning(personal_context.as_ref());

    if matches.get_flag("web") {
        return run_web_server(
            shared_context,
            personal_context,
            matches
                .get_one::<u16>("web-port")
                .copied()
                .or(config.web.port),
            config.web.theme.clone(),
        );
    }

    let list_shelves = matches.get_flag("list-shelves");
    let needs_resolved_shelf = !list_shelves
        && import_postman_path.is_none()
        && (matches.get_one::<String>("create-shelf").is_some()
            || add_command.is_some()
            || list_commands
            || matches.get_one::<String>("shelf").is_some());
    let shelf = if list_shelves {
        None
    } else if needs_resolved_shelf {
        Some(resolve_target_shelf(&matches, &config)?)
    } else {
        None
    };
    let data_file = if let Some(shelf) = shelf.as_deref() {
        Some(resolve_data_file_path(
            &matches,
            shared_context.as_ref(),
            shelf,
        )?)
    } else {
        None
    };

    if list_shelves {
        return list_shelves_for_scope(&matches, &config, shared_context.as_ref(), json_output);
    }

    if matches.get_one::<String>("create-shelf").is_some() {
        let shelf = shelf
            .as_deref()
            .expect("shelf should be resolved for shelf creation");
        let data_file = data_file
            .as_deref()
            .expect("data file should be resolved for shelf creation");
        let publish_context = resolve_shared_publish_context(
            &matches,
            shared_context.as_ref(),
            shelf,
            "create shelf",
        )?;
        let personal_context_for_write = if matches.get_one::<String>("team").is_none() {
            personal_context.as_ref()
        } else {
            None
        };
        return run_write_operation(
            publish_context,
            personal_context_for_write,
            data_file,
            shelf,
            false,
            || create_shelf(&matches, data_file, shelf),
        )
        .map(|_| ());
    }

    if let Some(import_path) = import_postman_path {
        let import_outcome = load_postman_import(&matches, Path::new(import_path))?;
        let import_shelf = import_outcome.shelf_name.clone();
        let publish_context = resolve_shared_publish_context(
            &matches,
            shared_context.as_ref(),
            &import_shelf,
            "import a Postman collection",
        )?;
        let personal_context_for_write = if matches.get_one::<String>("team").is_none() {
            personal_context.as_ref()
        } else {
            None
        };
        return run_write_operation(
            publish_context,
            personal_context_for_write,
            &get_local_data_file_path(&import_shelf)?,
            &import_shelf,
            false,
            || Ok(save_postman_import(&matches, shared_context.as_ref(), import_outcome)?.changed),
        )
        .map(|_| ());
    }

    if let Some(command) = add_command {
        let shelf = shelf
            .as_deref()
            .expect("shelf should be resolved for add operations");
        let data_file = data_file
            .as_deref()
            .expect("data file should be resolved for add operations");
        let publish_context = resolve_shared_publish_context(
            &matches,
            shared_context.as_ref(),
            shelf,
            "add a command",
        )?;
        let personal_context_for_write = if matches.get_one::<String>("team").is_none() {
            personal_context.as_ref()
        } else {
            None
        };
        let description = matches
            .get_one::<String>("description")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let name = matches.get_one::<String>("name");
        let mut add_outcome = AddCommandOutcome::Unchanged;
        let publish_outcome = run_write_operation(
            publish_context,
            personal_context_for_write,
            data_file,
            shelf,
            json_output,
            || {
                let mut db = CommandDatabase::load_from_file(data_file)?;
                add_outcome =
                    db.add_named_command(command.clone(), description.clone(), name.cloned())?;
                let changed = add_outcome != AddCommandOutcome::Unchanged;
                if changed {
                    db.save_to_file(data_file)?;
                }
                Ok(changed)
            },
        )?;

        if json_output {
            let database = CommandDatabase::load_from_file(data_file)?;
            let entry = OutputEntry::from_command(match name {
                Some(name) => database
                    .find_by_name(name)
                    .expect("successful named add persisted the command"),
                None => database
                    .commands
                    .iter()
                    .find(|stored| stored.command == *command)
                    .expect("successful add persisted the command"),
            });
            let section = match matches.get_one::<String>("team") {
                Some(team) => OutputSection::shared_team(team, shelf, vec![entry]),
                None => OutputSection::local(shelf, vec![entry]),
            };
            let result = sections_to_json_results(std::slice::from_ref(&section))?
                .into_iter()
                .next()
                .expect("add result has one command");
            println!(
                "{}",
                serde_json::to_string(&JsonAddResult {
                    schema_version: 1,
                    operation: "add",
                    status: add_status(add_outcome),
                    result,
                    pull_request_url: publish_outcome.pull_request_url,
                })?
            );
        } else {
            match (add_outcome, name) {
                (AddCommandOutcome::Added, None) => match description {
                    Some(description) => {
                        println!("Added command to shelf '{shelf}': {command} ({description})")
                    }
                    None => println!("Added command to shelf '{shelf}': {command}"),
                },
                (AddCommandOutcome::Added, Some(name)) => match description {
                    Some(description) => println!(
                        "Added command '{name}' to shelf '{shelf}': {command} ({description})"
                    ),
                    None => println!("Added command '{name}' to shelf '{shelf}': {command}"),
                },
                (AddCommandOutcome::NamedExisting, Some(name)) => {
                    println!("Named existing command '{name}' in shelf '{shelf}'.")
                }
                (AddCommandOutcome::Unchanged, Some(name)) => {
                    println!("Command '{name}' already exists in shelf '{shelf}'.")
                }
                (AddCommandOutcome::NamedExisting, None) => unreachable!("naming needs a name"),
                (AddCommandOutcome::Unchanged, None) => {
                    println!("Command already exists in shelf '{shelf}'.")
                }
            }
        }
        return Ok(());
    }

    if list_commands {
        let shelf = shelf
            .as_deref()
            .expect("shelf should be resolved for list operations");
        let data_file = data_file
            .as_deref()
            .expect("data file should be resolved for list operations");
        let limit = resolve_list_limit(&matches, &config);

        if let Some(team) = matches.get_one::<String>("team") {
            let commands = CommandDatabase::load_from_file(data_file)?;
            let mut sections = vec![OutputSection::shared_team(
                team.clone(),
                shelf.to_string(),
                filter_commands(&commands, shelf, search_keywords.as_deref()),
            )];
            let summary = OutputSummary {
                hidden_due_to_limit: apply_list_limit(&mut sections, limit),
                active_limit: limit,
                ..OutputSummary::default()
            };
            emit_sections(
                &sections,
                &empty_message(search_keywords.is_some(), Some(shelf)),
                &summary,
                json_output,
            )?;
            return Ok(());
        }

        if all_teams {
            let mut sections = load_shared_sections_for_target(
                shared_context
                    .as_ref()
                    .ok_or(shared_repository_required_message())?,
                &SharedReadTarget::AllTeams,
                shelf,
                search_keywords.as_deref(),
            )?;
            let summary = OutputSummary {
                hidden_due_to_limit: apply_list_limit(&mut sections, limit),
                active_limit: limit,
                ..OutputSummary::default()
            };
            emit_sections(
                &sections,
                &empty_message(search_keywords.is_some(), Some(shelf)),
                &summary,
                json_output,
            )?;
            return Ok(());
        }

        let local_db = CommandDatabase::load_from_file(data_file)?;
        let plan = resolve_default_read_plan(&matches, &config, shared_context.as_ref())?;
        let (mut sections, hidden_local_duplicates) = load_default_read_sections(
            &local_db,
            shared_context.as_ref(),
            shelf,
            search_keywords.as_deref(),
            &plan,
        )?;
        let mut summary = OutputSummary {
            hidden_local_duplicates,
            hidden_due_to_limit: 0,
            active_limit: limit,
            search_limit: false,
        };
        summary.hidden_due_to_limit = apply_list_limit(&mut sections, limit);
        emit_sections(
            &sections,
            &empty_message(search_keywords.is_some(), Some(shelf)),
            &summary,
            json_output,
        )?;
        return Ok(());
    }

    if let Some(keyword_vec) = search_keywords.as_deref() {
        let limit = matches
            .get_one::<usize>("limit")
            .copied()
            .and_then(normalize_limit);
        if let Some(shelf) = shelf.as_deref() {
            if let Some(team) = matches.get_one::<String>("team") {
                let data_file = data_file
                    .as_deref()
                    .expect("data file should be resolved for team shelf search");
                let commands = CommandDatabase::load_from_file(data_file)?;
                let mut sections = vec![OutputSection::shared_team(
                    team.clone(),
                    shelf.to_string(),
                    filter_commands(&commands, shelf, Some(keyword_vec)),
                )];
                let summary = OutputSummary {
                    hidden_due_to_limit: apply_list_limit(&mut sections, limit),
                    active_limit: limit,
                    search_limit: true,
                    ..OutputSummary::default()
                };
                emit_sections(
                    &sections,
                    &empty_message(true, Some(shelf)),
                    &summary,
                    json_output,
                )?;
                return Ok(());
            }

            if all_teams {
                let mut sections = load_shared_sections_for_target(
                    shared_context
                        .as_ref()
                        .ok_or(shared_repository_required_message())?,
                    &SharedReadTarget::AllTeams,
                    shelf,
                    Some(keyword_vec),
                )?;
                let summary = OutputSummary {
                    hidden_due_to_limit: apply_list_limit(&mut sections, limit),
                    active_limit: limit,
                    search_limit: true,
                    ..OutputSummary::default()
                };
                emit_sections(
                    &sections,
                    &empty_message(true, Some(shelf)),
                    &summary,
                    json_output,
                )?;
                return Ok(());
            }

            let data_file = data_file
                .as_deref()
                .expect("data file should be resolved for single-shelf search");
            let local_db = CommandDatabase::load_from_file(data_file)?;
            let plan = resolve_default_read_plan(&matches, &config, shared_context.as_ref())?;
            let (mut sections, hidden_local_duplicates) = load_default_read_sections(
                &local_db,
                shared_context.as_ref(),
                shelf,
                Some(keyword_vec),
                &plan,
            )?;
            let summary = OutputSummary {
                hidden_local_duplicates,
                hidden_due_to_limit: apply_list_limit(&mut sections, limit),
                active_limit: limit,
                search_limit: true,
            };
            emit_sections(
                &sections,
                &empty_message(true, Some(shelf)),
                &summary,
                json_output,
            )?;
            return Ok(());
        }

        let (mut sections, hidden_local_duplicates) = load_search_sections_without_active_shelf(
            &matches,
            &config,
            shared_context.as_ref(),
            keyword_vec,
        )?;
        let summary = OutputSummary {
            hidden_local_duplicates,
            hidden_due_to_limit: apply_list_limit(&mut sections, limit),
            active_limit: limit,
            search_limit: true,
        };
        emit_sections(&sections, &empty_message(true, None), &summary, json_output)?;
    }

    Ok(())
}

fn configure_shared_repo(
    config_path: &Path,
    config: &ShellshelfConfig,
    repo_input: &str,
) -> Result<()> {
    let github_repo = normalize_github_repo_input(repo_input)?;
    let (teams_dir, default_team, default_all_teams, auto_update_repo, auto_update_interval) =
        match config.shared_repo.as_ref() {
            Some(SharedRepoConfig::Path(existing)) => (
                existing.teams_dir.clone(),
                existing.default_team.clone(),
                existing.default_all_teams,
                true,
                DEFAULT_GITHUB_REPO_AUTO_UPDATE_INTERVAL_MINUTES,
            ),
            Some(SharedRepoConfig::Github(existing)) => (
                existing.teams_dir.clone(),
                existing.default_team.clone(),
                existing.default_all_teams,
                existing.auto_update_repo,
                existing.auto_update_interval_minutes,
            ),
            None => (
                None,
                None,
                false,
                true,
                DEFAULT_GITHUB_REPO_AUTO_UPDATE_INTERVAL_MINUTES,
            ),
        };

    let mut updated = config.clone();
    updated.shared_repo = Some(SharedRepoConfig::Github(GithubSharedRepoConfig {
        github_repo: github_repo.clone(),
        teams_dir,
        auto_update_repo,
        auto_update_interval_minutes: auto_update_interval,
        default_team,
        default_all_teams,
    }));
    write_config(config_path, &updated)?;

    println!(
        "Configured shared GitHub repository '{github_repo}' in {}.",
        config_path.display()
    );
    Ok(())
}

fn configure_personal_repo(
    config_path: &Path,
    config: &ShellshelfConfig,
    repo_input: &str,
    bootstrap_mode: PersonalRepoBootstrapMode,
) -> Result<()> {
    let github_repo = normalize_github_repo_input(repo_input)?;
    let (auto_update_repo, auto_update_interval, sync_check_interval) =
        match config.personal_repo.as_ref() {
            Some(PersonalRepoConfig::Github(existing)) => (
                existing.auto_update_repo,
                existing.auto_update_interval_minutes,
                existing.sync_check_interval_minutes,
            ),
            Some(PersonalRepoConfig::Path(_)) | None => (
                true,
                DEFAULT_GITHUB_REPO_AUTO_UPDATE_INTERVAL_MINUTES,
                DEFAULT_PERSONAL_REPO_SYNC_CHECK_INTERVAL_MINUTES,
            ),
        };

    let mut updated = config.clone();
    updated.personal_repo = Some(PersonalRepoConfig::Github(GithubPersonalRepoConfig {
        github_repo: github_repo.clone(),
        auto_update_repo,
        auto_update_interval_minutes: auto_update_interval,
        sync_check_interval_minutes: sync_check_interval,
    }));
    write_config(config_path, &updated)?;

    println!(
        "Configured personal GitHub repository '{github_repo}' in {}.",
        config_path.display()
    );

    let Some(personal_context) = resolve_personal_storage_context(&updated, false)? else {
        return Ok(());
    };
    match bootstrap_personal_repo(&personal_context, &local_shelves_root(), bootstrap_mode)? {
        PersonalRepoBootstrapOutcome::Skipped => {}
        PersonalRepoBootstrapOutcome::Merged => {
            println!("Merged local shelves with the personal repository.");
        }
        PersonalRepoBootstrapOutcome::Pushed => {
            println!("Seeded the personal repository from local shelves.");
        }
        PersonalRepoBootstrapOutcome::Pulled => {
            println!("Imported shelves from the personal repository into local shelves.");
        }
        PersonalRepoBootstrapOutcome::AlreadyInSync => {
            println!("Local shelves already matched the personal repository.");
        }
    }

    Ok(())
}

fn parse_personal_repo_bootstrap_mode(value: &str) -> Result<PersonalRepoBootstrapMode> {
    match value {
        "auto" => Ok(PersonalRepoBootstrapMode::Auto),
        "merge" => Ok(PersonalRepoBootstrapMode::Merge),
        "push" => Ok(PersonalRepoBootstrapMode::Push),
        "pull" => Ok(PersonalRepoBootstrapMode::Pull),
        "skip" => Ok(PersonalRepoBootstrapMode::Skip),
        _ => Err(
            "personal repo bootstrap mode must be one of: auto, merge, push, pull, skip.".into(),
        ),
    }
}

fn emit_personal_repo_sync_warning(personal_context: Option<&PersonalStorageContext>) {
    let Some(personal_context) = personal_context else {
        return;
    };

    match personal_repo_sync_warning(personal_context) {
        Ok(Some(PersonalRepoSyncWarning::Ahead {
            local_commits,
            push_command,
            inspect_command,
        })) => {
            eprintln!(
                "Warning: the managed personal repository checkout is ahead of origin by {local_commits} commit(s).\nPush: {push_command}\nInspect: {inspect_command}"
            );
        }
        Ok(Some(PersonalRepoSyncWarning::Behind {
            remote_commits,
            pull_command,
            inspect_command,
        })) => {
            eprintln!(
                "Warning: the managed personal repository checkout is behind origin by {remote_commits} commit(s).\nPull: {pull_command}\nInspect: {inspect_command}"
            );
        }
        Ok(Some(PersonalRepoSyncWarning::Diverged {
            local_commits,
            remote_commits,
            pull_command,
            push_command,
            inspect_command,
        })) => {
            eprintln!(
                "Warning: the managed personal repository checkout and origin have diverged ({local_commits} local-only commit(s), {remote_commits} remote-only commit(s)).\nPull: {pull_command}\nPush: {push_command}\nInspect: {inspect_command}"
            );
        }
        Ok(None) => {}
        Err(error) => {
            eprintln!("Warning: failed to inspect personal repository sync status: {error}");
        }
    }
}

fn run_force_sync(shared_context: Option<&SharedStorageContext>) -> Result<()> {
    let shared_context = shared_context.ok_or(shared_repository_required_message())?;
    if !force_sync_shared_storage(shared_context)? {
        return Err(
            "Force sync requires a managed GitHub shared repository configured through shared_repo.mode = 'github'."
                .into(),
        );
    }

    let github_repo = shared_context
        .managed_github_repo
        .as_deref()
        .expect("managed github repo should exist after a successful force sync");
    println!("Force-synced managed shared repository '{github_repo}'.");
    Ok(())
}

fn run_personal_force_sync(personal_context: Option<&PersonalStorageContext>) -> Result<()> {
    let personal_context = personal_context.ok_or(personal_repository_required_message())?;
    if !force_sync_personal_storage(personal_context)? {
        return Err(
            "Force sync requires a managed GitHub personal repository configured through personal_repo.github_repo."
                .into(),
        );
    }

    let github_repo = personal_context
        .managed_github_repo
        .as_deref()
        .expect("managed github repo should exist after a successful personal force sync");
    println!("Force-synced managed personal repository '{github_repo}'.");
    Ok(())
}

fn run_personal_sync(personal_context: Option<&PersonalStorageContext>) -> Result<()> {
    let personal_context = personal_context.ok_or(personal_repository_required_message())?;
    let changed = sync_all_personal_shelves(personal_context, &local_shelves_root())?;
    if changed == 0 {
        println!("Local shelves already match the configured personal repository.");
    } else {
        println!("Synchronized local shelves to the configured personal repository.");
    }
    Ok(())
}

fn resolve_shared_publish_context(
    matches: &clap::ArgMatches,
    shared_context: Option<&SharedStorageContext>,
    shelf: &str,
    action_description: &str,
) -> Result<Option<SharedPublishContext>> {
    if !matches.get_flag("open-pr") {
        return Ok(None);
    }

    let team = matches
        .get_one::<String>("team")
        .expect("--open-pr validation should require --team");
    let shared_context = shared_context.ok_or(shared_repository_required_message())?;
    let repo_root = shared_context.repository_root.clone();
    let data_file = resolve_data_file_path(matches, Some(shared_context), shelf)?;
    let default_branch = format!(
        "shellshelf/{}-{}",
        sanitize_branch_component(team),
        sanitize_branch_component(shelf)
    );
    let prepared_branch = prepare_publish_branch(
        &repo_root,
        matches.get_one::<String>("base-branch").map(String::as_str),
        matches.get_one::<String>("pr-branch").map(String::as_str),
        &default_branch,
    )?;

    let shelf_label = format!("{team}/{shelf}");
    let commit_message = match action_description {
        "create shelf" => format!("Add {shelf_label} shelf"),
        "import a Postman collection" => format!("Import {shelf_label} shelf"),
        _ => format!("Update {shelf_label} shelf"),
    };
    let pr_title = commit_message.clone();
    let pr_body =
        format!("## Summary\n- {action_description} in the shared shelf `{shelf_label}`\n");

    Ok(Some(SharedPublishContext {
        prepared_branch,
        plan: PublishPullRequestPlan {
            commit_message,
            pr_title,
            pr_body,
            changed_paths: vec![data_file],
        },
        repo_root,
        restore_managed_checkout: shared_context.managed_github_repo.is_some(),
    }))
}

fn run_write_operation<F>(
    publish_context: Option<SharedPublishContext>,
    personal_context: Option<&PersonalStorageContext>,
    local_data_file: &Path,
    shelf: &str,
    quiet: bool,
    operation: F,
) -> Result<PublishOutcome>
where
    F: FnOnce() -> Result<bool>,
{
    let result = operation().and_then(|changed| {
        let publish_outcome = publish_shared_changes(publish_context.clone(), changed, quiet)?;
        publish_personal_changes(personal_context, local_data_file, shelf, changed, quiet)?;
        Ok(publish_outcome)
    });
    let cleanup_result = cleanup_shared_publish_context(publish_context.as_ref());

    match (result, cleanup_result) {
        (Ok(outcome), Ok(())) => Ok(outcome),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup_error)) => Err(cleanup_error),
        (Err(error), Err(cleanup_error)) => {
            Err(format!("{error} Cleanup also failed: {cleanup_error}").into())
        }
    }
}

fn publish_personal_changes(
    personal_context: Option<&PersonalStorageContext>,
    local_data_file: &Path,
    shelf: &str,
    changed: bool,
    quiet: bool,
) -> Result<()> {
    let Some(personal_context) = personal_context else {
        return Ok(());
    };

    if !changed {
        return Ok(());
    }

    let synchronized = sync_personal_local_shelf(personal_context, local_data_file, shelf)?;
    if !quiet {
        if synchronized {
            println!("Updated personal sync for local shelf '{shelf}'.");
        } else {
            println!("Local shelf '{shelf}' already matched the personal repository.");
        }
    }

    Ok(())
}

fn cleanup_shared_publish_context(publish_context: Option<&SharedPublishContext>) -> Result<()> {
    let Some(publish_context) = publish_context else {
        return Ok(());
    };

    if !publish_context.restore_managed_checkout {
        return Ok(());
    }

    restore_managed_checkout_to_base_branch(
        &publish_context.repo_root,
        &publish_context.prepared_branch.base_branch,
    )
}

fn publish_shared_changes(
    publish_context: Option<SharedPublishContext>,
    changed: bool,
    quiet: bool,
) -> Result<PublishOutcome> {
    let Some(publish_context) = publish_context else {
        return Ok(PublishOutcome::default());
    };

    if !changed {
        if !quiet {
            println!("No shared changes were published.");
        }
        return Ok(PublishOutcome::default());
    }

    let pull_request_url = publish_pull_request(
        &publish_context.repo_root,
        &publish_context.prepared_branch,
        &publish_context.plan,
    )?;
    if !quiet {
        if let Some(pr_url) = pull_request_url.as_deref() {
            println!("Opened pull request: {pr_url}");
        } else {
            println!("No shared changes were published.");
        }
    }

    Ok(PublishOutcome { pull_request_url })
}

fn add_status(outcome: AddCommandOutcome) -> &'static str {
    match outcome {
        AddCommandOutcome::Added => "added",
        AddCommandOutcome::NamedExisting => "named_existing",
        AddCommandOutcome::Unchanged => "unchanged",
    }
}

fn get_or_render_command(
    matches: &clap::ArgMatches,
    shared_context: Option<&SharedStorageContext>,
    command_ref: CommandRef,
    render: bool,
) -> Result<()> {
    let data_file = match &command_ref {
        CommandRef::Local { shelf, .. } => get_local_data_file_path(shelf)?,
        CommandRef::Shared { team, shelf, .. } => {
            let context = shared_context.ok_or(shared_repository_required_message())?;
            get_team_data_file_path(&context.repository_root, &context.teams_dir, team, shelf)?
        }
    };
    let database = CommandDatabase::load_from_file(&data_file)?;
    let stored = database
        .find_by_name(command_ref.name())
        .ok_or_else(|| format!("Named command '{command_ref}' was not found."))?;

    let mut entry = OutputEntry::from_command(stored);
    if render {
        let arguments = crate::template::parse_arguments(
            matches
                .get_many::<String>("arg")
                .into_iter()
                .flatten()
                .map(String::as_str),
        )?;
        entry.template = Some(entry.command.clone());
        entry.command = crate::template::render(&entry.command, &arguments)?;
    }

    if matches.get_flag("raw") {
        println!("{}", entry.command);
        return Ok(());
    }

    let section = match &command_ref {
        CommandRef::Local { shelf, .. } => OutputSection::local(shelf, vec![entry]),
        CommandRef::Shared { team, shelf, .. } => {
            OutputSection::shared_team(team, shelf, vec![entry])
        }
    };
    if matches.get_flag("json") {
        let result = sections_to_json_results(std::slice::from_ref(&section))?
            .into_iter()
            .next()
            .expect("exact lookup has one command");
        println!(
            "{}",
            serde_json::to_string(&JsonSingleResult {
                schema_version: 1,
                result,
            })?
        );
        return Ok(());
    }
    emit_sections(
        &[section],
        "Named command was not found.",
        &OutputSummary::default(),
        false,
    )
}

fn validate_matches(matches: &clap::ArgMatches) -> Result<()> {
    let add_repo = matches.get_one::<String>("add-repo");
    let add_personal_repo = matches.get_one::<String>("add-personal-repo");
    let all_teams = matches.get_flag("all-teams");
    let force_sync = matches.get_flag("force-sync");
    let force_sync_personal = matches.get_flag("force-sync-personal");
    let local_only = matches.get_flag("local-only");
    let shared_only = matches.get_flag("shared-only");
    let import_postman = matches.get_one::<String>("import-postman");
    let open_pr = matches.get_flag("open-pr");
    let personal_repo_bootstrap = matches.get_one::<String>("personal-repo-bootstrap");
    let sync_personal = matches.get_flag("sync-personal");
    let web_mode = matches.get_flag("web");
    let has_keywords = matches
        .get_many::<String>("keywords")
        .map(|values| values.len() > 0)
        .unwrap_or(false);
    validate_agent_flags(matches, has_keywords)?;

    if add_repo.is_some()
        && (web_mode
            || matches.get_one::<u16>("web-port").is_some()
            || matches.get_one::<String>("add").is_some()
            || matches.get_one::<String>("description").is_some()
            || import_postman.is_some()
            || matches.get_one::<String>("target-shelf").is_some()
            || matches.get_one::<String>("shelf").is_some()
            || matches.get_one::<String>("create-shelf").is_some()
            || matches.get_flag("list")
            || matches.get_flag("list-shelves")
            || matches.get_one::<usize>("limit").is_some()
            || matches.get_one::<String>("repo").is_some()
            || matches.get_one::<String>("teams-dir").is_some()
            || matches.get_one::<String>("team").is_some()
            || open_pr
            || matches.get_one::<String>("base-branch").is_some()
            || matches.get_one::<String>("pr-branch").is_some()
            || all_teams
            || local_only
            || shared_only
            || force_sync_personal
            || sync_personal
            || add_personal_repo.is_some()
            || personal_repo_bootstrap.is_some()
            || has_keywords)
    {
        return Err("--add-repo must be used on its own.".into());
    }

    if add_personal_repo.is_some()
        && (web_mode
            || matches.get_one::<u16>("web-port").is_some()
            || matches.get_one::<String>("add").is_some()
            || matches.get_one::<String>("description").is_some()
            || import_postman.is_some()
            || matches.get_one::<String>("target-shelf").is_some()
            || matches.get_one::<String>("shelf").is_some()
            || matches.get_one::<String>("create-shelf").is_some()
            || matches.get_flag("list")
            || matches.get_flag("list-shelves")
            || matches.get_one::<usize>("limit").is_some()
            || matches.get_one::<String>("repo").is_some()
            || matches.get_one::<String>("teams-dir").is_some()
            || matches.get_one::<String>("team").is_some()
            || open_pr
            || matches.get_one::<String>("base-branch").is_some()
            || matches.get_one::<String>("pr-branch").is_some()
            || all_teams
            || local_only
            || shared_only
            || force_sync
            || force_sync_personal
            || sync_personal
            || has_keywords)
    {
        return Err("--add-personal-repo must be used on its own.".into());
    }

    if personal_repo_bootstrap.is_some() && add_personal_repo.is_none() {
        return Err("--personal-repo-bootstrap can only be used with --add-personal-repo.".into());
    }

    if matches.get_one::<u16>("web-port").is_some() && !web_mode {
        return Err("--web-port can only be used with --web.".into());
    }

    if force_sync
        && (matches.get_one::<String>("add").is_some()
            || matches.get_one::<String>("description").is_some()
            || matches.get_one::<String>("import-postman").is_some()
            || matches.get_one::<String>("target-shelf").is_some()
            || matches.get_one::<String>("shelf").is_some()
            || matches.get_one::<String>("create-shelf").is_some()
            || matches.get_flag("list")
            || matches.get_flag("list-shelves")
            || matches.get_one::<String>("repo").is_some()
            || add_repo.is_some()
            || add_personal_repo.is_some()
            || matches.get_one::<String>("teams-dir").is_some()
            || matches.get_one::<String>("team").is_some()
            || matches.get_flag("open-pr")
            || matches.get_one::<String>("base-branch").is_some()
            || matches.get_one::<String>("pr-branch").is_some()
            || matches.get_flag("all-teams")
            || matches.get_flag("local-only")
            || matches.get_flag("shared-only")
            || force_sync_personal
            || sync_personal
            || matches.get_one::<usize>("limit").is_some()
            || matches.get_flag("web")
            || has_keywords)
    {
        return Err("--force-sync must be used on its own.".into());
    }

    if force_sync_personal
        && (matches.get_one::<String>("add").is_some()
            || matches.get_one::<String>("description").is_some()
            || matches.get_one::<String>("import-postman").is_some()
            || matches.get_one::<String>("target-shelf").is_some()
            || matches.get_one::<String>("shelf").is_some()
            || matches.get_one::<String>("create-shelf").is_some()
            || matches.get_flag("list")
            || matches.get_flag("list-shelves")
            || matches.get_one::<String>("repo").is_some()
            || add_repo.is_some()
            || add_personal_repo.is_some()
            || matches.get_one::<String>("teams-dir").is_some()
            || matches.get_one::<String>("team").is_some()
            || matches.get_flag("open-pr")
            || matches.get_one::<String>("base-branch").is_some()
            || matches.get_one::<String>("pr-branch").is_some()
            || matches.get_flag("all-teams")
            || matches.get_flag("local-only")
            || matches.get_flag("shared-only")
            || force_sync
            || sync_personal
            || matches.get_one::<usize>("limit").is_some()
            || matches.get_flag("web")
            || has_keywords)
    {
        return Err("--force-sync-personal must be used on its own.".into());
    }

    if sync_personal
        && (matches.get_one::<String>("add").is_some()
            || matches.get_one::<String>("description").is_some()
            || matches.get_one::<String>("import-postman").is_some()
            || matches.get_one::<String>("target-shelf").is_some()
            || matches.get_one::<String>("shelf").is_some()
            || matches.get_one::<String>("create-shelf").is_some()
            || matches.get_flag("list")
            || matches.get_flag("list-shelves")
            || matches.get_one::<String>("repo").is_some()
            || add_repo.is_some()
            || add_personal_repo.is_some()
            || matches.get_one::<String>("teams-dir").is_some()
            || matches.get_one::<String>("team").is_some()
            || matches.get_flag("open-pr")
            || matches.get_one::<String>("base-branch").is_some()
            || matches.get_one::<String>("pr-branch").is_some()
            || matches.get_flag("all-teams")
            || matches.get_flag("local-only")
            || matches.get_flag("shared-only")
            || force_sync
            || force_sync_personal
            || matches.get_one::<usize>("limit").is_some()
            || matches.get_flag("web")
            || has_keywords)
    {
        return Err("--sync-personal must be used on its own.".into());
    }

    if matches.get_one::<String>("base-branch").is_some() && !open_pr {
        return Err("--base-branch can only be used with --open-pr.".into());
    }

    if matches.get_one::<String>("pr-branch").is_some() && !open_pr {
        return Err("--pr-branch can only be used with --open-pr.".into());
    }

    if web_mode {
        if matches.get_one::<String>("add").is_some()
            || matches.get_flag("list")
            || matches.get_flag("list-shelves")
            || matches.get_one::<String>("create-shelf").is_some()
            || import_postman.is_some()
        {
            return Err(
                "--web cannot be combined with --add, --list, --list-shelves, --create-shelf, or --import-postman."
                    .into(),
            );
        }
        if matches.get_one::<String>("description").is_some() {
            return Err("--description cannot be used with --web.".into());
        }
        if matches.get_one::<usize>("limit").is_some() {
            return Err("--limit cannot be used with --web.".into());
        }
        if open_pr
            || matches.get_one::<String>("base-branch").is_some()
            || matches.get_one::<String>("pr-branch").is_some()
        {
            return Err(
                "--open-pr, --base-branch, and --pr-branch cannot be used with --web.".into(),
            );
        }
        if matches.get_one::<String>("shelf").is_some() {
            return Err("--shelf cannot be used with --web.".into());
        }
        if matches.get_one::<String>("team").is_some() || all_teams || local_only || shared_only {
            return Err(
                "--web cannot be combined with --team, --all-teams, --local-only, or --shared-only."
                    .into(),
            );
        }
        if has_keywords {
            return Err("--web cannot be combined with search keywords.".into());
        }
    }

    if local_only && shared_only {
        return Err("--local-only cannot be used together with --shared-only.".into());
    }

    if matches.get_one::<String>("team").is_some() && (local_only || shared_only) {
        return Err("--local-only and --shared-only cannot be used with --team.".into());
    }

    if all_teams && (local_only || shared_only) {
        return Err("--local-only and --shared-only cannot be used with --all-teams.".into());
    }

    if matches.get_one::<usize>("limit").is_some() && !matches.get_flag("list") && !has_keywords {
        return Err("--limit can only be used with --list or search keywords.".into());
    }

    if matches.get_one::<String>("description").is_some()
        && matches.get_one::<String>("add").is_none()
    {
        return Err("--description can only be used with --add.".into());
    }

    if open_pr
        && matches.get_one::<String>("add").is_none()
        && matches.get_one::<String>("create-shelf").is_none()
        && import_postman.is_none()
    {
        return Err(
            "--open-pr can only be used with --add, --create-shelf, or --import-postman.".into(),
        );
    }

    if open_pr && matches.get_one::<String>("team").is_none() {
        return Err("--open-pr requires --team so the change targets shared storage.".into());
    }

    if matches.get_flag("list-shelves") {
        if matches.get_one::<String>("add").is_some()
            || matches.get_flag("list")
            || matches.get_one::<String>("create-shelf").is_some()
            || import_postman.is_some()
        {
            return Err(
                "--list-shelves cannot be combined with --add, --list, --create-shelf, or --import-postman.".into(),
            );
        }
        if matches.get_one::<String>("description").is_some() {
            return Err("--description cannot be used with --list-shelves.".into());
        }
        if matches.get_one::<usize>("limit").is_some() {
            return Err("--limit cannot be used with --list-shelves.".into());
        }
        if open_pr {
            return Err("--open-pr cannot be used with --list-shelves.".into());
        }
        if matches.get_one::<String>("shelf").is_some() {
            return Err("--shelf cannot be used with --list-shelves.".into());
        }
        if has_keywords {
            return Err("--list-shelves cannot be combined with search keywords.".into());
        }
    }

    if let Some(create_shelf) = matches.get_one::<String>("create-shelf") {
        if all_teams {
            return Err("--all-teams cannot be used with --create-shelf.".into());
        }
        if local_only || shared_only {
            return Err(
                "--local-only and --shared-only cannot be used with --create-shelf.".into(),
            );
        }
        if matches.get_one::<String>("add").is_some()
            || matches.get_flag("list")
            || import_postman.is_some()
        {
            return Err(
                "--create-shelf cannot be combined with --add, --list, or --import-postman.".into(),
            );
        }
        if has_keywords {
            return Err("--create-shelf cannot be combined with search keywords.".into());
        }
        if matches.get_one::<String>("description").is_some() {
            return Err("--description cannot be used with --create-shelf.".into());
        }
        if matches.get_one::<usize>("limit").is_some() {
            return Err("--limit cannot be used with --create-shelf.".into());
        }
        if let Some(active_shelf) = matches.get_one::<String>("shelf") {
            if active_shelf != create_shelf {
                return Err("--shelf must match --create-shelf when both are provided.".into());
            }
        }
        if matches.get_one::<String>("repo").is_some()
            && matches.get_one::<String>("team").is_none()
        {
            return Err("--repo requires --team when creating a shared shelf.".into());
        }
        if matches.get_one::<String>("teams-dir").is_some()
            && matches.get_one::<String>("team").is_none()
        {
            return Err("--teams-dir requires --team when creating a shared shelf.".into());
        }
    }

    if matches.get_one::<String>("add").is_some() {
        if all_teams {
            return Err("--all-teams cannot be used with --add.".into());
        }
        if local_only || shared_only {
            return Err("--local-only and --shared-only cannot be used with --add.".into());
        }
        if matches.get_one::<String>("repo").is_some()
            && matches.get_one::<String>("team").is_none()
        {
            return Err("--repo requires --team when using shared repository write mode.".into());
        }
        if matches.get_one::<String>("teams-dir").is_some()
            && matches.get_one::<String>("team").is_none()
        {
            return Err(
                "--teams-dir requires --team when using shared repository write mode.".into(),
            );
        }
        if import_postman.is_some() {
            return Err("--add cannot be combined with --import-postman.".into());
        }
    }

    if import_postman.is_some() {
        if all_teams {
            return Err("--all-teams cannot be used with --import-postman.".into());
        }
        if local_only || shared_only {
            return Err(
                "--local-only and --shared-only cannot be used with --import-postman.".into(),
            );
        }
        if matches.get_flag("list") {
            return Err("--list cannot be combined with --import-postman.".into());
        }
        if matches.get_one::<String>("description").is_some() {
            return Err("--description cannot be used with --import-postman.".into());
        }
        if matches.get_one::<String>("shelf").is_some() {
            return Err(
                "--shelf cannot be used with --import-postman. Use --target-shelf instead.".into(),
            );
        }
        if matches.get_one::<usize>("limit").is_some() {
            return Err("--limit cannot be used with --import-postman.".into());
        }
        if has_keywords {
            return Err("--import-postman cannot be combined with search keywords.".into());
        }
        if matches.get_one::<String>("repo").is_some()
            && matches.get_one::<String>("team").is_none()
        {
            return Err("--repo requires --team when importing into shared storage.".into());
        }
        if matches.get_one::<String>("teams-dir").is_some()
            && matches.get_one::<String>("team").is_none()
        {
            return Err("--teams-dir requires --team when importing into shared storage.".into());
        }
    }
    Ok(())
}

fn validate_agent_flags(matches: &clap::ArgMatches, has_keywords: bool) -> Result<()> {
    let get = matches.get_one::<String>("get").is_some();
    let render = matches.get_one::<String>("render").is_some();
    let add = matches.get_one::<String>("add").is_some();
    let json = matches.get_flag("json");
    let raw = matches.get_flag("raw");
    let has_args = matches
        .get_many::<String>("arg")
        .is_some_and(|values| values.len() > 0);

    if get && render {
        return Err("--get and --render cannot be used together.".into());
    }
    if matches.get_one::<String>("name").is_some() && !add {
        return Err("--name can only be used with --add.".into());
    }
    if raw && !(get || render) {
        return Err("--raw can only be used with --get or --render.".into());
    }
    if raw && json {
        return Err("--raw and --json cannot be used together.".into());
    }
    if has_args && !render {
        return Err("--arg can only be used with --render.".into());
    }
    if json
        && !(add
            || get
            || render
            || matches.get_flag("list")
            || matches.get_flag("list-shelves")
            || has_keywords)
    {
        return Err(
            "--json requires --add, --get, --render, --list, --list-shelves, or search keywords."
                .into(),
        );
    }

    if get || render {
        let incompatible = add
            || matches.get_one::<String>("description").is_some()
            || matches.get_one::<String>("name").is_some()
            || matches.get_flag("list")
            || matches.get_flag("list-shelves")
            || matches.get_one::<String>("shelf").is_some()
            || matches.get_one::<String>("team").is_some()
            || matches.get_flag("all-teams")
            || matches.get_flag("local-only")
            || matches.get_flag("shared-only")
            || matches.get_one::<String>("create-shelf").is_some()
            || matches.get_one::<String>("import-postman").is_some()
            || matches.get_one::<String>("target-shelf").is_some()
            || matches.get_flag("open-pr")
            || matches.get_one::<String>("base-branch").is_some()
            || matches.get_one::<String>("pr-branch").is_some()
            || matches.get_one::<usize>("limit").is_some()
            || matches.get_flag("web")
            || matches.get_flag("force-sync")
            || matches.get_flag("force-sync-personal")
            || matches.get_flag("sync-personal")
            || matches.get_one::<String>("add-repo").is_some()
            || matches.get_one::<String>("add-personal-repo").is_some()
            || has_keywords;
        if incompatible {
            return Err("--get and --render must be used as standalone read operations.".into());
        }
    }

    Ok(())
}

fn resolve_target_shelf(matches: &clap::ArgMatches, config: &ShellshelfConfig) -> Result<String> {
    if let Some(create_shelf) = matches.get_one::<String>("create-shelf") {
        crate::config::validate_shelf_name(create_shelf)?;
        Ok(create_shelf.clone())
    } else {
        resolve_active_shelf(matches, config)
    }
}

fn create_shelf(
    matches: &clap::ArgMatches,
    data_file: &std::path::Path,
    shelf: &str,
) -> Result<bool> {
    if data_file.exists() {
        println!("Shelf '{shelf}' already exists.");
        return Ok(false);
    }

    CommandDatabase::new().save_to_file(data_file)?;

    if let Some(team) = matches.get_one::<String>("team") {
        println!("Created shelf '{shelf}' for team '{team}'.");
    } else {
        println!("Created shelf '{shelf}'.");
    }

    Ok(true)
}

struct ImportPostmanResult {
    changed: bool,
}

fn load_postman_import(
    matches: &clap::ArgMatches,
    import_path: &Path,
) -> Result<PostmanImportOutcome> {
    import_postman_collection(
        import_path,
        matches
            .get_one::<String>("target-shelf")
            .map(String::as_str),
    )
}

fn save_postman_import(
    matches: &clap::ArgMatches,
    shared_context: Option<&SharedStorageContext>,
    outcome: PostmanImportOutcome,
) -> Result<ImportPostmanResult> {
    let data_file = resolve_data_file_path(matches, shared_context, &outcome.shelf_name)?;

    if data_file.exists() {
        return Err(format!(
            "Shelf '{}' already exists. Use --target-shelf <NAME> to choose a different shelf name for this import.",
            outcome.shelf_name
        )
        .into());
    }

    outcome.database.save_to_file(&data_file)?;
    print_postman_import_summary(matches, &outcome);
    Ok(ImportPostmanResult { changed: true })
}

fn print_postman_import_summary(matches: &clap::ArgMatches, outcome: &PostmanImportOutcome) {
    let imported_count = outcome.database.commands.len();
    let skipped_count = outcome.warnings.len();
    let imported_label = if imported_count == 1 {
        "request"
    } else {
        "requests"
    };
    let skipped_label = if skipped_count == 1 {
        "request"
    } else {
        "requests"
    };

    if let Some(team) = matches.get_one::<String>("team") {
        println!(
            "Imported {imported_count} {imported_label} into shelf '{}' for team '{}'. Skipped {skipped_count} {skipped_label}.",
            outcome.shelf_name, team
        );
    } else {
        println!(
            "Imported {imported_count} {imported_label} into shelf '{}'. Skipped {skipped_count} {skipped_label}.",
            outcome.shelf_name
        );
    }

    if !outcome.warnings.is_empty() {
        eprintln!(
            "Warning: skipped {} {} during Postman import.",
            skipped_count, skipped_label
        );
        for warning in &outcome.warnings {
            eprintln!("- {}: {}", warning.request_name, warning.reason);
        }
    }
}

fn list_shelves_for_scope(
    matches: &clap::ArgMatches,
    config: &ShellshelfConfig,
    shared_context: Option<&SharedStorageContext>,
    json: bool,
) -> Result<()> {
    if let Some(team) = matches.get_one::<String>("team") {
        let shared_context = shared_context.ok_or(shared_repository_required_message())?;
        let sections = vec![ShelfSection {
            title: format!("Shared / {team}"),
            shelves: list_team_shelves(shared_context, team)?,
        }];
        emit_shelf_sections(
            &sections,
            &format!("No shelves available for team '{team}'."),
            json,
        )?;
        return Ok(());
    }

    if matches.get_flag("all-teams") {
        let shared_context = shared_context.ok_or(shared_repository_required_message())?;
        let sections = sections_from_grouped_team_shelves(list_all_team_shelves(shared_context)?);
        emit_shelf_sections(&sections, "No shelves available in shared storage.", json)?;
        return Ok(());
    }

    let plan = resolve_default_read_plan(matches, config, shared_context)?;
    let mut sections = Vec::new();

    if plan.include_local {
        sections.push(ShelfSection {
            title: "Local".to_string(),
            shelves: list_local_shelves()?,
        });
    }

    match plan.shared_target {
        Some(SharedReadTarget::Team(team)) => {
            let shared_context = shared_context.ok_or(shared_repository_required_message())?;
            sections.push(ShelfSection {
                title: format!("Shared / {team}"),
                shelves: list_team_shelves(shared_context, &team)?,
            });
        }
        Some(SharedReadTarget::AllTeams) => {
            let shared_context = shared_context.ok_or(shared_repository_required_message())?;
            sections.extend(sections_from_grouped_team_shelves(list_all_team_shelves(
                shared_context,
            )?));
        }
        None => {}
    }

    emit_shelf_sections(&sections, "No shelves available.", json)?;
    Ok(())
}

fn sections_from_grouped_team_shelves(grouped: Vec<(String, String)>) -> Vec<ShelfSection> {
    let mut sections = Vec::new();
    let mut current_team = None::<String>;
    let mut current_shelves = Vec::new();

    for (team, shelf) in grouped {
        if current_team.as_deref() != Some(team.as_str()) {
            if let Some(team_name) = current_team.take() {
                sections.push(ShelfSection {
                    title: format!("Shared / {team_name}"),
                    shelves: std::mem::take(&mut current_shelves),
                });
            }
            current_team = Some(team);
        }
        current_shelves.push(shelf);
    }

    if let Some(team_name) = current_team {
        sections.push(ShelfSection {
            title: format!("Shared / {team_name}"),
            shelves: current_shelves,
        });
    }

    sections
}

fn filter_commands(
    database: &CommandDatabase,
    shelf: &str,
    keywords: Option<&[String]>,
) -> Vec<OutputEntry> {
    match keywords {
        Some(keywords) => database
            .search_in_shelf(keywords, shelf)
            .into_iter()
            .map(OutputEntry::from_command)
            .collect(),
        None => database
            .commands
            .iter()
            .map(OutputEntry::from_command)
            .collect(),
    }
}

fn resolve_default_read_plan(
    matches: &clap::ArgMatches,
    config: &ShellshelfConfig,
    shared_context: Option<&SharedStorageContext>,
) -> Result<DefaultReadPlan> {
    if matches.get_flag("local-only") {
        return Ok(DefaultReadPlan {
            include_local: true,
            shared_target: None,
        });
    }

    if matches.get_flag("shared-only") {
        if shared_context.is_none() {
            return Err(shared_repository_required_message().into());
        }
        return Ok(DefaultReadPlan {
            include_local: false,
            shared_target: Some(
                config
                    .default_shared_read_target()
                    .map(Into::into)
                    .unwrap_or(SharedReadTarget::AllTeams),
            ),
        });
    }

    Ok(DefaultReadPlan {
        include_local: true,
        shared_target: if shared_context.is_some() {
            Some(
                config
                    .default_shared_read_target()
                    .map(Into::into)
                    .unwrap_or(SharedReadTarget::AllTeams),
            )
        } else {
            None
        },
    })
}

fn load_default_read_sections(
    local_db: &CommandDatabase,
    shared_context: Option<&SharedStorageContext>,
    shelf: &str,
    keywords: Option<&[String]>,
    plan: &DefaultReadPlan,
) -> Result<(Vec<OutputSection>, usize)> {
    let mut local_commands = if plan.include_local {
        filter_commands(local_db, shelf, keywords)
    } else {
        Vec::new()
    };

    let shared_sections = match &plan.shared_target {
        Some(target) => load_shared_sections_for_target(
            shared_context.ok_or(shared_repository_required_message())?,
            target,
            shelf,
            keywords,
        )?,
        None => Vec::new(),
    };

    let hidden_local_duplicates =
        hide_local_duplicates(&mut local_commands, shared_sections.as_slice());

    let mut sections = Vec::new();
    if !local_commands.is_empty() {
        sections.push(OutputSection::local(shelf.to_string(), local_commands));
    }
    sections.extend(shared_sections);

    Ok((sections, hidden_local_duplicates))
}

fn load_search_sections_without_active_shelf(
    matches: &clap::ArgMatches,
    config: &ShellshelfConfig,
    shared_context: Option<&SharedStorageContext>,
    keywords: &[String],
) -> Result<(Vec<OutputSection>, usize)> {
    if let Some(team) = matches.get_one::<String>("team") {
        let sections = load_shared_sections_for_team_all_shelves(
            shared_context.ok_or(shared_repository_required_message())?,
            team,
            keywords,
        )?;
        return Ok((sections, 0));
    }

    if matches.get_flag("all-teams") {
        let sections = load_shared_sections_for_all_shelves(
            shared_context.ok_or(shared_repository_required_message())?,
            keywords,
        )?;
        return Ok((sections, 0));
    }

    let plan = resolve_default_read_plan(matches, config, shared_context)?;
    let mut local_sections = if plan.include_local {
        load_local_sections_for_all_shelves(keywords)?
    } else {
        Vec::new()
    };
    let shared_sections = match &plan.shared_target {
        Some(SharedReadTarget::Team(team)) => load_shared_sections_for_team_all_shelves(
            shared_context.ok_or(shared_repository_required_message())?,
            team,
            keywords,
        )?,
        Some(SharedReadTarget::AllTeams) => load_shared_sections_for_all_shelves(
            shared_context.ok_or(shared_repository_required_message())?,
            keywords,
        )?,
        None => Vec::new(),
    };

    let hidden_local_duplicates =
        hide_local_duplicates_in_sections(&mut local_sections, shared_sections.as_slice());
    let mut sections = local_sections;
    sections.extend(shared_sections);
    Ok((sections, hidden_local_duplicates))
}

fn load_shared_sections_for_target(
    shared_context: &SharedStorageContext,
    target: &SharedReadTarget,
    shelf: &str,
    keywords: Option<&[String]>,
) -> Result<Vec<OutputSection>> {
    match target {
        SharedReadTarget::Team(team) => {
            let commands = load_team_commands(shared_context, team, shelf, keywords)?;
            Ok(vec![OutputSection::shared_team(
                team.clone(),
                shelf.to_string(),
                commands
                    .into_iter()
                    .map(OutputEntry::from_owned_command)
                    .collect(),
            )])
        }
        SharedReadTarget::AllTeams => load_shared_sections(shared_context, shelf, keywords),
    }
}

fn load_shared_sections(
    shared_context: &SharedStorageContext,
    shelf: &str,
    keywords: Option<&[String]>,
) -> Result<Vec<OutputSection>> {
    let results = load_all_team_commands(shared_context, shelf, keywords)?;
    let mut sections = Vec::new();
    let mut current_team = None::<String>;
    let mut current_commands = Vec::new();

    for (team, command) in results {
        if current_team.as_deref() != Some(team.as_str()) {
            if let Some(team_name) = current_team.take() {
                sections.push(OutputSection::shared_team(
                    team_name,
                    shelf.to_string(),
                    std::mem::take(&mut current_commands),
                ));
            }
            current_team = Some(team);
        }
        current_commands.push(OutputEntry::from_owned_command(command));
    }

    if let Some(team_name) = current_team {
        sections.push(OutputSection::shared_team(
            team_name,
            shelf.to_string(),
            current_commands,
        ));
    }

    Ok(sections)
}

fn load_local_sections_for_all_shelves(keywords: &[String]) -> Result<Vec<OutputSection>> {
    let mut sections = Vec::new();

    for shelf in list_local_shelves()? {
        let data_file = get_local_data_file_path(&shelf)?;
        let database = CommandDatabase::load_from_file(&data_file)?;
        sections.push(OutputSection::local(
            shelf.clone(),
            filter_commands(&database, &shelf, Some(keywords)),
        ));
    }

    Ok(sections)
}

fn load_shared_sections_for_team_all_shelves(
    shared_context: &SharedStorageContext,
    team: &str,
    keywords: &[String],
) -> Result<Vec<OutputSection>> {
    let mut sections = Vec::new();

    for shelf in list_team_shelves(shared_context, team)? {
        let commands = load_team_commands(shared_context, team, &shelf, Some(keywords))?;
        sections.push(OutputSection::shared_team(
            team.to_string(),
            shelf,
            commands
                .into_iter()
                .map(OutputEntry::from_owned_command)
                .collect(),
        ));
    }

    Ok(sections)
}

fn load_shared_sections_for_all_shelves(
    shared_context: &SharedStorageContext,
    keywords: &[String],
) -> Result<Vec<OutputSection>> {
    let mut sections = Vec::new();

    for (team, shelf) in list_all_team_shelves(shared_context)? {
        let commands = load_team_commands(shared_context, &team, &shelf, Some(keywords))?;
        sections.push(OutputSection::shared_team(
            team,
            shelf,
            commands
                .into_iter()
                .map(OutputEntry::from_owned_command)
                .collect(),
        ));
    }

    Ok(sections)
}

fn hide_local_duplicates(
    local_commands: &mut Vec<OutputEntry>,
    shared_sections: &[OutputSection],
) -> usize {
    let shared_commands = shared_commands(shared_sections);

    if shared_commands.is_empty() {
        return 0;
    }

    let original_len = local_commands.len();
    local_commands.retain(|command| !shared_commands.contains(command.command.as_str()));
    original_len.saturating_sub(local_commands.len())
}

fn hide_local_duplicates_in_sections(
    local_sections: &mut [OutputSection],
    shared_sections: &[OutputSection],
) -> usize {
    let shared_commands = shared_commands(shared_sections);

    if shared_commands.is_empty() {
        return 0;
    }

    let mut hidden = 0;
    for section in local_sections {
        if !matches!(section.source, OutputSectionSource::Local { .. }) {
            continue;
        }

        let original_len = section.entries.len();
        section
            .entries
            .retain(|entry| !shared_commands.contains(entry.command.as_str()));
        hidden += original_len.saturating_sub(section.entries.len());
    }

    hidden
}

fn shared_commands(shared_sections: &[OutputSection]) -> HashSet<&str> {
    shared_sections
        .iter()
        .filter(|section| section.is_shared())
        .flat_map(|section| section.entries.iter().map(|entry| entry.command.as_str()))
        .collect()
}

fn resolve_list_limit(matches: &clap::ArgMatches, config: &ShellshelfConfig) -> Option<usize> {
    if let Some(limit) = matches.get_one::<usize>("limit").copied() {
        return normalize_limit(limit);
    }

    match config.default_list_limit {
        Some(limit) => normalize_limit(limit),
        None => Some(DEFAULT_LIST_LIMIT),
    }
}

fn normalize_limit(limit: usize) -> Option<usize> {
    if limit == 0 {
        None
    } else {
        Some(limit)
    }
}

fn apply_list_limit(sections: &mut [OutputSection], limit: Option<usize>) -> usize {
    let Some(mut remaining) = limit else {
        return 0;
    };

    let mut hidden = 0;
    for section in sections {
        if remaining == 0 {
            hidden += section.entries.len();
            section.entries.clear();
            continue;
        }

        if section.entries.len() > remaining {
            hidden += section.entries.len() - remaining;
            section.entries.truncate(remaining);
            remaining = 0;
        } else {
            remaining -= section.entries.len();
        }
    }

    hidden
}

fn empty_message(filtered: bool, shelf: Option<&str>) -> String {
    match (filtered, shelf) {
        (true, Some(shelf)) => format!("No matching commands in shelf '{shelf}'."),
        (false, Some(shelf)) => format!("No commands stored in shelf '{shelf}'."),
        (true, None) => "No matching commands in any shelf.".to_string(),
        (false, None) => "No commands stored in any shelf.".to_string(),
    }
}

fn emit_sections(
    sections: &[OutputSection],
    empty_message: &str,
    summary: &OutputSummary,
    json: bool,
) -> Result<()> {
    if json {
        let results = sections_to_json_results(sections)?;
        println!(
            "{}",
            serde_json::to_string(&JsonResults {
                schema_version: 1,
                results,
                summary: JsonSummary {
                    hidden_duplicates: summary.hidden_local_duplicates,
                    hidden_by_limit: summary.hidden_due_to_limit,
                },
            })?
        );
        return Ok(());
    }

    let sections: Vec<&OutputSection> = sections
        .iter()
        .filter(|section| !section.entries.is_empty())
        .collect();

    if sections.is_empty() {
        println!("{empty_message}");
        return Ok(());
    }

    for (section_index, section) in sections.iter().enumerate() {
        if section_index > 0 {
            println!();
        }

        println!("{}", format_section_header(&section.title()));
        println!();

        for (index, entry) in section.entries.iter().enumerate() {
            if index > 0 {
                println!();
            }

            match entry.description.as_deref() {
                Some(description) => println!("[{}] {}", index + 1, description),
                None => println!("[{}]", index + 1),
            }
            println!("{}", entry.command);
        }
    }

    let duplicate_message = format_duplicate_hidden_message(summary.hidden_local_duplicates);
    let limit_message = format_limit_hidden_message(
        summary.hidden_due_to_limit,
        summary.active_limit,
        summary.search_limit,
    );

    if duplicate_message.is_some() || limit_message.is_some() {
        println!();
    }

    if let Some(message) = duplicate_message {
        println!("{message}");
    }

    if let Some(message) = limit_message {
        println!("{message}");
    }

    Ok(())
}

fn sections_to_json_results(sections: &[OutputSection]) -> Result<Vec<JsonCommandResult>> {
    let mut results = Vec::new();
    for section in sections {
        let (source, shelf) = match &section.source {
            OutputSectionSource::Local { shelf } => (JsonCommandSource::Local, shelf),
            OutputSectionSource::SharedTeam { team, shelf } => {
                (JsonCommandSource::Shared { team: team.clone() }, shelf)
            }
        };
        for entry in &section.entries {
            let command_ref = entry.name.as_deref().map(|name| match &section.source {
                OutputSectionSource::Local { shelf } => format!("local/{shelf}/{name}"),
                OutputSectionSource::SharedTeam { team, shelf } => {
                    format!("shared/{team}/{shelf}/{name}")
                }
            });
            let parameters = match entry.name.as_deref() {
                Some(_) => crate::template::parameters(
                    entry.template.as_deref().unwrap_or(&entry.command),
                )?,
                None => Vec::new(),
            };
            results.push(JsonCommandResult {
                command_ref,
                name: entry.name.clone(),
                description: entry.description.clone(),
                command: entry.command.clone(),
                parameters,
                source: source.clone(),
                shelf: shelf.clone(),
                template: entry.template.clone(),
            });
        }
    }
    Ok(results)
}

fn emit_shelf_sections(sections: &[ShelfSection], empty_message: &str, json: bool) -> Result<()> {
    if json {
        let results = sections
            .iter()
            .flat_map(|section| {
                let team = section.title.strip_prefix("Shared / ");
                section.shelves.iter().map(move |shelf| JsonShelfResult {
                    source: if team.is_some() { "shared" } else { "local" },
                    team,
                    shelf,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string(&JsonShelfResults {
                schema_version: 1,
                results,
            })?
        );
        return Ok(());
    }

    let sections: Vec<&ShelfSection> = sections
        .iter()
        .filter(|section| !section.shelves.is_empty())
        .collect();

    if sections.is_empty() {
        println!("{empty_message}");
        return Ok(());
    }

    for (section_index, section) in sections.iter().enumerate() {
        if section_index > 0 {
            println!();
        }

        println!("{}", format_section_header(&section.title));
        println!();

        for (index, shelf) in section.shelves.iter().enumerate() {
            println!("[{}] {}", index + 1, shelf);
        }
    }
    Ok(())
}

fn format_section_header(title: &str) -> String {
    format!("=== {} ===", title.to_uppercase())
}

fn format_duplicate_hidden_message(hidden_local_duplicates: usize) -> Option<String> {
    if hidden_local_duplicates == 0 {
        None
    } else if hidden_local_duplicates == 1 {
        Some("1 local command was hidden because it duplicates shared storage.".to_string())
    } else {
        Some(format!(
            "{hidden_local_duplicates} local commands were hidden because they duplicate shared storage."
        ))
    }
}

fn format_limit_hidden_message(
    hidden_due_to_limit: usize,
    active_limit: Option<usize>,
    search_limit: bool,
) -> Option<String> {
    let limit = active_limit?;
    let limit_kind = if search_limit { "search" } else { "list" };

    if hidden_due_to_limit == 0 {
        None
    } else if hidden_due_to_limit == 1 {
        Some(format!(
            "Showing first {limit} commands. 1 additional command was hidden by the active {limit_kind} limit."
        ))
    } else {
        Some(format!(
            "Showing first {limit} commands. {hidden_due_to_limit} additional commands were hidden by the active {limit_kind} limit."
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_list_limit, format_duplicate_hidden_message, format_limit_hidden_message,
        format_section_header, hide_local_duplicates, normalize_limit, OutputEntry, OutputSection,
    };

    #[test]
    fn test_format_section_header_for_local() {
        assert_eq!(
            format_section_header("Local / curl"),
            "=== LOCAL / CURL ==="
        );
    }

    #[test]
    fn test_format_section_header_for_shared_team() {
        assert_eq!(
            format_section_header("Shared / platform / curl"),
            "=== SHARED / PLATFORM / CURL ==="
        );
    }

    #[test]
    fn test_hide_local_duplicates_against_shared_sections() {
        let mut local_commands = vec![
            OutputEntry {
                name: None,
                command: "curl https://shared.example.com/health".to_string(),
                description: Some("Shared health".to_string()),
                template: None,
            },
            OutputEntry {
                name: None,
                command: "curl https://local.example.com/health".to_string(),
                description: Some("Local health".to_string()),
                template: None,
            },
        ];
        let shared_sections = vec![OutputSection::shared_team(
            "platform",
            "curl",
            vec![OutputEntry {
                name: None,
                command: "curl https://shared.example.com/health".to_string(),
                description: Some("Shared health".to_string()),
                template: None,
            }],
        )];

        let hidden = hide_local_duplicates(&mut local_commands, &shared_sections);

        assert_eq!(hidden, 1);
        assert_eq!(
            local_commands,
            vec![OutputEntry {
                name: None,
                command: "curl https://local.example.com/health".to_string(),
                description: Some("Local health".to_string()),
                template: None,
            }]
        );
    }

    #[test]
    fn test_apply_list_limit_across_sections() {
        let mut sections = vec![
            OutputSection::local(
                "curl",
                vec![
                    OutputEntry {
                        name: None,
                        command: "curl https://local.example.com/one".to_string(),
                        description: None,
                        template: None,
                    },
                    OutputEntry {
                        name: None,
                        command: "curl https://local.example.com/two".to_string(),
                        description: Some("Second".to_string()),
                        template: None,
                    },
                ],
            ),
            OutputSection::shared_team(
                "platform",
                "curl",
                vec![OutputEntry {
                    name: None,
                    command: "curl https://shared.example.com/one".to_string(),
                    description: None,
                    template: None,
                }],
            ),
        ];

        let hidden = apply_list_limit(&mut sections, Some(2));

        assert_eq!(hidden, 1);
        assert_eq!(sections[0].entries.len(), 2);
        assert!(sections[1].entries.is_empty());
    }

    #[test]
    fn test_normalize_limit_zero_means_unlimited() {
        assert_eq!(normalize_limit(0), None);
        assert_eq!(normalize_limit(5), Some(5));
    }

    #[test]
    fn test_duplicate_hidden_message_pluralization() {
        assert_eq!(
            format_duplicate_hidden_message(1),
            Some("1 local command was hidden because it duplicates shared storage.".to_string())
        );
        assert_eq!(
            format_duplicate_hidden_message(2),
            Some("2 local commands were hidden because they duplicate shared storage.".to_string())
        );
    }

    #[test]
    fn test_limit_hidden_message_pluralization() {
        assert_eq!(
            format_limit_hidden_message(1, Some(20), false),
            Some(
                "Showing first 20 commands. 1 additional command was hidden by the active list limit."
                    .to_string()
            )
        );
        assert_eq!(
            format_limit_hidden_message(3, Some(10), false),
            Some(
                "Showing first 10 commands. 3 additional commands were hidden by the active list limit."
                    .to_string()
            )
        );
    }
}
