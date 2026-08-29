// Copyright 2026 The Jujutsu Authors
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

use insta::assert_snapshot;

use crate::common::TestEnvironment;

#[test]
fn test_file_list_ignored() {
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    work_dir.write_file(".gitignore", "*.ignored\nignored-root/\n");
    // Untracked files ignored by .gitignore patterns.
    work_dir.write_file("ignored-root/only-ignored/file.txt", "content");
    work_dir.write_file("root-ignored.ignored", "content");

    // Fully ignored directories are listed as a single collapsed entry (with
    // trailing separator), and files matching an ignore pattern are listed
    // individually.
    let output = work_dir.run_jj(["file", "list", "--ignored"]);
    assert_snapshot!(output, @"
    ignored-root/
    root-ignored.ignored
    [EOF]
    ");
}

#[test]
fn test_file_list_ignored_empty_directory() {
    // Regression test: an empty directory that matches a directory-level
    // `.gitignore` pattern should still be listed, even though it has no
    // content to recurse into.
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    work_dir.write_file(".gitignore", "empty-ignored/\n");
    work_dir.create_dir("empty-ignored");

    let output = work_dir.run_jj(["file", "list", "--ignored"]);
    assert_snapshot!(output, @"
    empty-ignored/
    [EOF]
    ");
}

#[test]
fn test_file_list_ignored_tracked_files_not_listed() {
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    work_dir.write_file(".gitignore", "*.ignored\n");
    work_dir.write_file("tracked.ignored", "content");
    work_dir
        .run_jj(["file", "track", "--include-ignored", "tracked.ignored"])
        .success();
    work_dir.run_jj(["commit", "-m", "setup"]).success();

    // A tracked file that matches the ignore patterns is not listed.
    work_dir.write_file("untracked.ignored", "content");
    let output = work_dir.run_jj(["file", "list", "--ignored"]);
    assert_snapshot!(output, @"
    untracked.ignored
    [EOF]
    ");
}

#[test]
fn test_file_list_ignored_delete() {
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    work_dir.write_file(".gitignore", "*.ignored\nignored-root/\n");
    work_dir.write_file("tracked-file.txt", "content");
    work_dir.write_file("ignored-root/only-ignored/file.txt", "content");
    work_dir
        .run_jj(["file", "track", "tracked-file.txt"])
        .success();
    work_dir.run_jj(["commit", "-m", "setup"]).success();

    // Add more ignored files after the commit.
    work_dir.write_file("root-ignored.ignored", "content");
    work_dir.write_file("ignored-root/late.ignored", "content");

    // Deletion removes exactly the listed ignored paths from the working copy,
    // and prints them to stdout just like `file list --ignored` does. Tracked
    // files and .gitignore are left intact. `ignored-root` has no tracked files
    // underneath, so it's removed as a single collapsed unit.
    let output = work_dir.run_jj(["file", "list", "--ignored", "--delete"]);
    assert_snapshot!(output, @"
    ignored-root/
    root-ignored.ignored
    [EOF]
    ");
    assert!(!work_dir.root().join("root-ignored.ignored").exists());
    assert!(!work_dir.root().join("ignored-root").exists());
    assert!(!work_dir.root().join("ignored-root/only-ignored").exists());
    assert!(!work_dir.root().join("ignored-root/late.ignored").exists());
    assert!(work_dir.root().join("tracked-file.txt").exists());
    assert!(work_dir.root().join(".gitignore").exists());

    // A second run has nothing left to remove.
    let output2 = work_dir.run_jj(["file", "list", "--ignored", "--delete"]);
    assert_snapshot!(output2, @"
    ------- stderr -------
    No ignored paths to delete.
    [EOF]
    ");
}

#[test]
fn test_file_list_ignored_collapses_directory_with_only_ignored_files() {
    // A directory that has no dedicated `.gitignore` pattern of its own, but
    // whose whole content is ignored (here via a `*.log` pattern), is listed
    // as a single collapsed entry. `--delete` then removes the directory
    // recursively, exactly as listed.
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    work_dir.write_file(".gitignore", "*.log\n");
    work_dir.write_file("tracked-file.txt", "content");
    work_dir
        .run_jj(["file", "track", "tracked-file.txt"])
        .success();
    work_dir.run_jj(["commit", "-m", "setup"]).success();

    // `logs/` itself doesn't match any ignore pattern, only the files inside
    // it do.
    work_dir.write_file("logs/one.log", "content");
    work_dir.write_file("logs/two.log", "content");

    let list_output = work_dir.run_jj(["file", "list", "--ignored"]);
    assert_snapshot!(list_output, @"
    logs/
    [EOF]
    ");

    let output = work_dir.run_jj(["file", "list", "--ignored", "--delete"]);
    assert_snapshot!(output, @"
    logs/
    [EOF]
    ");
    assert!(!work_dir.root().join("logs").exists());
    assert!(work_dir.root().join("tracked-file.txt").exists());
}

#[test]
fn test_file_list_ignored_collapses_nested_directories() {
    // Collapsing is recursive: if `a/b/` only contains ignored files, then
    // `a/` itself only contains ignored content and is listed instead.
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    work_dir.write_file(".gitignore", "*.log\n");
    work_dir.write_file("a/b/c.log", "content");

    let output = work_dir.run_jj(["file", "list", "--ignored"]);
    assert_snapshot!(output, @"
    a/
    [EOF]
    ");
}

#[test]
fn test_file_list_ignored_does_not_collapse_directory_containing_nested_repo() {
    // A directory that contains a nested repository must not be collapsed (and
    // therefore never removed recursively), because the nested repository's
    // contents are not reported as ignored paths.
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    work_dir.write_file(".gitignore", "*.log\n");
    work_dir.write_file("logs/one.log", "content");
    work_dir.write_file("logs/nested/.git/config", "content");

    let output = work_dir.run_jj(["file", "list", "--ignored"]);
    assert_snapshot!(output, @"
    logs/one.log
    [EOF]
    ");

    let output = work_dir.run_jj(["file", "list", "--ignored", "--delete"]);
    assert_snapshot!(output, @"
    logs/one.log
    [EOF]
    ");
    assert!(!work_dir.root().join("logs/one.log").exists());
    assert!(work_dir.root().join("logs/nested/.git/config").exists());
}

#[test]
fn test_file_list_ignored_delete_removes_empty_directory() {
    // Regression test: an empty directory that matches a directory-level
    // `.gitignore` pattern is itself ignored, so it should be removed by
    // `file list --ignored --delete`, exactly like it is listed by
    // `file list --ignored`.
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    work_dir.write_file(".gitignore", "empty-ignored/\n");
    work_dir.create_dir("empty-ignored");

    let output = work_dir.run_jj(["file", "list", "--ignored", "--delete"]);
    assert_snapshot!(output, @"
    empty-ignored/
    [EOF]
    ");
    assert!(!work_dir.root().join("empty-ignored").exists());
}

#[test]
fn test_file_list_ignored_delete_does_not_touch_unrelated_file_in_same_directory() {
    // Safety test: a directory that isn't itself covered by a `.gitignore`
    // pattern, and that contains both an ignored file and an unrelated file
    // that isn't ignored, must only have the ignored file removed. The
    // directory itself must be left behind, since it still has content.
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    work_dir.write_file(".gitignore", "*.log\n");
    work_dir.write_file("logs/one.log", "content");
    work_dir.write_file("logs/keep.txt", "content");

    // `logs/` still contains a non-ignored file, so it isn't collapsed.
    let list_output = work_dir.run_jj(["file", "list", "--ignored"]);
    assert_snapshot!(list_output, @"
    logs/one.log
    [EOF]
    ");

    let output = work_dir.run_jj(["file", "list", "--ignored", "--delete"]);
    assert_snapshot!(output, @"
    logs/one.log
    [EOF]
    ");
    assert!(!work_dir.root().join("logs/one.log").exists());
    assert!(work_dir.root().join("logs/keep.txt").exists());
    assert!(work_dir.root().join("logs").exists());
}

#[test]
fn test_file_list_ignored_delete_scoped_to_path() {
    // Passing a path restricts both `file list --ignored` and `file list --ignored --delete` to
    // that subset, leaving other ignored content (e.g. a build directory
    // you don't want touched yet) completely untouched. Running
    // `file list --ignored` with the same path first lets you preview what
    // `file list --ignored --delete` would do, without a separate `--dry-run` flag.
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    work_dir.write_file(".gitignore", "/target/\n/rendered-docs\n");
    work_dir.write_file("target/some-build-artifact.bin", "content");
    work_dir.create_dir("rendered-docs");

    let list_output = work_dir.run_jj(["file", "list", "--ignored", "rendered-docs"]);
    assert_snapshot!(list_output, @"
    rendered-docs/
    [EOF]
    ");

    let output = work_dir.run_jj(["file", "list", "--ignored", "--delete", "rendered-docs"]);
    assert_snapshot!(output, @"
    rendered-docs/
    [EOF]
    ");
    assert!(!work_dir.root().join("rendered-docs").exists());
    // `target/` was never mentioned, so it's left completely alone.
    assert!(
        work_dir
            .root()
            .join("target/some-build-artifact.bin")
            .exists()
    );
}

#[test]
fn test_file_list_ignored_conflicts_with_revision() {
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    let output = work_dir.run_jj(["file", "list", "--ignored", "-r", "@"]);
    assert!(!output.status.success());
}

#[test]
fn test_file_list_ignored_conflicts_with_template() {
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    let output = work_dir.run_jj(["file", "list", "--ignored", "-T", "path"]);
    assert!(!output.status.success());
}

#[test]
fn test_file_list_delete_requires_ignored() {
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    let output = work_dir.run_jj(["file", "list", "--delete"]);
    assert!(!output.status.success());
}

#[test]
fn test_file_list_ignored_delete_requires_writable_working_copy() {
    // Deletion is based on the snapshot of the working copy, so it can't work
    // when snapshotting is disabled.
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    let output = work_dir.run_jj([
        "--ignore-working-copy",
        "file",
        "list",
        "--ignored",
        "--delete",
    ]);
    assert_snapshot!(output, @"
    ------- stderr -------
    Error: Cannot delete ignored paths because the working copy is not writable (e.g. --ignore-working-copy was passed)
    [EOF]
    [exit status: 1]
    ");
    assert!(!output.status.success());
}
