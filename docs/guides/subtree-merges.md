# Subtree merges (experimental)

A *subtree merge* merges another history into your repository such that its
files live under a directory prefix, similar to Git's `-s subtree` merge
strategy and the `git subtree` tool. A typical use is vendoring a library at
`vendor/lib/` while keeping the ability to pull its upstream changes later.

This feature is experimental. To enable creating subtree merges, set:

```toml
[experimental]
subtree-merge = true
```

## Creating a subtree merge

`jj new` takes a repeatable `--subtree PATH=REVSET` option that adds the
resolved revision as an additional parent of the new change and records that
its tree is grafted at `PATH`:

```shell
# Make the lib history available in your repo, e.g.:
$ jj git fetch --remote lib-upstream

# Merge it into trunk, placing its files under vendor/lib/:
$ jj new trunk --subtree vendor/lib=lib@lib-upstream
```

The new commit is a regular merge commit whose tree contains `trunk`'s files
at their usual locations and the library's files under `vendor/lib/`. The
prefix is recorded with the commit, so:

* `jj diff`, `jj log -p`, and `jj status` show only the changes you actually
  make in the commit, not a spurious whole-tree move.
* Rebasing the merge (including jj's automatic rebasing of descendants)
  recomputes its tree with the graft applied. Replacing the library parent
  with a newer library commit grafts the new version at the same prefix.

## Pulling upstream changes

To update the vendored copy later, create another subtree merge with the
newer upstream commit:

```shell
$ jj git fetch --remote lib-upstream
$ jj new trunk-head --subtree vendor/lib=lib@lib-upstream
```

jj finds the previously merged library commit as the merge base and grafts it
at the prefix, so upstream changes (including renames within the library)
apply cleanly under `vendor/lib/`, and your local modifications to the
vendored files are merged with the upstream changes like in any other merge.

## Picking individual commits

To apply a single upstream commit to the vendored copy without merging the
whole history (the equivalent of `git cherry-pick -Xsubtree=PATH`), use
`jj duplicate` with `--subtree`:

```shell
$ jj duplicate <upstream-commit> --onto trunk-head --subtree vendor/lib
```

The duplicated commit's changes are applied under `vendor/lib/`. It is a
plain commit (no subtree prefix is recorded on it), since its parent is the
destination rather than the upstream history.

The inverse direction, `--from-subtree`, extracts the changes a commit made
under the prefix and applies them relative to the root of the destination.
This is how you contribute a fix made to the vendored copy back to the
upstream history:

```shell
$ jj duplicate <trunk-commit> --onto lib@lib-upstream --from-subtree vendor/lib
```

Only the commit's changes under `vendor/lib/` are applied; changes to other
trunk files are dropped from the duplicate.

## Git interoperability

The prefix is stored in a `jj:subtree-prefixes` extra header of the Git
commit object, so it survives `jj git push`, fetch, and clone between jj
users. Plain Git sees a normal merge commit and ignores the header.

Caveats:

* Git tools won't apply the subtree strategy when *they* merge or rebase the
  commits; only jj interprets the recorded prefix.
* jj versions without this feature preserve the commit (and its tree) but
  drop the recorded prefix if they *rewrite* the commit; rebases of such a
  commit then treat it as a regular merge.

## Limitations

* Changing the number of parents of a subtree merge (e.g. `jj rebase` onto a
  single destination, or `jj simplify-parents`) is rejected; recreate the
  merge with `jj new --subtree` instead.
* Extracting a subdirectory's entire history as standalone commits (the
  equivalent of `git subtree split`) is not implemented; individual commits
  can be picked back upstream with `jj duplicate --from-subtree`.
