# Worker distribution and registry readiness

Rust **1.99.0** is the supported minimum and the exact toolchain used by the
four native CI targets. AddressSanitizer and fuzzing use the separately pinned
`nightly-2026-10-20`; consumers do not require nightly.

`python tools/package_worker.py` builds an optimized native worker, collects
dependency license texts, writes a version/target/protocol manifest, and creates
a ZIP plus SHA-256 file under `target/distributions`. It extracts that ZIP into
a fresh directory and executes the archived release worker against 20 native
VST2/VST3 tests, including numeric DSP, editor lifecycle, state and failure.
On Linux use `xvfb-run -a python3 tools/package_worker.py`.

The worker is an executable on Windows/Linux and an AppKit `.app` helper on
macOS. Pass the extracted executable's absolute path to `Host::new`; on macOS
that is `VstAudioWorker.app/Contents/MacOS/vst-audio-worker`. Keep it beside the
application or in its helper directory. A Cargo library dependency does not
install a matching worker. Keep both at the same release: incompatible control
protocol majors and ring layouts are rejected during startup.

Artifacts contain only this project's worker and license/provenance records.
Third-party compatibility-test plugins are built in a separate temporary tree
and never enter these ZIPs. The license inventory follows the worker's selected
target dependency graph and excludes dev dependencies. If a crate omitted its
license text, the packager retrieves it from the exact Git commit recorded in
that crate's `.cargo_vcs_info.json`; the source URL and hash are recorded.

## macOS signing

CI bundles use an ad-hoc hardened-runtime signature. This validates the helper
layout and native execution; the artifacts are **not Developer ID signed or
notarized**. The helper's only extra entitlement is
`com.apple.security.cs.disable-library-validation`, needed to host plugin code
signed by other developers. Keep that entitlement on the worker helper; the
application process does not load plugin libraries.

A distributor with a Developer ID identity should sign the helper, then the
containing application, from the inside out. For a standalone worker helper:

```sh
codesign --force --sign "Developer ID Application: YOUR NAME (TEAMID)" \
  --timestamp --options runtime --entitlements worker.entitlements.plist \
  VstAudioWorker.app
codesign --verify --strict --verbose=2 VstAudioWorker.app
ditto -c -k --keepParent VstAudioWorker.app worker-notarize.zip
xcrun notarytool submit worker-notarize.zip --keychain-profile YOUR_PROFILE --wait
xcrun stapler staple VstAudioWorker.app
xcrun stapler validate VstAudioWorker.app
spctl --assess --type execute --verbose=2 VstAudioWorker.app
```

Recreate the shipping archive and its checksum after signing/stapling. A host
application embedding the helper submits its final application bundle instead.
Signing credentials belong only to the distributor's trusted release job.
See Apple's [library-validation entitlement](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.security.cs.disable-library-validation)
and [notarization workflow](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution).

## Cargo publication

`python tools/package_check.py` creates all seven `.crate` archives, extracts
them outside the checkout, then builds a worker and an independent consumer
whose patches refer only to those extracted archives. This exercises normalized
Cargo manifests and package contents before any registry mutation.

The dependency order is ring, protocol, transport-sys, application library,
worker-sys, worker-runtime, worker executable. The test plugins always remain
`publish = false`.

Every push to `main` runs `release.yml`. Its gate (`tools/release_gate.py`)
releases the workspace version when it has no tag yet; otherwise, when a
shipped file changed since that tag, it commits a version bump to `main`
(`feat:` bumps minor, anything else patch) and releases that. The whole of
`ci.yml` then runs on the release commit, and only after it passes are the
crates published through crates.io trusted publishing (no registry token is
stored in the repository) and the tag and GitHub release created with the
worker archives of all four targets. Pull requests and other branches run
`ci.yml` alone and never publish.
