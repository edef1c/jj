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

//! Support for subtree merges, where a parent's tree is treated as grafted at
//! a directory prefix.

use std::collections::HashMap;
use std::sync::Arc;

use crate::backend;
use crate::backend::BackendResult;
use crate::backend::TreeId;
use crate::backend::TreeValue;
use crate::merge::Merge;
use crate::merged_tree::MergedTree;
use crate::repo_path::RepoPath;
use crate::repo_path::RepoPathBuf;
use crate::store::Store;

/// Returns tree ids in which each term of `tree_ids` appears grafted under
/// `prefix`, by wrapping it in synthetic single-entry ancestor trees.
///
/// Returns the input unchanged if `prefix` is the root. An empty tree grafts
/// to an empty tree, so no empty intermediate directories are created. The
/// shape of the merge is preserved, so conflict labels associated with the
/// input remain valid for the output.
pub async fn graft_tree_at_prefix(
    store: &Arc<Store>,
    tree_ids: &Merge<TreeId>,
    prefix: &RepoPath,
) -> BackendResult<Merge<TreeId>> {
    if prefix.is_root() {
        return Ok(tree_ids.clone());
    }
    // The terms of a merge often repeat, and grafting is pure, so memoize by
    // tree id.
    let mut memo: HashMap<&TreeId, TreeId> = HashMap::new();
    let mut grafted_terms = Vec::with_capacity(tree_ids.iter().len());
    for term in tree_ids.iter() {
        let grafted = match memo.get(term) {
            Some(id) => id.clone(),
            None => {
                let id = graft_single_tree_at_prefix(store, term, prefix).await?;
                memo.insert(term, id.clone());
                id
            }
        };
        grafted_terms.push(grafted);
    }
    Ok(Merge::from_vec(grafted_terms))
}

/// Returns `tree` grafted under `prefix`, with its conflict labels preserved.
/// Returns the tree unchanged if `prefix` is the root.
pub async fn graft_merged_tree(tree: &MergedTree, prefix: &RepoPath) -> BackendResult<MergedTree> {
    if prefix.is_root() {
        return Ok(tree.clone());
    }
    let store = tree.store().clone();
    let (tree_ids, labels) = tree.clone().into_tree_ids_and_labels();
    let grafted = graft_tree_at_prefix(&store, &tree_ids, prefix).await?;
    Ok(MergedTree::new(store, grafted, labels))
}

async fn graft_single_tree_at_prefix(
    store: &Arc<Store>,
    tree_id: &TreeId,
    prefix: &RepoPath,
) -> BackendResult<TreeId> {
    if tree_id == store.empty_tree_id() {
        return Ok(tree_id.clone());
    }
    let mut tree_id = tree_id.clone();
    let mut dir = prefix;
    while let Some((parent, basename)) = dir.split() {
        let tree = backend::Tree::from_sorted_entries(vec![(
            basename.to_owned(),
            TreeValue::Tree(tree_id),
        )]);
        tree_id = store.write_tree(parent, tree).await?.id().clone();
        dir = parent;
    }
    Ok(tree_id)
}

/// Returns tree ids in which each term of `tree_ids` is replaced by its
/// subtree at `prefix`, or by the empty tree if there is no directory at
/// that path. This is the inverse of [`graft_tree_at_prefix()`]. The shape
/// of the merge is preserved, so conflict labels associated with the input
/// remain valid for the output.
pub async fn extract_tree_at_prefix(
    store: &Arc<Store>,
    tree_ids: &Merge<TreeId>,
    prefix: &RepoPath,
) -> BackendResult<Merge<TreeId>> {
    if prefix.is_root() {
        return Ok(tree_ids.clone());
    }
    // The terms of a merge often repeat, and extraction is pure, so memoize
    // by tree id.
    let mut memo: HashMap<&TreeId, TreeId> = HashMap::new();
    let mut extracted_terms = Vec::with_capacity(tree_ids.iter().len());
    for term in tree_ids.iter() {
        let extracted = match memo.get(term) {
            Some(id) => id.clone(),
            None => {
                let tree = store.get_tree(RepoPathBuf::root(), term).await?;
                let id = match tree.sub_tree_recursive(prefix).await? {
                    Some(sub_tree) => sub_tree.id().clone(),
                    None => store.empty_tree_id().clone(),
                };
                memo.insert(term, id.clone());
                id
            }
        };
        extracted_terms.push(extracted);
    }
    Ok(Merge::from_vec(extracted_terms))
}

/// Returns the subtree of `tree` at `prefix` (the empty tree if there is no
/// directory at that path), with its conflict labels preserved. Returns the
/// tree unchanged if `prefix` is the root.
pub async fn extract_merged_tree(
    tree: &MergedTree,
    prefix: &RepoPath,
) -> BackendResult<MergedTree> {
    if prefix.is_root() {
        return Ok(tree.clone());
    }
    let store = tree.store().clone();
    let (tree_ids, labels) = tree.clone().into_tree_ids_and_labels();
    let extracted = extract_tree_at_prefix(&store, &tree_ids, prefix).await?;
    Ok(MergedTree::new(store, extracted, labels))
}

/// How to reinterpret a commit's tree relative to a subtree prefix when its
/// changes are applied elsewhere (e.g. when duplicating commits).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum SubtreeShift {
    /// Use the tree as-is.
    #[default]
    None,
    /// Graft the tree at the prefix, so that changes from a standalone
    /// history apply under the prefix (like `git cherry-pick -Xsubtree`).
    GraftAt(RepoPathBuf),
    /// Extract the subtree at the prefix, so that changes made under the
    /// prefix apply to a standalone history (e.g. to pick changes to a
    /// vendored copy back onto its upstream).
    ExtractAt(RepoPathBuf),
}

impl SubtreeShift {
    /// Returns true if this shift leaves trees unchanged.
    pub fn is_none(&self) -> bool {
        match self {
            Self::None => true,
            Self::GraftAt(prefix) | Self::ExtractAt(prefix) => prefix.is_root(),
        }
    }

    /// Applies the shift to a merged tree, preserving conflict labels.
    pub async fn apply(&self, tree: &MergedTree) -> BackendResult<MergedTree> {
        match self {
            Self::None => Ok(tree.clone()),
            Self::GraftAt(prefix) => graft_merged_tree(tree, prefix).await,
            Self::ExtractAt(prefix) => extract_merged_tree(tree, prefix).await,
        }
    }

    /// Maps a path in the original tree to its location in the shifted tree,
    /// or `None` if the path is not present in the shifted view.
    pub fn apply_to_path(&self, path: &RepoPath) -> Option<RepoPathBuf> {
        match self {
            Self::None => Some(path.to_owned()),
            Self::GraftAt(prefix) => {
                let mut shifted = prefix.clone();
                shifted.extend(path.components());
                Some(shifted)
            }
            Self::ExtractAt(prefix) => path.strip_prefix(prefix).map(|tail| tail.to_owned()),
        }
    }

    /// A suffix describing the shift, for use in conflict labels.
    pub fn conflict_label_suffix(&self) -> String {
        match self {
            Self::None => String::new(),
            Self::GraftAt(prefix) => {
                format!(" (subtree {})", prefix.as_internal_file_string())
            }
            Self::ExtractAt(prefix) => {
                format!(" (extracted from {})", prefix.as_internal_file_string())
            }
        }
    }
}
