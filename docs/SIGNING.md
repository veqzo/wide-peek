# Code signing (maintainers)

widepeek uses free code signing from the [SignPath Foundation](https://signpath.org).
CI signs the release exe only when the repository variable `SIGNPATH_ENABLED`
is `true`. Then a missing token stops the release; CI never publishes an
unsigned exe by mistake. With the variable unset, releases get the unsigned
exe.

## How a signed release works

1. You push a tag `vX.Y.Z`. The `build` job checks that the tag is `v` + the
   version in `Cargo.toml` and uploads the unsigned `widepeek.exe` as a
   GitHub artifact.
2. The `sign` job runs in the environment `release`. It sends the artifact to
   SignPath with the parameter `version = X.Y.Z`. This job has no write access
   to the repository.
3. SignPath checks the exe against
   [`.signpath/artifact-configuration.xml`](../.signpath/artifact-configuration.xml):
   product name `widepeek` and product version `X.Y.Z`. `build.rs` writes both
   values from `Cargo.toml`.
4. You approve the request on app.signpath.io. The job waits up to 1 hour.
5. The `sign` job checks the Authenticode signature of the signed exe.
6. The `publish` job creates the release with the exe and its SHA-256 file.
   It has no access to the SignPath token.

## One-time setup

1. Turn on MFA for your GitHub account and later for your SignPath account.
2. Apply at <https://signpath.org/apply>. Use this repository URL and the
   download page `https://github.com/veqzo/wide-peek/releases`.
3. After the approval, on app.signpath.io:
   1. Add the trusted build system **GitHub.com** to the organization.
   2. Open the project (slug `widepeek`) and link the trusted build system
      **GitHub.com** to it.
   3. Set the artifact configuration to the content of
      `.signpath/artifact-configuration.xml`.
   4. Check the signing policy slug (default `release-signing`) and that it
      needs an approval. If the policy has origin rules, allow only this
      repository and `refs/tags/v*`.
   5. Create a CI user or an API token with submitter rights for the policy.
4. In GitHub, Settings > Environments, create the environment `release`:
   - **Deployment branches and tags:** selected tags only, pattern `v*`.
   - **Environment secret:** `SIGNPATH_API_TOKEN` with the SignPath API token.
     Do not store it as a repository secret: then every branch could read it.
5. In GitHub, Settings > Rules > Rulesets, add a tag ruleset for `v*`:
   only you can create, update or delete these tags.
6. In GitHub, Settings > Secrets and variables > Actions > Variables:

   | Variable | Value |
   |---|---|
   | `SIGNPATH_ENABLED` | `true` |
   | `SIGNPATH_ORGANIZATION_ID` | The organization ID from SignPath |
   | `SIGNPATH_PROJECT_SLUG` | Only if it is not `widepeek` |
   | `SIGNPATH_SIGNING_POLICY_SLUG` | Only if it is not `release-signing` |

## Rules

- Sign only builds from this repository's CI. Never sign a local build.
- Third-party actions are pinned to commit SHAs. Review each Dependabot
  update before you merge it.
- Keep the roles in the README [Code signing policy](../README.md#code-signing-policy)
  current.
- If the approval does not come within 1 hour, the job fails and nothing is
  published. Delete the release tag, push it again and approve in time.
- After the first signed release, the in-app updater accepts only updates
  with the same signer (see the README section Updates).
