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

use jj_lib::merge::Merge;
use jj_lib::repo::Repo as _;
use jj_lib::rewrite::merge_commit_trees;
use jj_lib::subtree::SubtreeShift;
use jj_lib::subtree::graft_tree_at_prefix;
use pollster::FutureExt as _;
use testutils::CommitBuilderExt as _;
use testutils::TestRepo;
use testutils::TestResult;
use testutils::assert_tree_eq;
use testutils::create_single_tree;
use testutils::create_tree;
use testutils::repo_path;
use testutils::repo_path_buf;

#[test]
fn test_graft_resolved_tree() -> TestResult {
    let test_repo = TestRepo::init();
    let repo = &test_repo.repo;
    let store = repo.store();

    let tree = create_single_tree(
        repo,
        &[(repo_path("a/b.txt"), "b"), (repo_path("top.txt"), "t")],
    );
    let expected = create_single_tree(
        repo,
        &[
            (repo_path("vendor/lib/a/b.txt"), "b"),
            (repo_path("vendor/lib/top.txt"), "t"),
        ],
    );

    let grafted = graft_tree_at_prefix(
        store,
        &Merge::resolved(tree.id().clone()),
        repo_path("vendor/lib"),
    )
    .block_on()?;
    assert_eq!(grafted, Merge::resolved(expected.id().clone()));
    Ok(())
}

#[test]
fn test_graft_at_root_is_noop() -> TestResult {
    let test_repo = TestRepo::init();
    let repo = &test_repo.repo;
    let store = repo.store();

    let tree = create_single_tree(repo, &[(repo_path("a.txt"), "a")]);
    let tree_ids = Merge::resolved(tree.id().clone());
    let grafted = graft_tree_at_prefix(store, &tree_ids, repo_path("")).block_on()?;
    assert_eq!(grafted, tree_ids);
    Ok(())
}

#[test]
fn test_graft_empty_tree() -> TestResult {
    let test_repo = TestRepo::init();
    let repo = &test_repo.repo;
    let store = repo.store();

    // No empty intermediate directories should be created.
    let tree_ids = Merge::resolved(store.empty_tree_id().clone());
    let grafted = graft_tree_at_prefix(store, &tree_ids, repo_path("vendor/lib")).block_on()?;
    assert_eq!(grafted, tree_ids);
    Ok(())
}

#[test]
fn test_graft_deep_prefix() -> TestResult {
    let test_repo = TestRepo::init();
    let repo = &test_repo.repo;
    let store = repo.store();

    let tree = create_single_tree(repo, &[(repo_path("f"), "f")]);
    let expected = create_single_tree(repo, &[(repo_path("a/b/c/f"), "f")]);

    let grafted = graft_tree_at_prefix(
        store,
        &Merge::resolved(tree.id().clone()),
        repo_path("a/b/c"),
    )
    .block_on()?;
    assert_eq!(grafted, Merge::resolved(expected.id().clone()));
    Ok(())
}

#[test]
fn test_graft_conflicted_tree() -> TestResult {
    let test_repo = TestRepo::init();
    let repo = &test_repo.repo;
    let store = repo.store();

    let path = repo_path("file");
    let base = create_single_tree(repo, &[(path, "base")]);
    let left = create_single_tree(repo, &[(path, "left")]);
    let right = create_single_tree(repo, &[(path, "right")]);
    let expected_base = create_single_tree(repo, &[(repo_path("sub/file"), "base")]);
    let expected_left = create_single_tree(repo, &[(repo_path("sub/file"), "left")]);
    let expected_right = create_single_tree(repo, &[(repo_path("sub/file"), "right")]);

    let tree_ids = Merge::from_vec(vec![
        left.id().clone(),
        base.id().clone(),
        right.id().clone(),
    ]);
    let grafted = graft_tree_at_prefix(store, &tree_ids, repo_path("sub")).block_on()?;
    assert_eq!(
        grafted,
        Merge::from_vec(vec![
            expected_left.id().clone(),
            expected_base.id().clone(),
            expected_right.id().clone(),
        ])
    );
    Ok(())
}

#[test]
fn test_subtree_merge_parent_tree() -> TestResult {
    let test_repo = TestRepo::init();
    let repo = &test_repo.repo;

    // Trunk and lib are unrelated histories.
    let trunk_tree = create_tree(
        repo,
        &[
            (repo_path("README"), "trunk\n"),
            (repo_path("src/main.rs"), "fn main() {}\n"),
        ],
    );
    let lib_tree = create_tree(
        repo,
        &[
            (repo_path("README"), "lib\n"),
            (repo_path("lib.rs"), "pub fn f() {}\n"),
        ],
    );

    let mut tx = repo.start_transaction();
    let trunk = tx
        .repo_mut()
        .new_commit(vec![repo.store().root_commit_id().clone()], trunk_tree)
        .write_unwrap();
    let lib = tx
        .repo_mut()
        .new_commit(vec![repo.store().root_commit_id().clone()], lib_tree)
        .write_unwrap();

    let prefixes = vec![repo_path_buf(""), repo_path_buf("vendor/lib")];
    let merged_tree =
        merge_commit_trees(tx.repo_mut(), &[trunk.clone(), lib.clone()], &prefixes).block_on()?;
    // The lib files appear under the prefix; the colliding root README
    // doesn't conflict because the lib tree is grafted.
    let expected = create_tree(
        repo,
        &[
            (repo_path("README"), "trunk\n"),
            (repo_path("src/main.rs"), "fn main() {}\n"),
            (repo_path("vendor/lib/README"), "lib\n"),
            (repo_path("vendor/lib/lib.rs"), "pub fn f() {}\n"),
        ],
    );
    assert_tree_eq!(&merged_tree, &expected);

    // A merge commit recording the prefixes reports the same parent tree and
    // is considered empty.
    let merge_commit = tx
        .repo_mut()
        .new_commit(
            vec![trunk.id().clone(), lib.id().clone()],
            merged_tree.clone(),
        )
        .set_subtree_prefixes(prefixes)
        .write_unwrap();
    assert_eq!(
        merge_commit.subtree_prefixes(),
        &[repo_path_buf(""), repo_path_buf("vendor/lib")]
    );
    let parent_tree = merge_commit.parent_tree(tx.repo_mut()).block_on()?;
    assert_tree_eq!(&parent_tree, &expected);
    assert!(merge_commit.is_empty(tx.repo_mut()).block_on()?);
    Ok(())
}

#[test]
fn test_subtree_merge_single_parent() -> TestResult {
    let test_repo = TestRepo::init();
    let repo = &test_repo.repo;

    let lib_tree = create_tree(repo, &[(repo_path("lib.rs"), "lib\n")]);
    let mut tx = repo.start_transaction();
    let lib = tx
        .repo_mut()
        .new_commit(vec![repo.store().root_commit_id().clone()], lib_tree)
        .write_unwrap();

    let merged_tree =
        merge_commit_trees(tx.repo_mut(), &[lib], &[repo_path_buf("sub")]).block_on()?;
    let expected = create_tree(repo, &[(repo_path("sub/lib.rs"), "lib\n")]);
    assert_tree_eq!(&merged_tree, &expected);
    Ok(())
}

#[test]
fn test_subtree_merge_second_pull() -> TestResult {
    let test_repo = TestRepo::init();
    let repo = &test_repo.repo;

    let trunk_tree = create_tree(repo, &[(repo_path("README"), "trunk\n")]);
    let lib1_tree = create_tree(
        repo,
        &[
            (repo_path("lib.rs"), "v1\n"),
            (repo_path("old-name.rs"), "moves\n"),
        ],
    );

    let mut tx = repo.start_transaction();
    let trunk = tx
        .repo_mut()
        .new_commit(vec![repo.store().root_commit_id().clone()], trunk_tree)
        .write_unwrap();
    let lib1 = tx
        .repo_mut()
        .new_commit(vec![repo.store().root_commit_id().clone()], lib1_tree)
        .write_unwrap();

    let prefixes = vec![repo_path_buf(""), repo_path_buf("vendor/lib")];
    let merge1_tree =
        merge_commit_trees(tx.repo_mut(), &[trunk.clone(), lib1.clone()], &prefixes).block_on()?;
    let merge1 = tx
        .repo_mut()
        .new_commit(
            vec![trunk.id().clone(), lib1.id().clone()],
            merge1_tree.clone(),
        )
        .set_subtree_prefixes(prefixes.clone())
        .write_unwrap();

    // Trunk advances on top of the merge.
    let trunk2_tree = create_tree(
        repo,
        &[
            (repo_path("README"), "trunk v2\n"),
            (repo_path("vendor/lib/lib.rs"), "v1\n"),
            (repo_path("vendor/lib/old-name.rs"), "moves\n"),
        ],
    );
    let trunk2 = tx
        .repo_mut()
        .new_commit(vec![merge1.id().clone()], trunk2_tree)
        .write_unwrap();

    // Lib advances upstream: modifies a file and renames another.
    let lib2_tree = create_tree(
        repo,
        &[
            (repo_path("lib.rs"), "v2\n"),
            (repo_path("new-name.rs"), "moves\n"),
        ],
    );
    let lib2 = tx
        .repo_mut()
        .new_commit(vec![lib1.id().clone()], lib2_tree)
        .write_unwrap();

    // Pulling the new lib version into trunk merges cleanly: the merge base
    // is lib1 grafted at the prefix.
    let merge2_tree =
        merge_commit_trees(tx.repo_mut(), &[trunk2.clone(), lib2.clone()], &prefixes).block_on()?;
    let expected = create_tree(
        repo,
        &[
            (repo_path("README"), "trunk v2\n"),
            (repo_path("vendor/lib/lib.rs"), "v2\n"),
            (repo_path("vendor/lib/new-name.rs"), "moves\n"),
        ],
    );
    assert_tree_eq!(&merge2_tree, &expected);
    Ok(())
}

#[test]
fn test_subtree_merge_prefix_collides_with_file() -> TestResult {
    let test_repo = TestRepo::init();
    let repo = &test_repo.repo;

    // Trunk has a *file* at the path the lib tree is grafted at.
    let trunk_tree = create_tree(repo, &[(repo_path("vendor/lib"), "file in the way\n")]);
    let lib_tree = create_tree(repo, &[(repo_path("lib.rs"), "lib\n")]);

    let mut tx = repo.start_transaction();
    let trunk = tx
        .repo_mut()
        .new_commit(vec![repo.store().root_commit_id().clone()], trunk_tree)
        .write_unwrap();
    let lib = tx
        .repo_mut()
        .new_commit(vec![repo.store().root_commit_id().clone()], lib_tree)
        .write_unwrap();

    let prefixes = vec![repo_path_buf(""), repo_path_buf("vendor/lib")];
    let merged_tree = merge_commit_trees(tx.repo_mut(), &[trunk, lib], &prefixes).block_on()?;
    // The file-vs-directory clash surfaces as a conflict.
    assert!(!merged_tree.tree_ids().is_resolved());
    Ok(())
}

#[test]
fn test_graft_repeated_terms() -> TestResult {
    let test_repo = TestRepo::init();
    let repo = &test_repo.repo;
    let store = repo.store();

    let path = repo_path("file");
    let side = create_single_tree(repo, &[(path, "side")]);
    let base = create_single_tree(repo, &[(path, "base")]);

    // The same term appears twice (e.g. in a criss-cross merge); the grafted
    // ids must be identical.
    let tree_ids = Merge::from_vec(vec![
        side.id().clone(),
        base.id().clone(),
        side.id().clone(),
    ]);
    let grafted = graft_tree_at_prefix(store, &tree_ids, repo_path("sub")).block_on()?;
    let terms: Vec<_> = grafted.iter().collect();
    assert_eq!(terms[0], terms[2]);
    assert_ne!(terms[0], terms[1]);
    Ok(())
}
#[test]
fn test_subtree_shift_apply_to_path() {
    let prefix = repo_path_buf("vendor/lib");
    let path = repo_path("src/foo.rs");
    assert_eq!(
        SubtreeShift::None.apply_to_path(path),
        Some(path.to_owned())
    );
    assert_eq!(
        SubtreeShift::GraftAt(prefix.clone()).apply_to_path(path),
        Some(repo_path_buf("vendor/lib/src/foo.rs"))
    );
    assert_eq!(
        SubtreeShift::ExtractAt(prefix.clone()).apply_to_path(repo_path("vendor/lib/src/foo.rs")),
        Some(repo_path_buf("src/foo.rs"))
    );
    assert_eq!(
        SubtreeShift::ExtractAt(prefix.clone()).apply_to_path(repo_path("other/foo.rs")),
        None
    );
    // A sibling directory sharing a string prefix must not match.
    assert_eq!(
        SubtreeShift::ExtractAt(prefix).apply_to_path(repo_path("vendor/library/foo.rs")),
        None
    );
}
