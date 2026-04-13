#!/bin/bash
# We want to make sure all PRs are targeting the right branch when they're
# opened, otherwise we risk (for example) to land a beta-specific change to the
# default branch. This script ensures the branch of the PR matches the channel.

set -euo pipefail
IFS=$'\n\t'

source "$(cd "$(dirname "$0")" && pwd)/../shared.sh"

if isCiBranch try-perf || isCiBranch automation/bors/try || isCiBranch automation/bors/auto; then
    echo "channel verification is only executed on PR builds"
    exit
fi

# `GITHUB_BASE_REF` is only set for `pull_request` workflows. On `push` (e.g. fork branches used
# for development) it is empty and `ciBaseBranch` would be wrong — skip the check.
if [[ -z "${GITHUB_BASE_REF:-}" ]]; then
    echo "channel verification skipped: GITHUB_BASE_REF is unset (not a pull_request event)"
    exit 0
fi

channel=$(cat "$(ciCheckoutPath)/src/ci/channel")
case "${channel}" in
    nightly)
        channel_branch="main"
        ;;
    beta)
        channel_branch="beta"
        ;;
    stable)
        channel_branch="stable"
        ;;
    *)
        echo "error: unknown channel defined in src/ci/channel: ${channel}"
        exit 1
esac

branch="$(ciBaseBranch)"
if [[ "${branch}" != "${channel_branch}" ]]; then
    echo "error: PRs changing the \`${channel}\` channel should be sent to the \
\`${channel_branch}\` branch!"

    exit 1
fi
