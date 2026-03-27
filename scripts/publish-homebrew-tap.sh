#!/bin/sh

set -eu

usage() {
  echo "usage: $0 --tap-repo <owner/repo> --token <token> --formula <path> --version <version>" >&2
  exit 1
}

tap_repo=""
token=""
formula=""
version=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --tap-repo)
      [ "$#" -ge 2 ] || usage
      tap_repo="$2"
      shift 2
      ;;
    --token)
      [ "$#" -ge 2 ] || usage
      token="$2"
      shift 2
      ;;
    --formula)
      [ "$#" -ge 2 ] || usage
      formula="$2"
      shift 2
      ;;
    --version)
      [ "$#" -ge 2 ] || usage
      version="$2"
      shift 2
      ;;
    *)
      usage
      ;;
  esac
done

[ -n "$tap_repo" ] || usage
[ -n "$token" ] || usage
[ -n "$formula" ] || usage
[ -n "$version" ] || usage
[ -f "$formula" ] || {
  echo "formula not found: $formula" >&2
  exit 1
}

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

git clone "https://x-access-token:${token}@github.com/${tap_repo}.git" "$workdir/repo"
mkdir -p "$workdir/repo/Formula"
cp "$formula" "$workdir/repo/Formula/shellshelf.rb"

cd "$workdir/repo"

if git diff --quiet --exit-code -- Formula/shellshelf.rb; then
  echo "Homebrew tap already up to date"
  exit 0
fi

git config user.name "github-actions[bot]"
git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
git add Formula/shellshelf.rb
git commit -m "Update shellshelf formula to ${version}"
git push origin HEAD:main
