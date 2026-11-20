# Releasing (macOS)

A release is a version tag (`v0.2.0`). Pushing the tag runs [`.github/workflows/release-macos.yml`](.github/workflows/release-macos.yml), which calls [`scripts/release-macos.sh`](scripts/release-macos.sh) and attaches the files to a **draft** GitHub Release. Nothing is public until you press **Publish**.

| File | Contents |
|---|---|
| `BNK8-Configurator-<version>-macos-universal.dmg` | `BNK8 Configurator.app` (Apple Silicon + Intel), Developer ID signed, notarized and stapled; drag to Applications |
| `bnk8-<version>-macos-universal.tar.gz` | `bnk8` CLI (signed and notarized), README and licenses |
| `SHA256SUMS.txt` | Checksums of both |

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
4. Watch **Actions → Release (macOS)** (about 15 minutes; notarization is usually the slowest step). It fails early if the tag and the `Cargo.toml` versions differ.
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

SIGN_IDENTITY="Developer ID Application: Alexander Di Battista (TEAMID)" \
NOTARY_PROFILE=bnk8-notary \
scripts/release-macos.sh 0.2.0

gh release create v0.2.0 dist/* --draft --generate-notes --title "bnk8 v0.2.0"
```

`gh release create` creates the tag from the current `main` if you haven't pushed it.

## Smoke test without certificates

```sh
SIGN_IDENTITY=- scripts/release-macos.sh 0.1.0
```

This builds the same files with an ad-hoc signature and no notarization, so you can check the bundle and DMG. **Never publish these files**: Gatekeeper blocks them on other Macs. Add `TARGETS=aarch64-apple-darwin` to build only for Apple Silicon with Homebrew's Rust.
