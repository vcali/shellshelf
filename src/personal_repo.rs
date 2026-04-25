use crate::config::{validate_shelf_name, PersonalStorageContext};
use crate::database::{CommandDatabase, MergeDatabaseOutcome};
use crate::github::{
    get_default_github_state_root, get_github_repo_state_stamp_path,
    should_refresh_github_repo_state, write_github_repo_state_stamp,
};
use crate::Result;
use shell_words::quote as shell_quote;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

const PERSONAL_SHELVES_DIR: &str = "shelves";
const PERSONAL_REPO_SYNC_STATUS_STAMP_SUFFIX: &str = "personal-sync-status";

struct PersonalBaseBranch {
    name: String,
    exists_on_origin: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PersonalRepoBootstrapMode {
    Auto,
    Merge,
    Push,
    Pull,
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PersonalRepoBootstrapOutcome {
    Skipped,
    Merged,
    Pushed,
    Pulled,
    AlreadyInSync,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PersonalRepoSyncWarning {
    Ahead {
        local_commits: usize,
        push_command: String,
        inspect_command: String,
    },
    Behind {
        remote_commits: usize,
        pull_command: String,
        inspect_command: String,
    },
    Diverged {
        local_commits: usize,
        remote_commits: usize,
        pull_command: String,
        push_command: String,
        inspect_command: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct MergeShelvesOutcome {
    local_shelves_changed: usize,
    personal_shelves_changed: usize,
    duplicate_commands_removed: usize,
    descriptions_upgraded: usize,
}

pub(crate) fn sync_personal_local_shelf(
    personal_context: &PersonalStorageContext,
    local_data_file: &Path,
    shelf: &str,
) -> Result<bool> {
    validate_shelf_name(shelf)?;
    sync_personal_repo(
        personal_context,
        [(
            local_data_file.to_path_buf(),
            personal_shelf_path(personal_context, shelf),
        )],
        format!("Sync personal shelf '{shelf}'"),
    )
}

pub(crate) fn sync_all_personal_shelves(
    personal_context: &PersonalStorageContext,
    local_shelves_root: &Path,
) -> Result<usize> {
    let mut sync_pairs = Vec::new();

    if local_shelves_root.exists() {
        for entry in fs::read_dir(local_shelves_root)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }

            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }

            let Some(shelf) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_string)
            else {
                continue;
            };

            if validate_shelf_name(&shelf).is_err() {
                continue;
            }

            sync_pairs.push((path, personal_shelf_path(personal_context, &shelf)));
        }
    }

    if sync_pairs.is_empty() {
        return Ok(0);
    }

    let changed = sync_personal_repo(
        personal_context,
        sync_pairs,
        "Sync personal shelves".to_string(),
    )?;

    Ok(usize::from(changed))
}

pub(crate) fn bootstrap_personal_repo(
    personal_context: &PersonalStorageContext,
    local_shelves_root: &Path,
    mode: PersonalRepoBootstrapMode,
) -> Result<PersonalRepoBootstrapOutcome> {
    match mode {
        PersonalRepoBootstrapMode::Skip => return Ok(PersonalRepoBootstrapOutcome::Skipped),
        PersonalRepoBootstrapMode::Merge => {
            return Ok(
                if merge_all_personal_shelves(personal_context, local_shelves_root)?
                    == MergeShelvesOutcome::default()
                {
                    PersonalRepoBootstrapOutcome::AlreadyInSync
                } else {
                    PersonalRepoBootstrapOutcome::Merged
                },
            );
        }
        PersonalRepoBootstrapMode::Push => {
            return Ok(
                if sync_all_personal_shelves(personal_context, local_shelves_root)? > 0 {
                    PersonalRepoBootstrapOutcome::Pushed
                } else {
                    PersonalRepoBootstrapOutcome::AlreadyInSync
                },
            );
        }
        PersonalRepoBootstrapMode::Pull => {
            return Ok(
                if import_all_personal_shelves(personal_context, local_shelves_root)? > 0 {
                    PersonalRepoBootstrapOutcome::Pulled
                } else {
                    PersonalRepoBootstrapOutcome::AlreadyInSync
                },
            );
        }
        PersonalRepoBootstrapMode::Auto => {}
    }

    let local_shelves = collect_shelf_snapshots(local_shelves_root)?;
    let personal_shelves =
        collect_shelf_snapshots(&personal_context.repository_root.join(PERSONAL_SHELVES_DIR))?;

    if local_shelves.is_empty() && personal_shelves.is_empty() {
        return Ok(PersonalRepoBootstrapOutcome::Skipped);
    }

    if personal_shelves.is_empty() {
        return Ok(
            if sync_all_personal_shelves(personal_context, local_shelves_root)? > 0 {
                PersonalRepoBootstrapOutcome::Pushed
            } else {
                PersonalRepoBootstrapOutcome::AlreadyInSync
            },
        );
    }

    if local_shelves.is_empty() {
        return Ok(
            if import_all_personal_shelves(personal_context, local_shelves_root)? > 0 {
                PersonalRepoBootstrapOutcome::Pulled
            } else {
                PersonalRepoBootstrapOutcome::AlreadyInSync
            },
        );
    }

    if local_shelves == personal_shelves {
        Ok(PersonalRepoBootstrapOutcome::AlreadyInSync)
    } else {
        Ok(
            if merge_all_personal_shelves(personal_context, local_shelves_root)?
                == MergeShelvesOutcome::default()
            {
                PersonalRepoBootstrapOutcome::AlreadyInSync
            } else {
                PersonalRepoBootstrapOutcome::Merged
            },
        )
    }
}

pub(crate) fn personal_repo_sync_warning(
    personal_context: &PersonalStorageContext,
) -> Result<Option<PersonalRepoSyncWarning>> {
    if personal_context.managed_github_repo.is_none() {
        return Ok(None);
    }

    let mut base_branch = detect_default_base_branch(&personal_context.repository_root)?;
    maybe_refresh_personal_repo_sync_refs(personal_context, &base_branch.name);
    if !base_branch.exists_on_origin {
        base_branch.exists_on_origin =
            remote_branch_exists(&personal_context.repository_root, &base_branch.name)?;
    }
    if !base_branch.exists_on_origin
        || !local_branch_exists(&personal_context.repository_root, &base_branch.name)?
    {
        return Ok(None);
    }

    let comparison_ref = format!("{}...origin/{}", base_branch.name, base_branch.name);
    let rev_list = run_git(
        &personal_context.repository_root,
        [
            "rev-list",
            "--left-right",
            "--count",
            comparison_ref.as_str(),
        ],
    )?;
    let mut counts = rev_list.split_whitespace();
    let local_commits = counts
        .next()
        .ok_or("Failed to parse local personal repository sync status.")?
        .parse::<usize>()?;
    let remote_commits = counts
        .next()
        .ok_or("Failed to parse remote personal repository sync status.")?
        .parse::<usize>()?;

    if local_commits == 0 && remote_commits == 0 {
        return Ok(None);
    }

    let repo_root_display = personal_context.repository_root.display().to_string();
    let repo_root = shell_quote(&repo_root_display);
    let inspect_command = format!(
        "git -C {repo_root} fetch origin {branch} && git -C {repo_root} log --oneline --left-right --cherry {branch}...origin/{branch} && git -C {repo_root} diff --stat {branch}...origin/{branch}",
        branch = base_branch.name,
    );

    Ok(Some(match (local_commits, remote_commits) {
        (local_commits, 0) => PersonalRepoSyncWarning::Ahead {
            local_commits,
            push_command: format!(
                "git -C {repo_root} push origin {branch}",
                branch = base_branch.name
            ),
            inspect_command,
        },
        (0, remote_commits) => PersonalRepoSyncWarning::Behind {
            remote_commits,
            pull_command: format!(
                "git -C {repo_root} pull --ff-only origin {branch}",
                branch = base_branch.name
            ),
            inspect_command,
        },
        (local_commits, remote_commits) => PersonalRepoSyncWarning::Diverged {
            local_commits,
            remote_commits,
            pull_command: format!(
                "git -C {repo_root} pull --rebase origin {branch}",
                branch = base_branch.name
            ),
            push_command: format!(
                "git -C {repo_root} push origin {branch}",
                branch = base_branch.name
            ),
            inspect_command,
        },
    }))
}

fn maybe_refresh_personal_repo_sync_refs(personal_context: &PersonalStorageContext, branch: &str) {
    let Some(github_repo) = personal_context.managed_github_repo.as_deref() else {
        return;
    };
    let Some(sync_check_interval) = personal_context.sync_check_interval else {
        return;
    };

    let state_root = get_default_github_state_root();
    let Ok(stamp_path) = get_github_repo_state_stamp_path(
        &state_root,
        github_repo,
        PERSONAL_REPO_SYNC_STATUS_STAMP_SUFFIX,
    ) else {
        return;
    };

    if !should_refresh_github_repo_state(&stamp_path, sync_check_interval) {
        return;
    }

    let _ = run_git(
        &personal_context.repository_root,
        ["fetch", "origin", branch],
    );
    let _ = write_github_repo_state_stamp(
        &state_root,
        github_repo,
        PERSONAL_REPO_SYNC_STATUS_STAMP_SUFFIX,
    );
}

pub(crate) fn personal_shelf_path(
    personal_context: &PersonalStorageContext,
    shelf: &str,
) -> PathBuf {
    personal_context
        .repository_root
        .join(PERSONAL_SHELVES_DIR)
        .join(format!("{shelf}.json"))
}

fn import_all_personal_shelves(
    personal_context: &PersonalStorageContext,
    local_shelves_root: &Path,
) -> Result<usize> {
    let personal_shelves_root = personal_context.repository_root.join(PERSONAL_SHELVES_DIR);
    let shelf_paths = collect_shelf_paths(&personal_shelves_root)?;
    if shelf_paths.is_empty() {
        return Ok(0);
    }

    fs::create_dir_all(local_shelves_root)?;
    let mut changed = 0;

    for (shelf, source_path) in shelf_paths {
        let target_path = local_shelves_root.join(format!("{shelf}.json"));
        let source_bytes = fs::read(&source_path)?;
        let target_matches = match fs::read(&target_path) {
            Ok(existing_bytes) => existing_bytes == source_bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };

        if target_matches {
            continue;
        }

        fs::write(target_path, source_bytes)?;
        changed += 1;
    }

    Ok(changed)
}

fn merge_all_personal_shelves(
    personal_context: &PersonalStorageContext,
    local_shelves_root: &Path,
) -> Result<MergeShelvesOutcome> {
    let personal_shelves_root = personal_context.repository_root.join(PERSONAL_SHELVES_DIR);
    let local_shelf_paths = collect_shelf_paths(local_shelves_root)?;
    let personal_shelf_paths = collect_shelf_paths(&personal_shelves_root)?;
    if local_shelf_paths.is_empty() && personal_shelf_paths.is_empty() {
        return Ok(MergeShelvesOutcome::default());
    }

    ensure_clean_worktree(&personal_context.repository_root)?;
    let base_branch = detect_default_base_branch(&personal_context.repository_root)?;
    switch_to_branch(
        &personal_context.repository_root,
        &base_branch.name,
        base_branch.exists_on_origin,
    )?;
    if base_branch.exists_on_origin {
        run_git(
            &personal_context.repository_root,
            ["pull", "--ff-only", "origin", base_branch.name.as_str()],
        )?;
    }

    fs::create_dir_all(local_shelves_root)?;
    fs::create_dir_all(&personal_shelves_root)?;

    let mut all_shelves = collect_shelf_names(&local_shelf_paths);
    for shelf in collect_shelf_names(&personal_shelf_paths) {
        if !all_shelves.contains(&shelf) {
            all_shelves.push(shelf);
        }
    }
    all_shelves.sort();

    let mut outcome = MergeShelvesOutcome::default();
    let mut changed_personal_paths = Vec::new();

    for shelf in all_shelves {
        let local_path = local_shelves_root.join(format!("{shelf}.json"));
        let personal_path = personal_shelf_path(personal_context, &shelf);
        let local_db = CommandDatabase::load_from_file(&local_path)?;
        let personal_db = CommandDatabase::load_from_file(&personal_path)?;
        let (merged_db, merge_outcome) = local_db.merged_with(&personal_db);

        accumulate_merge_outcome(&mut outcome, merge_outcome);

        if local_db != merged_db {
            merged_db.save_to_file(&local_path)?;
            outcome.local_shelves_changed += 1;
        }

        if personal_db != merged_db {
            merged_db.save_to_file(&personal_path)?;
            outcome.personal_shelves_changed += 1;
            changed_personal_paths.push(personal_path);
        }
    }

    if !changed_personal_paths.is_empty() {
        commit_personal_repo_changes(
            personal_context,
            &base_branch,
            &changed_personal_paths,
            "Merge personal shelves",
        )?;
    }

    Ok(outcome)
}

fn collect_shelf_snapshots(root: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut snapshots = Vec::new();
    for (shelf, path) in collect_shelf_paths(root)? {
        snapshots.push((shelf, fs::read(path)?));
    }
    Ok(snapshots)
}

fn collect_shelf_paths(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut shelf_paths = Vec::new();

    if !root.exists() {
        return Ok(shelf_paths);
    }

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }

        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }

        let Some(shelf) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string)
        else {
            continue;
        };

        if validate_shelf_name(&shelf).is_err() {
            continue;
        }

        shelf_paths.push((shelf, path));
    }

    shelf_paths.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(shelf_paths)
}

fn collect_shelf_names(paths: &[(String, PathBuf)]) -> Vec<String> {
    paths.iter().map(|(shelf, _path)| shelf.clone()).collect()
}

fn sync_personal_repo(
    personal_context: &PersonalStorageContext,
    sync_pairs: impl IntoIterator<Item = (PathBuf, PathBuf)>,
    commit_message: String,
) -> Result<bool> {
    ensure_clean_worktree(&personal_context.repository_root)?;

    let base_branch = detect_default_base_branch(&personal_context.repository_root)?;
    switch_to_branch(
        &personal_context.repository_root,
        &base_branch.name,
        base_branch.exists_on_origin,
    )?;
    if base_branch.exists_on_origin {
        run_git(
            &personal_context.repository_root,
            ["pull", "--ff-only", "origin", base_branch.name.as_str()],
        )?;
    }

    let mut changed_paths = Vec::new();

    for (source_path, target_path) in sync_pairs {
        let Some(parent) = target_path.parent() else {
            return Err("Personal shelf path must have a parent directory.".into());
        };
        fs::create_dir_all(parent)?;
        fs::copy(source_path, &target_path)?;
        changed_paths.push(target_path);
    }

    if !paths_have_changes(&personal_context.repository_root, &changed_paths)? {
        return Ok(false);
    }

    commit_personal_repo_changes(
        personal_context,
        &base_branch,
        &changed_paths,
        commit_message.as_str(),
    )?;

    Ok(true)
}

fn accumulate_merge_outcome(total: &mut MergeShelvesOutcome, next: MergeDatabaseOutcome) {
    total.duplicate_commands_removed += next.duplicate_commands_removed;
    total.descriptions_upgraded += next.descriptions_upgraded;
}

fn commit_personal_repo_changes(
    personal_context: &PersonalStorageContext,
    base_branch: &PersonalBaseBranch,
    changed_paths: &[PathBuf],
    commit_message: &str,
) -> Result<()> {
    run_git_with_os_args(
        &personal_context.repository_root,
        std::iter::once(OsString::from("add")).chain(
            changed_paths
                .iter()
                .map(|path| path.as_os_str().to_os_string()),
        ),
    )?;
    run_git(
        &personal_context.repository_root,
        ["commit", "-m", commit_message],
    )?;
    run_git(
        &personal_context.repository_root,
        ["push", "origin", base_branch.name.as_str()],
    )?;
    Ok(())
}

fn detect_default_base_branch(repo_root: &Path) -> Result<PersonalBaseBranch> {
    if let Ok(symbolic_ref) = run_git(repo_root, ["symbolic-ref", "refs/remotes/origin/HEAD"]) {
        let trimmed = symbolic_ref.trim();
        if let Some(branch) = trimmed.strip_prefix("refs/remotes/origin/") {
            return Ok(PersonalBaseBranch {
                name: branch.to_string(),
                exists_on_origin: true,
            });
        }
    }

    for branch in ["main", "master"] {
        if remote_branch_exists(repo_root, branch)? {
            return Ok(PersonalBaseBranch {
                name: branch.to_string(),
                exists_on_origin: true,
            });
        }
    }

    let branch = current_branch(repo_root)?;
    if !branch.is_empty() {
        // Freshly cloned empty repositories often start on "master" even though Shellshelf
        // treats "main" as the default bootstrap branch when origin has no branch yet.
        let branch = if branch == "master" {
            "main".to_string()
        } else {
            branch
        };
        return Ok(PersonalBaseBranch {
            exists_on_origin: remote_branch_exists(repo_root, &branch)?,
            name: branch,
        });
    }

    Err("Could not determine the personal repository base branch.".into())
}

fn ensure_clean_worktree(repo_root: &Path) -> Result<()> {
    let status = run_git(repo_root, ["status", "--porcelain"])?;
    if status.trim().is_empty() {
        Ok(())
    } else {
        Err(
            "Personal repository checkout has uncommitted changes. Commit, stash, or discard them before syncing local shelves."
                .into(),
        )
    }
}

fn current_branch(repo_root: &Path) -> Result<String> {
    Ok(run_git(repo_root, ["branch", "--show-current"])?
        .trim()
        .to_string())
}

fn local_branch_exists(repo_root: &Path, branch: &str) -> Result<bool> {
    let branch_ref = format!("refs/heads/{branch}");
    Ok(git_exit_status(
        repo_root,
        ["show-ref", "--verify", "--quiet", branch_ref.as_str()],
    )?
    .success())
}

fn remote_branch_exists(repo_root: &Path, branch: &str) -> Result<bool> {
    let branch_ref = format!("refs/remotes/origin/{branch}");
    Ok(git_exit_status(
        repo_root,
        ["show-ref", "--verify", "--quiet", branch_ref.as_str()],
    )?
    .success())
}

fn switch_to_branch(repo_root: &Path, branch: &str, branch_exists_on_origin: bool) -> Result<()> {
    if current_branch(repo_root)? == branch {
        return Ok(());
    }

    if local_branch_exists(repo_root, branch)? {
        run_git(repo_root, ["switch", branch])?;
    } else if branch_exists_on_origin {
        let upstream_branch = format!("origin/{branch}");
        run_git(
            repo_root,
            ["switch", "-c", branch, "--track", upstream_branch.as_str()],
        )?;
    } else {
        run_git(repo_root, ["switch", "-c", branch])?;
    }

    Ok(())
}

fn paths_have_changes(repo_root: &Path, changed_paths: &[PathBuf]) -> Result<bool> {
    let status = run_git_with_os_args(
        repo_root,
        std::iter::once(OsString::from("status"))
            .chain(std::iter::once(OsString::from("--porcelain")))
            .chain(std::iter::once(OsString::from("--")))
            .chain(
                changed_paths
                    .iter()
                    .map(|path| path.as_os_str().to_os_string()),
            ),
    )?;

    Ok(!status.trim().is_empty())
}

fn run_git(repo_root: &Path, args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> Result<String> {
    let output = ProcessCommand::new(git_binary())
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .map_err(|error| format!("Personal sync requires Git to be installed: {error}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("git failed: {}", stderr.trim()).into())
    }
}

fn run_git_with_os_args(
    repo_root: &Path,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
) -> Result<String> {
    run_git(repo_root, args)
}

fn git_exit_status(
    repo_root: &Path,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
) -> Result<std::process::ExitStatus> {
    ProcessCommand::new(git_binary())
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .status()
        .map_err(|error| format!("Personal sync requires Git to be installed: {error}").into())
}

fn git_binary() -> String {
    std::env::var("SHELLSHELF_GIT_BIN").unwrap_or_else(|_| "git".to_string())
}
