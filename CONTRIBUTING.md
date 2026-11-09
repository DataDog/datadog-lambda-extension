# Contributing

Thank you for your interest in the Datadog Lambda Extension. This page tells you what a pull request needs before we can merge it.

## Sign your commits

GitHub blocks the merge of a pull request until every commit in it has a verified signature. Set up signing before you push your first commit:

1. Create a GPG or SSH signing key, and add it to your GitHub account as a signing key. See [Signing commits](https://docs.github.com/en/authentication/managing-commit-signature-verification/signing-commits) in the GitHub documentation.
2. Configure Git to sign every commit. For an SSH key:

   ```sh
   git config --global gpg.format ssh
   git config --global user.signingkey ~/.ssh/id_ed25519.pub
   git config --global commit.gpgsign true
   ```

3. Commit with an email address that you verified on your GitHub account. GitHub shows a commit as verified only when the signing key and the committer email belong to the same account.

If you already pushed commits without a signature, sign them again and force-push. Replace `upstream` with the name of your remote for `DataDog/datadog-lambda-extension`:

```sh
git rebase --exec 'git commit --amend --no-edit -n -S' $(git merge-base HEAD upstream/main)
git push --force-with-lease
```

## Open the pull request

- Fill in the pull request template.
- Keep **Allow edits by maintainers** selected. Then a maintainer can push small changes to your branch, for example to sign the commits again.

## Run the checks

The Rust code is in `bottlecap/`. Before you push, run these commands from that directory. CI runs the same checks:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --features default
cargo nextest run --workspace
```
