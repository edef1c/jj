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

use std::collections::HashMap;

use jj_lib::backend::CommitId;
use jj_lib::merge::Merge;
use jj_lib::repo::Repo as _;
use jj_lib::rewrite::CommitRewriter;
use jj_lib::rewrite::RebaseOptions;
use jj_lib::rewrite::RebasedCommit;
use jj_lib::rewrite::duplicate_commits;
use jj_lib::rewrite::merge_commit_trees;
use jj_lib::rewrite::rebase_commit;
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

/// Sets up trunk, lib, and a subtree merge of lib at vendor/lib. Returns
/// (trunk, lib, merge).
fn setup_subtree_merge(
    tx: &mut jj_lib::transaction::Transaction,
    repo: &std::sync::Arc<jj_lib::repo::ReadonlyRepo>,
) -> TestResult<(
    jj_lib::commit::Commit,
    jj_lib::commit::Commit,
    jj_lib::commit::Commit,
)> {
    let trunk_tree = create_tree(repo, &[(repo_path("README"), "trunk\n")]);
    let lib_tree = create_tree(repo, &[(repo_path("lib.rs"), "v1\n")]);
    let trunk = tx
        .repo_mut()
        .new_commit(vec![repo.store().root_commit_id().clone()], trunk_tree)
        .write_unwrap();
    let lib = tx
        .repo_mut()
        .new_commit(vec![repo.store().root_commit_id().clone()], lib_tree)
        .write_unwrap();
    let prefixes = vec![repo_path_buf(""), repo_path_buf("vendor/lib")];
    let merge_tree =
        merge_commit_trees(tx.repo_mut(), &[trunk.clone(), lib.clone()], &prefixes).block_on()?;
    let merge = tx
        .repo_mut()
        .new_commit(vec![trunk.id().clone(), lib.id().clone()], merge_tree)
        .set_subtree_prefixes(prefixes)
        .write_unwrap();
    Ok((trunk, lib, merge))
}

#[test]
fn test_rebase_subtree_merge_onto_newer_lib() -> TestResult {
    let test_repo = TestRepo::init();
    let repo = &test_repo.repo;
    let mut tx = repo.start_transaction();
    let (trunk, lib, merge) = setup_subtree_merge(&mut tx, repo)?;

    // Lib advances upstream; replace the lib parent with the newer commit.
    let lib2_tree = create_tree(repo, &[(repo_path("lib.rs"), "v2\n")]);
    let lib2 = tx
        .repo_mut()
        .new_commit(vec![lib.id().clone()], lib2_tree)
        .write_unwrap();

    let rebased = rebase_commit(
        tx.repo_mut(),
        merge,
        vec![trunk.id().clone(), lib2.id().clone()],
    )
    .block_on()?;
    // The prefix follows the parent position, and the new lib content lands
    // under the prefix.
    assert_eq!(
        rebased.subtree_prefixes(),
        &[repo_path_buf(""), repo_path_buf("vendor/lib")]
    );
    let expected = create_tree(
        repo,
        &[
            (repo_path("README"), "trunk\n"),
            (repo_path("vendor/lib/lib.rs"), "v2\n"),
        ],
    );
    assert_tree_eq!(&rebased.tree(), &expected);
    Ok(())
}

#[test]
fn test_rebase_descendants_preserves_subtree_merge() -> TestResult {
    let test_repo = TestRepo::init();
    let repo = &test_repo.repo;
    let mut tx = repo.start_transaction();
    let (trunk, lib, merge) = setup_subtree_merge(&mut tx, repo)?;

    // Rewriting trunk auto-rebases the subtree merge.
    let trunk2_tree = create_tree(repo, &[(repo_path("README"), "trunk v2\n")]);
    let trunk2 = tx
        .repo_mut()
        .rewrite_commit(&trunk)
        .set_tree(trunk2_tree)
        .write_unwrap();
    let mut rebased: HashMap<CommitId, RebasedCommit> = HashMap::new();
    tx.repo_mut()
        .rebase_descendants_with_options(&RebaseOptions::default(), |old_commit, new_commit| {
            rebased.insert(old_commit.id().clone(), new_commit);
        })
        .block_on()?;
    let RebasedCommit::Rewritten(new_merge) = &rebased[merge.id()] else {
        panic!("subtree merge should not be abandoned");
    };
    assert_eq!(
        new_merge.parent_ids(),
        &[trunk2.id().clone(), lib.id().clone()]
    );
    assert_eq!(
        new_merge.subtree_prefixes(),
        &[repo_path_buf(""), repo_path_buf("vendor/lib")]
    );
    let expected = create_tree(
        repo,
        &[
            (repo_path("README"), "trunk v2\n"),
            (repo_path("vendor/lib/lib.rs"), "v1\n"),
        ],
    );
    assert_tree_eq!(&new_merge.tree(), &expected);
    Ok(())
}

#[test]
fn test_rebase_subtree_merge_parent_count_change_fails() -> TestResult {
    let test_repo = TestRepo::init();
    let repo = &test_repo.repo;
    let mut tx = repo.start_transaction();
    let (trunk, _lib, merge) = setup_subtree_merge(&mut tx, repo)?;

    let result = rebase_commit(tx.repo_mut(), merge, vec![trunk.id().clone()]).block_on();
    let err = result.err().expect("rebase should fail");
    assert!(
        err.to_string().contains("subtree merge"),
        "unexpected error: {err}"
    );
    Ok(())
}

#[test]
fn test_simplify_ancestor_merge_skips_subtree_merge() -> TestResult {
    let test_repo = TestRepo::init();
    let repo = &test_repo.repo;

    // Trunk descends from lib (e.g. the lib history was merged back), and
    // lib is then subtree-merged into trunk: lib is a "redundant" ancestor
    // parent, but it must not be simplified away.
    let lib_tree = create_tree(repo, &[(repo_path("lib.rs"), "v1\n")]);
    let mut tx = repo.start_transaction();
    let lib = tx
        .repo_mut()
        .new_commit(vec![repo.store().root_commit_id().clone()], lib_tree)
        .write_unwrap();
    let trunk_tree = create_tree(repo, &[(repo_path("README"), "trunk\n")]);
    let trunk = tx
        .repo_mut()
        .new_commit(vec![lib.id().clone()], trunk_tree)
        .write_unwrap();

    let prefixes = vec![repo_path_buf(""), repo_path_buf("vendor/lib")];
    let merge_tree =
        merge_commit_trees(tx.repo_mut(), &[trunk.clone(), lib.clone()], &prefixes).block_on()?;
    let merge = tx
        .repo_mut()
        .new_commit(vec![trunk.id().clone(), lib.id().clone()], merge_tree)
        .set_subtree_prefixes(prefixes)
        .write_unwrap();

    let new_parents = vec![trunk.id().clone(), lib.id().clone()];
    let mut rewriter = CommitRewriter::new(tx.repo_mut(), merge, new_parents.clone());
    rewriter.simplify_ancestor_merge()?;
    assert_eq!(rewriter.new_parents(), new_parents);
    Ok(())
}

#[test]
fn test_duplicate_into_subtree() -> TestResult {
    let test_repo = TestRepo::init();
    let repo = &test_repo.repo;
    let mut tx = repo.start_transaction();

    // Trunk already contains lib v1 vendored at the prefix (e.g. from an
    // earlier subtree merge).
    let trunk_tree = create_tree(
        repo,
        &[
            (repo_path("README"), "trunk\n"),
            (repo_path("vendor/lib/lib.rs"), "v1\n"),
        ],
    );
    let trunk = tx
        .repo_mut()
        .new_commit(vec![repo.store().root_commit_id().clone()], trunk_tree)
        .write_unwrap();

    // Upstream lib history: v1, then a commit changing lib.rs and adding a
    // file.
    let lib1_tree = create_tree(repo, &[(repo_path("lib.rs"), "v1\n")]);
    let lib1 = tx
        .repo_mut()
        .new_commit(vec![repo.store().root_commit_id().clone()], lib1_tree)
        .write_unwrap();
    let lib2_tree = create_tree(
        repo,
        &[
            (repo_path("lib.rs"), "v2\n"),
            (repo_path("util.rs"), "util\n"),
        ],
    );
    let lib2 = tx
        .repo_mut()
        .new_commit(vec![lib1.id().clone()], lib2_tree)
        .write_unwrap();

    // Duplicate the upstream commit onto trunk with its changes applied
    // under the prefix (like `git cherry-pick -Xsubtree=vendor/lib`).
    let stats = duplicate_commits(
        tx.repo_mut(),
        &[lib2.id().clone()],
        &HashMap::new(),
        &[trunk.id().clone()],
        &[],
        &SubtreeShift::GraftAt(repo_path_buf("vendor/lib")),
    )
    .block_on()?;
    let duplicated = &stats.duplicated_commits[lib2.id()];
    assert_eq!(duplicated.parent_ids(), &[trunk.id().clone()]);
    // The duplicated commit is a plain commit; no subtree prefixes recorded.
    assert!(duplicated.subtree_prefixes().is_empty());
    let expected = create_tree(
        repo,
        &[
            (repo_path("README"), "trunk\n"),
            (repo_path("vendor/lib/lib.rs"), "v2\n"),
            (repo_path("vendor/lib/util.rs"), "util\n"),
        ],
    );
    assert_tree_eq!(&duplicated.tree(), &expected);
    Ok(())
}

#[test]
fn test_duplicate_extract_from_subtree() -> TestResult {
    let test_repo = TestRepo::init();
    let repo = &test_repo.repo;
    let mut tx = repo.start_transaction();

    // Upstream lib history.
    let lib1_tree = create_tree(repo, &[(repo_path("lib.rs"), "v1\n")]);
    let lib1 = tx
        .repo_mut()
        .new_commit(vec![repo.store().root_commit_id().clone()], lib1_tree)
        .write_unwrap();

    // Trunk contains lib v1 vendored at the prefix; a trunk commit fixes the
    // vendored lib and also touches an unrelated trunk file.
    let trunk_tree = create_tree(
        repo,
        &[
            (repo_path("README"), "trunk\n"),
            (repo_path("vendor/lib/lib.rs"), "v1\n"),
        ],
    );
    let trunk = tx
        .repo_mut()
        .new_commit(vec![repo.store().root_commit_id().clone()], trunk_tree)
        .write_unwrap();
    let trunk2_tree = create_tree(
        repo,
        &[
            (repo_path("README"), "trunk v2\n"),
            (repo_path("vendor/lib/lib.rs"), "v1 with fix\n"),
        ],
    );
    let trunk2 = tx
        .repo_mut()
        .new_commit(vec![trunk.id().clone()], trunk2_tree)
        .write_unwrap();

    // Pick the fix back onto the upstream history: only the changes under
    // the prefix apply, at root-relative paths.
    let stats = duplicate_commits(
        tx.repo_mut(),
        &[trunk2.id().clone()],
        &HashMap::new(),
        &[lib1.id().clone()],
        &[],
        &SubtreeShift::ExtractAt(repo_path_buf("vendor/lib")),
    )
    .block_on()?;
    let duplicated = &stats.duplicated_commits[trunk2.id()];
    assert_eq!(duplicated.parent_ids(), &[lib1.id().clone()]);
    assert!(duplicated.subtree_prefixes().is_empty());
    let expected = create_tree(repo, &[(repo_path("lib.rs"), "v1 with fix\n")]);
    assert_tree_eq!(&duplicated.tree(), &expected);

    // A trunk commit that doesn't touch the prefix extracts to an empty
    // commit.
    let trunk3_tree = create_tree(
        repo,
        &[
            (repo_path("README"), "trunk v3\n"),
            (repo_path("vendor/lib/lib.rs"), "v1 with fix\n"),
        ],
    );
    let trunk3 = tx
        .repo_mut()
        .new_commit(vec![trunk2.id().clone()], trunk3_tree)
        .write_unwrap();
    let stats = duplicate_commits(
        tx.repo_mut(),
        &[trunk3.id().clone()],
        &HashMap::new(),
        &[lib1.id().clone()],
        &[],
        &SubtreeShift::ExtractAt(repo_path_buf("vendor/lib")),
    )
    .block_on()?;
    let duplicated = &stats.duplicated_commits[trunk3.id()];
    // The base (trunk2) and the commit have the same content under the
    // prefix, so the duplicate makes no changes relative to lib1.
    assert_tree_eq!(&duplicated.tree(), &lib1.tree());
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
