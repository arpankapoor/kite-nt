#!/bin/bash
#
# Creates a kite-nt release commit and annotated Git tag.

set -Eeuo pipefail

fail() {
  printf 'release: %s\n' "$1" >&2
  exit 1
}

validate_environment() {
  local command
  for command in cargo git; do
    command -v "${command}" > /dev/null ||
      fail "required command not found: ${command}"
  done
  cargo set-version --help > /dev/null 2>&1 ||
    fail "cargo-edit is not installed (required for cargo set-version)"

  local repository_root
  repository_root="$(git rev-parse --show-toplevel 2> /dev/null)" ||
    fail "not inside a Git repository"
  cd "${repository_root}"

  [[ -z "$(git status --porcelain)" ]] || fail "Git worktree must be clean"
  git symbolic-ref --quiet HEAD > /dev/null ||
    fail "HEAD must be attached to a branch"
}

bump_version() {
  local requested_version="$1"

  case "${requested_version}" in
    major | minor | patch)
      cargo set-version --bump "${requested_version}"
      ;;
    *)
      cargo set-version "${requested_version}"
      ;;
  esac
}

get_release_tag() {
  local package_id
  package_id="$(cargo pkgid --package kite-nt)"

  local version="${package_id##*#}"
  [[ "${version}" != "${package_id}" ]] ||
    fail "could not determine the new Cargo version"

  local tag="v${version}"
  if git rev-parse --verify --quiet "refs/tags/${tag}" > /dev/null; then
    fail "tag already exists: ${tag}"
  fi

  printf '%s\n' "${tag}"
}

commit_release() {
  local tag="$1"

  git diff --check
  git add Cargo.toml Cargo.lock
  git diff --quiet || fail "version bump left unexpected unstaged changes"
  [[ -z "$(git ls-files --others --exclude-standard)" ]] ||
    fail "version bump left untracked files"
  git diff --cached --check

  git commit -m "chore(release): bump version to ${tag#v}"
  git tag -a "${tag}" -m "Release ${tag}"
}

print_next_step() {
  local tag="$1"

  printf 'Created release %s. Push it with:\n\n' "${tag}"
  printf '  git push origin HEAD %q\n' "${tag}"
}

main() {
  if (($# > 1)); then
    fail "usage: $0 [major|minor|patch|VERSION]"
  fi

  validate_environment

  local requested_version="${1:-minor}"
  bump_version "${requested_version}"

  local tag
  tag="$(get_release_tag)"
  commit_release "${tag}"
  print_next_step "${tag}"
}

main "$@"
