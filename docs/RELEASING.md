# How to make a release

GitHub Actions makes every release. You do not build, sign, or upload on your
Mac. This page tells you what to do. Every step is one action.

What the workflows do:

- `ci.yml` builds and tests every push to `main` and every pull request.
- `release.yml` makes a release. It starts only when you start it by hand.
  A tag push does not start it.
- `release.yml` makes a DRAFT release unless you turn on `publish`. No
  installed app sees a draft.

What CI cannot do: it cannot judge the picture or timing of the real
player. Three tests need a real GPU and an even clock, and CI only tries
them. You run `dev/test` on your Mac before each release. This is a step
below.

## One-time set-up

Do this once. Do it on your Mac, in this folder.

### 1. Export the private key

The private key of the updates is in your Keychain. Every installed Bloom
trusts only this key. If you lose it, no installed app can update. If someone
steals it, that person can push an update to every installed app.

1. Run `vendor/sparkle/bin/generate_keys -x ~/sparkle-ed-key.txt`.
2. Allow the access when the Keychain asks.
3. Run `chmod 600 ~/sparkle-ed-key.txt`.
4. Check that the file holds the key that the app trusts:

   ```sh
   echo check > /tmp/key-check.txt
   sig=$(vendor/sparkle/bin/sign_update --ed-key-file ~/sparkle-ed-key.txt -p /tmp/key-check.txt)
   pub=$(sed -n 's/^pub const UPDATE_PUBLIC_KEY: &str = "\(.*\)";/\1/p' src/brand.rs)
   swift dev/verify-update.swift "$pub" /tmp/key-check.txt "$sig" && echo MATCH
   rm /tmp/key-check.txt
   ```

5. If the output does not say `MATCH`, stop. Do not continue.

### 2. Keep a backup outside the Mac

1. Open the file `~/sparkle-ed-key.txt`. It holds one line.
2. Copy the line into a secure note of your password manager.
3. Check that the note is there. Do not save the key in iCloud Drive, in the
   repository, in a chat, or in an issue.

### 3. Make the environment `release`

The environment holds the key and asks you to approve each release run.

1. Open `https://github.com/dantech2000/bloom/settings/environments`.
2. Choose "New environment". Name it `release`.
3. Turn on "Required reviewers". Add yourself.
4. Leave "Prevent self-review" off. With one maintainer, it would lock you out.
5. Under "Deployment branches and tags", choose "Selected branches and
   tags". Add the branch rule `main`.
6. Save the rules.

### 4. Add the key as the secret

1. Run `gh secret set SPARKLE_ED_KEY --env release --repo dantech2000/bloom < ~/sparkle-ed-key.txt`.
2. Run `gh secret list --env release --repo dantech2000/bloom`.
3. Check that the list shows `SPARKLE_ED_KEY`.
4. Delete the file: `rm ~/sparkle-ed-key.txt`.

The Keychain still has its own copy. Only the environment `release` can read
the secret, and only a run on `main` can use that environment.

### 5. Set the repository rules

1. Open `https://github.com/dantech2000/bloom/settings/actions`.
2. Under "Fork pull request workflows", choose "Require approval for all
   outside collaborators".
3. Under "Workflow permissions", choose "Read repository contents and
   packages permissions".
4. Turn off "Allow GitHub Actions to create and approve pull requests".
5. Open `https://github.com/dantech2000/bloom/settings/rules`.
6. Make a ruleset for the tags `v*`. Turn on "Restrict deletions" and
   "Restrict updates". Do not turn on "Restrict creations": the release
   workflow makes the tag.

## Make a release

Replace `x.y.z` with the new version.

1. Run `dev/test` on your Mac. Every test must pass. This runs the three
   tests of picture and timing that CI only tries.
2. Write the notes in `docs/releases/x.y.z.md`. Say what is new in plain
   words. Do not write the install steps or the list of libraries: the
   workflow adds `docs/releases/_footer.md`. The release fails if the file is
   missing or empty.
3. Run `dev/bump x.y.z`. It sets the version in `Cargo.toml` and `Cargo.lock`.
4. Commit the changes with the message `Release x.y.z`.
5. Push to `main`.
6. Run `gh run watch --repo dantech2000/bloom` and wait for CI to pass on the
   commit. The release stops if CI has not passed on the commit.
7. Do a dry run (next section). Do this at least for the first release made
   by CI.
8. Start the real release:

   ```sh
   gh workflow run release.yml --repo dantech2000/bloom --ref main -f version=x.y.z -f publish=true
   ```

9. Open the run: `gh run list --repo dantech2000/bloom --workflow release.yml`.
10. When the job `Sign the feed` waits, choose "Review deployments", check
    that the version is the one you want, and approve.
11. Wait for the job `Make the GitHub release` to finish. It checks that the
    latest feed is the new feed.
12. Check the result:

    ```sh
    gh release view vx.y.z --repo dantech2000/bloom
    curl -fsSL https://github.com/dantech2000/bloom/releases/latest/download/appcast.xml | head -12
    gh release download vx.y.z --repo dantech2000/bloom -p 'Bloom-*.zip' -D /tmp/bloom-check
    gh attestation verify /tmp/bloom-check/Bloom-x.y.z.zip --repo dantech2000/bloom
    ```

13. Open the installed Bloom of an older version. Choose "Check for Updates".
    Check that the update appears.

## Do a dry run

A dry run makes a DRAFT release. It uses your real key. A draft is not the
latest release. No installed app sees it.

1. Do steps 1 to 6 above.
2. Run:

   ```sh
   gh workflow run release.yml --repo dantech2000/bloom --ref main -f version=x.y.z -f publish=false
   ```

3. Approve the job `Sign the feed` as in step 10.
4. Open `https://github.com/dantech2000/bloom/releases`. Find the draft
   `Bloom x.y.z`. Check the notes, the zip, and `appcast.xml`.
5. If you want to test the zip, download it from the draft and open it.
6. To publish: start the workflow again with `publish=true`. The run replaces
   the draft. Do not publish the draft by hand.
7. To drop the dry run: run `gh release delete vx.y.z --repo dantech2000/bloom --yes`.

### A rehearsal of the workflow itself

You can test a change of the workflow before it is on `main`. A run on a
branch other than `main` uses a throwaway key and cannot publish.

1. Push the branch.
2. Run `gh workflow run release.yml --repo dantech2000/bloom --ref <branch> -f version=x.y.z -f publish=false`.
   GitHub starts a workflow by hand only when its file is on `main` too; the
   run then uses the file of the branch.
3. The branch must have the commit `Release x.y.z`.
4. Download the artifact `dist-unsigned` of the run. It holds the zip and the
   feed. The job `Sign the feed` and the job `Make the GitHub release` do
   not run.

## What CI proves, and what it does not

Each push to `main` and each pull request runs `.github/workflows/ci.yml`.

- The job `Build and test` is the gate. It builds libmpv with
  `dev/build-mpv`, builds the app, compiles the icon, makes the bundle,
  checks it with `dev/release-verify --bundle`, and runs the tests that need
  no real player.
- The job `Tests with the real player` is a gate too, on a push. It runs
  every test with libmpv, but for three. A runner is a virtual machine
  with no GPU: the picture test can get a black frame, and the two timing
  tests are too uneven there. Those three run in a step of their own that
  only reports. The workflow names them in `UNSTABLE`.
- So `dev/test` on a Mac stays the gate of a release for the picture and
  for timing.
- A build of libmpv from nothing downloads eleven source archives from
  their own servers. A server that does not answer is tried again; when one
  stays down, start the run again. With a cache nothing is downloaded: the
  cache is keyed on `dev/build-mpv` and the version of Xcode, and a weekly
  run keeps it alive.

The first release made this way was 0.1.5.

## When a run fails

1. Open the run and read the log of the red job.
2. If a check of `dev/release-verify` failed, the log says which one. Fix the
   cause. Do not turn the check off.
3. If the job `Build the archive` failed, nothing is published. Fix the
   cause, push, and start the run again.
4. If the job `Sign the feed` failed, nothing is published. The key file is
   removed in the last step also when the job fails.
5. If the job `Make the GitHub release` failed, look at
   `https://github.com/dantech2000/bloom/releases`.
6. If a draft of the version is there, start the run again. It replaces the
   draft.
7. If a release of the version is there and it is not complete, make it a
   prerelease with `gh release edit vx.y.z --prerelease --repo dantech2000/bloom`.
   Then delete it with `gh release delete vx.y.z --cleanup-tag --yes --repo dantech2000/bloom`.
   Then start the run again.
8. If the run says a tag exists, delete the tag with
   `gh api -X DELETE repos/dantech2000/bloom/git/refs/tags/vx.y.z` after you
   check that no release uses it.
9. If a run does not start at the approval, check that the environment `release`
   has you as reviewer and the rule `main`.

## Roll back a bad release

An installed app reads the feed of the latest release. A rollback moves
"latest" back.

1. Run `gh release edit vx.y.z --prerelease --repo dantech2000/bloom`.
2. Open `https://github.com/dantech2000/bloom/releases` and check that the
   badge "Latest" is on the release before.
3. Run `curl -fsSL https://github.com/dantech2000/bloom/releases/latest/download/appcast.xml | head -12`.
   Check that the top entry is the version before.

What installed apps do:

- An app that did not update yet sees the old feed. It does not offer the
  bad version.
- An app that already updated to the bad version stays on it. Sparkle does
  not go back to an older version.
- An app that began the download of the zip finishes it, because a prerelease
  keeps its files. If you delete the release, that download fails.

To help the apps that have the bad version, make a new release with a higher
version number than the bad one. The new feed starts from the feed of the
release before, so the bad entry is not in it. Do not reuse the bad version
number.

## What you never do

- Never run `gh release create` by hand for a version. Never upload an
  `appcast.xml` by hand to the latest release. The feed of the latest
  release goes to every installed app.
- Never make a test release "latest". A test is a draft or a run on a branch.
- Never print the private key, paste it in a chat, an issue, or a commit, or
  save it in the repository.
- Never change `UPDATE_PUBLIC_KEY` in `src/brand.rs` without a plan. Installed
  apps reject updates that the new key signs.
- Never change a workflow to start on `pull_request_target` or on a tag push,
  or to give a secret to a pull request.
- Never move or reuse a tag `vx.y.z`.
- Never edit the files of a published release.
- Never use a `pip install`, `curl | sh`, or an action without a commit
  hash in a workflow. Pin every action to a full commit hash.

## What protects the key

- The secret is in the environment `release`. Only a job of a run on `main`
  that you approved can read it.
- A pull request from a fork never gets a secret.
- The job that builds the code has no secret. The job that signs does not
  build code. The private key is on the disk only while `dev/release --stage
  sign` runs, with mode 600, and the last step removes it.
- A tag push does not start a release. The workflow makes the tag.
- The workflow checks that CI passed on the commit, that the version is new
  and in `Cargo.toml`, and that the signature verifies with the public key in
  `src/brand.rs`. A wrong key stops the run before it publishes.
- Anyone with write access to the repository can start a run on `main`, but
  the run waits for your approval.

## Later: a Developer ID

The app is signed ad hoc and not notarised. macOS blocks it at the first
start, and the install steps in the notes say how to open it. If you get an
Apple Developer ID, the job `Sign the feed` can sign the bundle with the
certificate (from a secret, in a temporary keychain) and notarise it with
`xcrun notarytool submit --wait` and `xcrun stapler staple`. Do this before
the zip is made, and the Sparkle signature must be made after it. Nothing of
this is built now.
