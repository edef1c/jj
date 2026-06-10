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
use jj_lib::subtree::SubtreeShift;
use jj_lib::subtree::graft_tree_at_prefix;
use pollster::FutureExt as _;
use testutils::TestRepo;
use testutils::TestResult;
use testutils::create_single_tree;
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
