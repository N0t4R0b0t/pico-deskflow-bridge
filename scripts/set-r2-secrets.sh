#!/usr/bin/env bash
# Sets the repository secrets the R2 publishing job in
# .github/workflows/ci.yml needs. Needs the GitHub CLI, logged in with `gh auth
# login` as someone who can administer the repository.
#
#   scripts/set-r2-secrets.sh                 # prompts for each value
#   R2_BUCKET=... R2_ENDPOINT=... R2_ACCESS_KEY_ID=... R2_SECRET_ACCESS_KEY=... \
#       scripts/set-r2-secrets.sh             # no prompts
#
# REPO overrides the target repository (default: this checkout's `origin`).
# The secret values go to gh on stdin, never on a command line.
set -euo pipefail

command -v gh >/dev/null || { echo "The GitHub CLI (gh) isn't installed: https://cli.github.com" >&2; exit 1; }
gh auth status >/dev/null 2>&1 || { echo "Not logged in: run 'gh auth login' first." >&2; exit 1; }

repo="${REPO:-$(gh repo view --json nameWithOwner -q .nameWithOwner)}"
echo "Setting the R2 secrets on ${repo}"

ask() { # <name> <prompt> <secret?>
    local name="$1" prompt="$2" secret="${3:-}"
    if [ -z "${!name:-}" ]; then
        if [ -n "${secret}" ]; then
            read -r -s -p "${prompt}: " "${name}"; echo
        else
            read -r -p "${prompt}: " "${name}"
        fi
    fi
    [ -n "${!name}" ] || { echo "${name} is empty." >&2; exit 1; }
}

ask R2_BUCKET "R2 bucket name"
ask R2_ENDPOINT "R2 endpoint (https://<account id>.r2.cloudflarestorage.com)"
ask R2_ACCESS_KEY_ID "R2 access key ID" secret
ask R2_SECRET_ACCESS_KEY "R2 secret access key" secret

case "${R2_ENDPOINT}" in
    https://*.r2.cloudflarestorage.com | https://*.r2.cloudflarestorage.com/) ;;
    *) echo "Warning: ${R2_ENDPOINT} doesn't look like an R2 endpoint; continuing." >&2 ;;
esac

printf '%s' "${R2_BUCKET}" | gh secret set R2_BUCKET --repo "${repo}"
printf '%s' "${R2_ENDPOINT%/}" | gh secret set R2_ENDPOINT --repo "${repo}"
printf '%s' "${R2_ACCESS_KEY_ID}" | gh secret set R2_ACCESS_KEY_ID --repo "${repo}"
printf '%s' "${R2_SECRET_ACCESS_KEY}" | gh secret set R2_SECRET_ACCESS_KEY --repo "${repo}"

echo
gh secret list --repo "${repo}" | grep '^R2_' || true
