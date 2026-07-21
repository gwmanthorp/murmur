# Releasing Murmur

Murmur's Windows installer and in-app updates are published through GitHub Releases. CI validates every pull request and push to `main`; only a `vX.Y.Z` tag publishes an update.

## One-time GitHub setup

The updater signing private key is stored locally at:

`C:\Users\George\.tauri\murmur-updater.key`

Keep a secure backup. Losing it means existing installations cannot verify any future update. Never commit it, attach it to a release, or paste it into logs.

In the GitHub repository, open **Settings → Secrets and variables → Actions** and create:

- `TAURI_SIGNING_PRIVATE_KEY`: the complete contents of `murmur-updater.key`.
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`: leave this unavailable for the current passwordless key; the workflow treats an absent secret as empty.

The repository and its Releases must remain public so installed applications can download `latest.json` without credentials. The updater signature authenticates releases, but the first unsigned Windows installer may still trigger Microsoft SmartScreen because Murmur does not yet use a paid Authenticode certificate.

## Publish a release

Start from a clean, up-to-date `main`, then run:

```powershell
npm run version:set -- 0.2.1
git diff
git add package.json package-lock.json src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/tauri.conf.json
git commit -m "Release v0.2.1"
git tag v0.2.1
git push origin main
git push origin v0.2.1
```

Replace `0.2.1` with the intended SemVer. The release workflow rejects mismatched versions and tags not contained in `main`. A successful run publishes the NSIS installer, updater signature, generated release notes, and `latest.json`.

Version `0.2.0` is the updater bootstrap. Anyone on an older build must install `0.2.0` manually once; subsequent releases can update in app.

## Release checklist and rollback

Before tagging, confirm the branch CI is green, the signing secret is configured, the working tree is clean, and a locally built installer passes dictation, History, tray, and update-menu smoke tests. After publishing, install the release on a clean Windows account and monitor the Actions run and GitHub Release downloads.

Stop distribution if Murmur cannot launch, dictation or paste is broken, the updater signature fails, or settings/History are lost. Delete or unpublish the broken latest release to stop further downloads, then publish the reverted code as a higher patch version; the updater intentionally does not downgrade installations that already received the bad version.
