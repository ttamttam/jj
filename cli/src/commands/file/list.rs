// Copyright 2020 The Jujutsu Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

use clap_complete::ArgValueCandidates;
use clap_complete::ArgValueCompleter;
use futures::StreamExt as _;
use jj_lib::matchers::Matcher;
use jj_lib::repo_path::RepoPath;
use jj_lib::repo_path::RepoPathBuf;
use jj_lib::repo_path::RepoPathComponent;
use jj_lib::working_copy::IgnoredPath;
use tracing::instrument;

use crate::cli_util::CommandHelper;
use crate::cli_util::RevisionArg;
use crate::cli_util::WorkspaceCommandHelper;
use crate::cli_util::print_unmatched_explicit_paths;
use crate::command_error::CommandError;
use crate::command_error::user_error;
use crate::commit_templater::TreeEntry;
use crate::complete;
use crate::templater::TemplateRenderer;
use crate::ui::Ui;

/// List files in a revision
///
/// With `--ignored`, list paths ignored by .gitignore in the working copy
/// instead. The listed paths are exactly the ones that `--delete` would remove.
#[derive(clap::Args, Clone, Debug)]
pub(crate) struct FileListArgs {
    /// The revision to list files in
    #[arg(long, short, default_value = "@", value_name = "REVSET")]
    #[arg(add = ArgValueCompleter::new(complete::revset_expression_all))]
    #[arg(conflicts_with = "ignored")]
    revision: RevisionArg,

    /// Render each file entry using the given template
    ///
    /// All 0-argument methods of the [`TreeEntry` type][TreeEntry] are
    /// available as keywords in the template expression. See
    /// [`jj help -k templates`][templates] for more information.
    ///
    /// The default template can be set by the `templates.file_list`
    /// setting.
    ///
    /// [TreeEntry]: https://docs.jj-vcs.dev/latest/templates/#treeentry-type
    ///
    /// [templates]: https://docs.jj-vcs.dev/latest/templates/
    #[arg(long, short = 'T')]
    #[arg(add = ArgValueCandidates::new(complete::template_aliases))]
    #[arg(conflicts_with = "ignored")]
    template: Option<String>,

    /// List paths ignored by .gitignore in the working copy instead of files
    /// in a revision
    ///
    /// A directory whose content is entirely ignored is listed as a single
    /// entry, with a trailing `/`.
    #[arg(long)]
    ignored: bool,

    /// Delete the listed ignored paths instead of listing them
    ///
    /// Ignored directories are removed recursively. Only the listed paths are
    /// removed, so `file list --ignored` can be used as a dry run.
    #[arg(long, requires = "ignored")]
    delete: bool,

    /// Only list files matching these prefixes (instead of all files)
    #[arg(value_name = "FILESETS", value_hint = clap::ValueHint::AnyPath)]
    #[arg(add = ArgValueCompleter::new(complete::file_list_paths))]
    paths: Vec<String>,
}

#[instrument(skip_all)]
pub(crate) async fn cmd_file_list(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &FileListArgs,
) -> Result<(), CommandError> {
    if args.ignored {
        return cmd_file_list_ignored(ui, command, args).await;
    }

    let workspace_command = command.workspace_helper(ui).await?;
    let commit = workspace_command
        .resolve_single_rev(ui, &args.revision)
        .await?;
    let tree = commit.tree();
    let fileset_expression = workspace_command.parse_file_patterns(ui, &args.paths)?;
    let matcher = fileset_expression.to_matcher();
    let template: TemplateRenderer<TreeEntry> = {
        let language = workspace_command.commit_template_language();
        let text = match &args.template {
            Some(value) => value.to_owned(),
            None => workspace_command.settings().get("templates.file_list")?,
        };
        workspace_command
            .parse_template(ui, &language, &text)?
            .labeled(["file_list"])
    };

    ui.request_pager();
    let mut formatter = ui.stdout_formatter();
    let mut entries = tree.entries_matching(matcher.as_ref());
    while let Some((path, value)) = entries.next().await {
        let entry = TreeEntry {
            path,
            value: value?,
        };
        template.format(&entry, formatter.as_mut())?;
    }
    print_unmatched_explicit_paths(ui, &workspace_command, &fileset_expression, [&tree]).await?;
    Ok(())
}

async fn cmd_file_list_ignored(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &FileListArgs,
) -> Result<(), CommandError> {
    if args.delete && !command.is_working_copy_writable() {
        return Err(user_error(
            "Cannot delete ignored paths because the working copy is not writable \
             (e.g. --ignore-working-copy was passed)",
        ));
    }
    let (workspace_command, snapshot_stats, _) = command.workspace_helper_with_stats(ui).await?;
    let matcher = workspace_command
        .parse_file_patterns(ui, &args.paths)?
        .to_matcher();
    let ignored_paths: BTreeSet<IgnoredPath> = snapshot_stats
        .ignored_paths
        .iter()
        .filter(|ignored| ignored_path_matches(matcher.as_ref(), ignored))
        .cloned()
        .collect();
    let ignored_paths = collapse_ignored_dirs(workspace_command.workspace_root(), ignored_paths);
    if args.delete {
        if ignored_paths.is_empty() {
            writeln!(ui.status(), "No ignored paths to delete.")?;
            return Ok(());
        }
        let mut formatter = ui.stdout_formatter();
        delete_ignored_paths(&workspace_command, &ignored_paths, formatter.as_mut())?;
    } else {
        ui.request_pager();
        let mut formatter = ui.stdout_formatter();
        for ignored in &ignored_paths {
            writeln!(
                formatter,
                "{}",
                format_ignored_path(&workspace_command, ignored)
            )?;
        }
    }
    Ok(())
}

fn format_ignored_path(
    workspace_command: &WorkspaceCommandHelper,
    ignored: &IgnoredPath,
) -> String {
    let mut ui_path = workspace_command.format_file_path(&ignored.path);
    if ignored.is_dir {
        // Always use '/' regardless of platform, matching jj's other path
        // output (e.g. `jj status`), rather than `std::path::MAIN_SEPARATOR`
        // which would be '\' on Windows.
        ui_path.push('/');
    }
    ui_path
}

fn ignored_path_matches(matcher: &dyn Matcher, ignored: &IgnoredPath) -> bool {
    if ignored.is_dir {
        // The whole directory is removed as a single unit, so it's enough for
        // the fileset to be relevant anywhere under it.
        !matcher.visit(&ignored.path).is_nothing()
    } else {
        matcher.matches(&ignored.path)
    }
}

/// Collapses ignored files into their parent directory when that directory
/// contains nothing else, so that both listing and deletion treat it as a
/// single unit.
///
/// A directory is only collapsed if every entry underneath it is itself in
/// `ignored_paths`, recursively. This guarantees that removing the collapsed
/// directory with a recursive delete doesn't remove anything that wasn't
/// listed: a tracked, untracked-but-not-ignored, or otherwise unrepresented
/// entry keeps the directory from being collapsed.
fn collapse_ignored_dirs(
    workspace_root: &Path,
    ignored_paths: BTreeSet<IgnoredPath>,
) -> BTreeSet<IgnoredPath> {
    // Collapse the shallowest directories first so that the result is as
    // collapsed as possible (e.g. `a/` rather than `a/b/`).
    let candidates: BTreeSet<RepoPathBuf> = ignored_paths
        .iter()
        .filter_map(|ignored| ignored.path.parent())
        .flat_map(|parent| parent.ancestors())
        .filter(|path| !path.is_root())
        .map(ToOwned::to_owned)
        .collect();
    let mut candidates: Vec<_> = candidates.into_iter().collect();
    candidates.sort_by_key(|path| path.components().count());

    let mut collapsed = ignored_paths;
    for dir in candidates {
        if collapsed
            .iter()
            .any(|ignored| ignored.is_dir && dir.starts_with(&ignored.path))
        {
            // Already inside a directory that has been collapsed.
            continue;
        }
        if is_dir_fully_ignored(workspace_root, &collapsed, &dir) {
            collapsed.retain(|ignored| !ignored.path.starts_with(&dir));
            collapsed.insert(IgnoredPath::dir(dir));
        }
    }
    collapsed
}

/// Returns whether every entry under `dir` on disk is in `ignored_paths`.
fn is_dir_fully_ignored(
    workspace_root: &Path,
    ignored_paths: &BTreeSet<IgnoredPath>,
    dir: &RepoPath,
) -> bool {
    let Ok(disk_dir) = dir.to_fs_path(workspace_root) else {
        return false;
    };
    let Ok(entries) = disk_dir.read_dir() else {
        return false;
    };
    let mut has_entry = false;
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        has_entry = true;
        let Ok(file_name) = entry.file_name().into_string() else {
            // A path that isn't valid UTF-8 can't be represented as an ignored
            // path, so it can't have been reported as ignored.
            return false;
        };
        let Ok(name) = RepoPathComponent::new(&file_name) else {
            return false;
        };
        let path = dir.join(name);
        let Ok(file_type) = entry.file_type() else {
            return false;
        };
        if file_type.is_dir() {
            if ignored_paths.contains(&IgnoredPath::dir(path.clone())) {
                continue;
            }
            if !is_dir_fully_ignored(workspace_root, ignored_paths, &path) {
                return false;
            }
        } else if !ignored_paths.contains(&IgnoredPath::file(path)) {
            return false;
        }
    }
    has_entry
}

/// Removes the given ignored paths from the working copy, printing each
/// removed path to the given formatter.
///
/// This is the same set of paths as the one printed by `file list --ignored`,
/// which is why that command can be used as a dry run. A directory is only
/// ever removed when it was listed as a collapsed entry, i.e. when everything
/// it contained was listed too.
fn delete_ignored_paths(
    workspace_command: &WorkspaceCommandHelper,
    ignored_paths: &BTreeSet<IgnoredPath>,
    formatter: &mut dyn Write,
) -> Result<(), CommandError> {
    let workspace_root = workspace_command.workspace_root();
    for ignored in ignored_paths {
        let disk_path = ignored
            .path
            .to_fs_path(workspace_root)
            .map_err(|err| user_error(format!("Invalid path: {err}")))?;
        let Ok(metadata) = disk_path.symlink_metadata() else {
            // The file or directory was already removed (possibly
            // concurrently). Treat it as already deleted.
            continue;
        };
        if ignored.is_dir || metadata.is_dir() {
            std::fs::remove_dir_all(&disk_path).map_err(|err| {
                user_error(format!(
                    "Failed to remove directory {}: {err}",
                    disk_path.display()
                ))
            })?;
        } else {
            std::fs::remove_file(&disk_path).map_err(|err| {
                user_error(format!(
                    "Failed to remove file {}: {err}",
                    disk_path.display()
                ))
            })?;
        }
        writeln!(
            formatter,
            "{}",
            format_ignored_path(workspace_command, ignored)
        )?;
    }
    Ok(())
}
