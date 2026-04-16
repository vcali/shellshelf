use crate::config::{validate_shelf_name, BackupStorageContext};
use crate::Result;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

const BACKUP_SHELVES_DIR: &str = "shelves";

pub(crate) fn backup_local_shelf(
    backup_context: &BackupStorageContext,
    local_data_file: &Path,
    shelf: &str,
) -> Result<bool> {
    validate_shelf_name(shelf)?;
    sync_backup_repo(
        backup_context,
        [(
            local_data_file.to_path_buf(),
            backup_shelf_path(backup_context, shelf),
        )],
        format!("Backup local shelf '{shelf}'"),
    )
}

pub(crate) fn sync_all_local_shelves(
    backup_context: &BackupStorageContext,
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

            sync_pairs.push((path, backup_shelf_path(backup_context, &shelf)));
        }
    }

    if sync_pairs.is_empty() {
        return Ok(0);
    }

    let changed = sync_backup_repo(
        backup_context,
        sync_pairs,
        "Sync local shelves backup".to_string(),
    )?;

    Ok(usize::from(changed))
}

pub(crate) fn backup_shelf_path(backup_context: &BackupStorageContext, shelf: &str) -> PathBuf {
    backup_context
        .repository_root
        .join(BACKUP_SHELVES_DIR)
        .join(format!("{shelf}.json"))
}

fn sync_backup_repo(
    backup_context: &BackupStorageContext,
    sync_pairs: impl IntoIterator<Item = (PathBuf, PathBuf)>,
    commit_message: String,
) -> Result<bool> {
    ensure_clean_worktree(&backup_context.repository_root)?;

    let base_branch = detect_default_base_branch(&backup_context.repository_root)?;
    switch_to_branch(&backup_context.repository_root, &base_branch)?;
    run_git(
        &backup_context.repository_root,
        ["pull", "--ff-only", "origin", base_branch.as_str()],
    )?;

    let mut changed_paths = Vec::new();

    for (source_path, target_path) in sync_pairs {
        let Some(parent) = target_path.parent() else {
            return Err("Backup shelf path must have a parent directory.".into());
        };
        fs::create_dir_all(parent)?;
        fs::copy(source_path, &target_path)?;
        changed_paths.push(target_path);
    }

    if !paths_have_changes(&backup_context.repository_root, &changed_paths)? {
        return Ok(false);
    }

    run_git_with_os_args(
        &backup_context.repository_root,
        std::iter::once(OsString::from("add")).chain(
            changed_paths
                .iter()
                .map(|path| path.as_os_str().to_os_string()),
        ),
    )?;
    run_git(
        &backup_context.repository_root,
        ["commit", "-m", commit_message.as_str()],
    )?;
    run_git(
        &backup_context.repository_root,
        ["push", "origin", base_branch.as_str()],
    )?;

    Ok(true)
}

fn detect_default_base_branch(repo_root: &Path) -> Result<String> {
    if let Ok(symbolic_ref) = run_git(repo_root, ["symbolic-ref", "refs/remotes/origin/HEAD"]) {
        let trimmed = symbolic_ref.trim();
        if let Some(branch) = trimmed.strip_prefix("refs/remotes/origin/") {
            return Ok(branch.to_string());
        }
    }

    for branch in ["main", "master"] {
        let remote_ref = format!("refs/remotes/origin/{branch}");
        if git_exit_status(
            repo_root,
            ["show-ref", "--verify", "--quiet", remote_ref.as_str()],
        )?
        .success()
        {
            return Ok(branch.to_string());
        }
    }

    Err("Could not determine the backup repository base branch.".into())
}

fn ensure_clean_worktree(repo_root: &Path) -> Result<()> {
    let status = run_git(repo_root, ["status", "--porcelain"])?;
    if status.trim().is_empty() {
        Ok(())
    } else {
        Err(
            "Backup repository checkout has uncommitted changes. Commit, stash, or discard them before syncing local shelves."
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

fn switch_to_branch(repo_root: &Path, branch: &str) -> Result<()> {
    if current_branch(repo_root)? == branch {
        return Ok(());
    }

    if local_branch_exists(repo_root, branch)? {
        run_git(repo_root, ["switch", branch])?;
    } else {
        let upstream_branch = format!("origin/{branch}");
        run_git(
            repo_root,
            ["switch", "-c", branch, "--track", upstream_branch.as_str()],
        )?;
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
        .map_err(|error| format!("Backup sync requires Git to be installed: {error}"))?;

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
        .map_err(|error| format!("Backup sync requires Git to be installed: {error}").into())
}

fn git_binary() -> String {
    std::env::var("SHELLSHELF_GIT_BIN").unwrap_or_else(|_| "git".to_string())
}
