#!/usr/bin/env bash
# The boundrail GitHub Action (action.yml). Every input arrives as an INPUT_* environment
# variable, so this runs outside Actions too, given RUNNER_TEMP and GITHUB_STEP_SUMMARY:
#
#   INPUT_PATH INPUT_FAIL_ON INPUT_VERSION INPUT_BINARY INPUT_BASE_REF INPUT_TOKEN   as in action.yml
#   PR_NUMBER   the pull request's number, on pull_request events
#
# Runs `model check --format annotations` and replays its annotations. With a base ref, writes
# `model footprint --format pr-comment` to the job summary and, on a pull_request event with a
# token, posts it as one comment, replacing the comment the same account posted before. It never
# passes --language-servers. Exit 1 when the check finds what fail-on names, 2 on errors.
set -euo pipefail
# A failed setup step is a failure to run, not a gate result.
trap 'exit 2' ERR

# Only the posting step sees the token.
gh_token=${INPUT_TOKEN:-}
unset INPUT_TOKEN

# Everything printed below may hold text from the repository, which must not run as workflow
# commands: one stream, commands off except while the check's own annotations replay.
exec 2>&1
stop() {
  token=$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')
  echo "::stop-commands::$token"
}
resume() {
  echo "::$token::"
}
stop
failure=
# shellcheck disable=SC2317,SC2329 # run by the EXIT trap
cleanup() {
  local status=$?
  resume
  if ((status >= 2)); then
    echo "::error::boundrail: ${failure:-failed with exit $status; see the log above}"
  fi
}
trap cleanup EXIT
die() {
  failure="$*"
  exit 2
}

here=$(cd "$(dirname "$0")/.." && pwd)
: "${RUNNER_TEMP:?}" "${GITHUB_STEP_SUMMARY:?}"

path=${INPUT_PATH:-.}
[[ -d $path ]] || die "path $path is not a directory"
fail_on=${INPUT_FAIL_ON-warnings}
# An empty fail-on fails on nothing.
fail_on=${fail_on:-never}
case $fail_on in
  warnings | information | never) ;;
  *) die "fail-on must be warnings, information or never" ;;
esac
base_ref=${INPUT_BASE_REF:-}
work=$RUNNER_TEMP/boundrail-model
rm -rf "$work"
mkdir -p "$work"

if [[ -n ${INPUT_BINARY:-} ]]; then
  boundrail=$INPUT_BINARY
  command -v "$boundrail" >/dev/null || die "binary $boundrail is not an executable"
else
  # The release this checkout of the action was cut from.
  version=${INPUT_VERSION:-$(sed -n 's/^version = "\(.*\)"$/\1/p' "$here/Cargo.toml" | head -n 1)}
  ARCHGRAPH_VERSION=$version ARCHGRAPH_INSTALL_DIR=$work/bin sh "$here/install.sh"
  boundrail=$work/bin/boundrail
fi
"$boundrail" --version

check=0
"$boundrail" model check --root "$path" --format annotations >"$work/check.txt" || check=$?
if ((check > 1)); then
  cat "$work/check.txt"
  die "model check failed with exit $check"
fi
# Each line is one workflow command Boundrail wrote, its text escaped, so it replays as itself.
grep -E '^::(warning|notice) ' "$work/check.txt" >"$work/annotations.txt" || true
resume
cat "$work/annotations.txt"
stop
warnings=$(grep -c '^::warning ' "$work/annotations.txt" || true)
notices=$(grep -c '^::notice ' "$work/annotations.txt" || true)
echo "model check: $warnings warnings, $notices information"
{
  echo "**Model check:** $warnings warnings, $notices information. GitHub shows at most 10"
  echo "annotations of each kind on the pull request; the step log lists every finding."
  echo
} >>"$GITHUB_STEP_SUMMARY"

# Posts the footprint as the pull request's comment, replacing the one this token's account
# posted before: a comment counts only when that account wrote it and it starts with the marker.
post_comment() {
  local -x GH_TOKEN=$gh_token
  local repo=${GITHUB_REPOSITORY:?} marker author ids id
  marker=$(head -n 1 "$work/comment.md")
  # A personal token names its account; GITHUB_TOKEN cannot read /user and posts as the bot.
  author=$(gh api user --jq .login 2>/dev/null) || author='github-actions[bot]'
  ids=$(gh api --paginate "repos/$repo/issues/$PR_NUMBER/comments" \
    --jq ".[] | select(.user.login == \"$author\" and (.body | startswith(\"$marker\"))) | .id") ||
    return 1
  id=${ids%%$'\n'*}
  if [[ -n $id ]]; then
    gh api --method PATCH "repos/$repo/issues/comments/$id" -F "body=@$work/comment.md" >/dev/null
  else
    gh api --method POST "repos/$repo/issues/$PR_NUMBER/comments" -F "body=@$work/comment.md" >/dev/null
  fi
}

if [[ -n $base_ref ]]; then
  [[ $base_ref != -* ]] || die "base-ref $base_ref is not a commit"
  # The footprint starts from the merge-base, so it needs the history down to the base.
  if [[ $(git -C "$path" rev-parse --is-shallow-repository) == true ]]; then
    git -C "$path" fetch --no-tags --unshallow origin ||
      die "could not fetch the history to $base_ref; check out with fetch-depth: 0"
  fi
  if ! git -C "$path" cat-file -e "$base_ref^{commit}" 2>/dev/null; then
    git -C "$path" fetch --no-tags origin "$base_ref" ||
      die "could not fetch $base_ref; check out with fetch-depth: 0"
  fi
  "$boundrail" model footprint --root "$path" --base "$base_ref" --format pr-comment >"$work/comment.md" || {
    cat "$work/comment.md"
    die "model footprint failed"
  }
  cat "$work/comment.md" >>"$GITHUB_STEP_SUMMARY"
  # A comment cut at its size limit points at the summary, which gets the whole footprint.
  if grep -q '^_Truncated at ' "$work/comment.md"; then
    "$boundrail" model footprint --root "$path" --base "$base_ref" --format markdown >"$work/footprint.md" || {
      cat "$work/footprint.md"
      die "model footprint failed"
    }
    # A fence longer than any tilde run in the text, which the repository controls.
    fence='~~~'
    longest=$(awk '{
      line = $0
      while (match(line, /~+/)) {
        if (RLENGTH > m) m = RLENGTH
        line = substr(line, RSTART + RLENGTH)
      }
    } END { print m + 0 }' "$work/footprint.md")
    while ((${#fence} <= longest)); do fence+='~'; done
    {
      echo
      echo "$fence"
      cat "$work/footprint.md"
      echo "$fence"
    } >>"$GITHUB_STEP_SUMMARY"
  fi
  # Best effort: a token that cannot write (a fork's pull request, a Dependabot run, a read-only
  # token) leaves a notice, not a failure.
  if [[ ${GITHUB_EVENT_NAME:-} == pull_request && -n $gh_token && -n ${PR_NUMBER:-} ]]; then
    if post_comment; then
      echo "footprint comment posted on pull request $PR_NUMBER"
    else
      resume
      echo "::notice::footprint comment not posted: the token cannot write"
      stop
    fi
  fi
fi

case $fail_on in
  warnings) ((warnings == 0)) || exit 1 ;;
  information) ((warnings + notices == 0)) || exit 1 ;;
  never) ;;
esac
