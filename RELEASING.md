# Releasing (macOS)

A release is a version tag (`v0.2.0`). Pushing the tag runs [`.github/workflows/release-macos.yml`](.github/workflows/release-macos.yml), which calls [`scripts/release-macos.sh`](scripts/release-macos.sh) and attaches the files to a **draft** GitHub Release. Nothing is public until you press **Publish**.

| File | Contents |
|---|---|
| `BNK8-Configurator-<version>-macos-universal.dmg` | `BNK8 Configurator.app` (Apple Silicon + Intel), Developer ID signed, notarized and stapled; drag to Applications |
| `bnk8-<version>-macos-universal.tar.gz` | `bnk8` CLI (signed and notarized), README and licenses |
| `SHA256SUMS.txt` | Checksums of both |

Unsigned pre-releases (see [below](#unsigned-pre-releases)) carry the same files with `-unsigned` before the extension, e.g. `BNK8-Configurator-<version>-macos-universal-unsigned.dmg`.

The app requires macOS 11 or newer.

## One-time setup

### 1. Developer ID Application certificate

Only the Apple Developer team's **Account Holder** can create one, and only on the website; the App Store Connect API can't.

1. In **Keychain Access**, open **Certificate Assistant → Request a Certificate From a Certificate Authority**. Enter your email and name, choose **Saved to disk**, and save the `.certSigningRequest` file.
2. Go to <https://developer.apple.com/account/resources/certificates/add>, choose **Developer ID Application** (G2 Sub-CA), upload the request, then download the `.cer` and double-click it to install it.
3. Check that it shows up:
   ```sh
   security find-identity -v -p codesigning | grep "Developer ID Application"
   ```
   The quoted name, e.g. `Developer ID Application: Alexander Di Battista (TEAMID)`, is your signing identity.
4. For CI, export it: in Keychain Access, under **My Certificates**, right-click the certificate (it must show its private key underneath), choose **Export**, and save a `.p12` with a strong password.

### 2. Notarization key

1. In [App Store Connect](https://appstoreconnect.apple.com/access/integrations/api), go to **Users and Access → Integrations → App Store Connect API → Team Keys** and generate a key with the **Developer** role.
2. Download `AuthKey_<KEYID>.p8`. Apple lets you download it only once. Note the **Key ID** and the **Issuer ID** shown above the key list.
3. For local releases, store it in the keychain under a profile name:
   ```sh
   xcrun notarytool store-credentials bnk8-notary \
     --key AuthKey_<KEYID>.p8 --key-id <KEYID> --issuer <ISSUER-ID>
   ```

### 3. GitHub secrets

```sh
base64 -i DeveloperID.p12 | gh secret set MACOS_CERTIFICATE_P12
gh secret set MACOS_CERTIFICATE_PASSWORD          # prompts for the .p12 password
gh secret set APPLE_API_KEY_P8 < AuthKey_<KEYID>.p8
gh secret set APPLE_API_KEY_ID --body <KEYID>
gh secret set APPLE_API_ISSUER --body <ISSUER-ID>
```

Then delete the exported `.p12`, and keep the `.p8` somewhere safe, such as a password manager.

**Current setup (2026-11-24):** identity `Developer ID Application: V&A Labs inc (D763F5MGGM)` (certificate `PP2PV557YA`, expires 2027-12-24), notarization key `9887GJC4JS` ("bnk8 notary", Developer role), local notarytool profile `bnk8-notary`, and all five GitHub secrets set. When the certificate expires, create a new one from a new CSR and replace `MACOS_CERTIFICATE_P12` and `MACOS_CERTIFICATE_PASSWORD`.

**Dry run:** to check signing and notarization without releasing, run the workflow by hand: **Actions → Release (macOS) → Run workflow** on `main`, or `gh workflow run release-macos.yml --ref main`. It builds, signs and notarizes the version in `Cargo.toml` and keeps the files as the run's `macos-release` artifact for 7 days; it creates no tag or release.

## Each release

1. Pick the version, e.g. `0.2.0`, and set it in three places:
   - `version` in `Cargo.toml`
   - `version` in `gui/Cargo.toml`
   - the `bnk8 = { version = "…", path = ".." }` dependency in `gui/Cargo.toml`
2. Update the lockfile and run the tests:
   ```sh
   cargo build --workspace && cargo test --workspace
   ```
3. Commit, tag and push:
   ```sh
   git commit -am "Release v0.2.0"
   git tag v0.2.0
   git push origin main v0.2.0
   ```
4. Watch **Actions → Release (macOS)** (about 15 minutes; notarization is usually the slowest step). It fails early if a GitHub secret is missing or the tag and the `Cargo.toml` versions differ, and it refuses to create a release if one for the tag already exists.
5. Test the draft on a Mac. Download the DMG with a browser, so it gets the quarantine flag users get. Open it, drag the app to Applications and launch it: there must be no "unidentified developer" warning. Run `bnk8 --version` from the tarball.
6. Edit the generated release notes and press **Publish**.

**If the workflow fails:** fix the problem on `main`, delete the draft release if one was created, then move the tag:

```sh
git tag -d v0.2.0 && git push --delete origin v0.2.0
git tag v0.2.0 && git push origin v0.2.0
```

While the repo is private, each run uses about 150 of the 2,000 free monthly Actions minutes, because macOS minutes count 10×. Public repos run free.

## Releasing from your Mac instead

You need [rustup](https://rustup.rs) with both targets. Homebrew's `rust` builds only for your own CPU.

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin

SIGN_IDENTITY="Developer ID Application: V&A Labs inc (D763F5MGGM)" \
NOTARY_PROFILE=bnk8-notary \
scripts/release-macos.sh 0.2.0

git push origin main
gh release create v0.2.0 dist/* --draft --generate-notes --title "bnk8 v0.2.0" \
  --target "$(git rev-parse HEAD)"
```

Push the commit first: `--target` points the tag at that commit, so GitHub must already have it. If several keychain identities share the name, pass the SHA-1 hash shown by `security find-identity -v -p codesigning` as `SIGN_IDENTITY` instead.

Pushing the `v0.2.0` tag, or publishing the draft (which creates the tag), also starts the release workflow. It refuses with "a release for v0.2.0 already exists"; that failure is expected and changes nothing.

## Unsigned pre-releases

Without a Developer ID certificate you can still build and publish, but only as a GitHub **pre-release** whose title says "unsigned pre-release":

```sh
SIGN_IDENTITY=- scripts/release-macos.sh 0.2.0

git push origin main
gh release create v0.2.0 dist/* --prerelease --generate-notes \
  --title "bnk8 v0.2.0 (unsigned pre-release)" --target "$(git rev-parse HEAD)"
```

`SIGN_IDENTITY=-` signs ad-hoc and skips notarization, and the files are named `…-macos-universal-unsigned.dmg` and `…-macos-universal-unsigned.tar.gz`. Gatekeeper blocks them on other Macs with a "could not verify" warning; users have to click **Open Anyway** or clear the quarantine flag, as the README's Download section explains. Never publish them as a regular release.

Creating the release creates the tag, which starts the release workflow. It fails (missing secrets, or "a release for v0.2.0 already exists"); that failure is expected and changes nothing.

To check the bundle and DMG locally without publishing, run just the script. Add `TARGETS=aarch64-apple-darwin` to build only for Apple Silicon with Homebrew's Rust; the files are then named `…-macos-aarch64-unsigned…`.
