# Worker distribution and registry readiness

Rust **1.99.0** is the supported minimum and the exact toolchain used by the
four native CI targets. AddressSanitizer and fuzzing use the separately pinned
`nightly-2026-10-20`; consumers do not require nightly.

`python tools/package_worker.py` builds an optimized native worker, collects
dependency license texts, writes a version/target/protocol manifest, and creates
a ZIP plus SHA-256 file under `target/distributions`. It extracts that ZIP into
a fresh directory and executes the archived release worker with the ignored
native VST2/VST3 suite, including numeric DSP, editor lifecycle, state and
failure. The summary records the exact test command and its exit status.
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

## Architecture-specific workers

Build each additional worker with `cargo build --locked --release --target
<Rust target> -p vst-audio-worker` and ship it beside the native worker. Executable
names are arbitrary: pass the native executable to `Host::new`, then register
additional paths with `Host::with_worker(Architecture::X86, worker_path)` (or
`X86_64` / `Aarch64`). Selection reads plugin headers and prefers a native slice;
recovery reuses the originally selected executable. Registration does not install
OS compatibility support. Linux x86 workers need 32-bit runtime libraries;
macOS x86_64 workers on Apple Silicon require Rosetta. Build a thin executable
for each registered architecture so the OS launches the intended architecture.
Package each macOS worker in its own helper bundle and sign every helper before
signing the containing application; the signing requirements below apply to each.

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

## Python and .NET packages

The Python bindings (PyPI `vst-audio`, import name `vst_audio`) and .NET bindings
(NuGet `VstAudio`, targeting `net8.0`) are implemented under `bindings/`
as described in ADR 0006. These packages carry their own versions, independent
of the crate version, and are outside the release gate's shipped files, so
changing them never releases the crates. The Python and C ABI binding crates
have independent Cargo locks that the gate rewrites on a crate version bump.

Each wheel bundles the release worker of its platform under `vst_audio/_worker`
with the licence notices of the worker's and the extension module's Rust
dependencies (`dependencies.json`, `dependencies-extension.json`);
`bindings/python/stage_worker.py` stages them. There is no sdist on PyPI, since a source build would not contain the
worker. `ci.yml` builds one abi3 wheel per native target (the Linux one with
`bindings/python/build_manylinux.sh` in the pinned `manylinux_2_28` container,
which `auditwheel` checks), runs `twine check`, and installs it into a fresh
venv where `bindings/python/run_tests.py --wheel` runs the pytest suite against
the native VST2 and VST3 fixtures and the bundled worker. Locally,
`python bindings/python/run_tests.py` (Python 3.11 or newer) builds and tests a
wheel in one step. On every push to `main`, the `pypi` and `nuget` jobs of
`release.yml` publish through trusted publishing and skip versions that are
already published. A NuGet version cannot be deleted, only unlisted.

To release a binding, raise its version (`bindings/python/pyproject.toml` and
the conda recipe's `context.version`, which a tool test keeps equal, or
`bindings/dotnet/Version.props`, which the package, its tests, the GUI examples
and the runners read). When the publish job finds the version missing from the
registry before uploading it, it tags the release commit `python-v<version>` or
`dotnet-v<version>` after the upload (`tools/binding_release.py`). The `pypi`
job then hashes that tag's GitHub archive and uploads `conda-staged-recipe`:
`recipes/vst-audio/` with the recipe pointed at the archive, `build.py` and
`variants.yaml`, ready for a pull request to `conda-forge/staged-recipes`.

The NuGet package contains `lib/net8.0/VstAudio.dll` and its XML API documentation,
plus the C ABI library, matching worker and the licence notices of both
(`dependencies.json`, `dependencies-ffi.json`) under
`runtimes/<rid>/native/` for `win-x64`, `linux-x64`, `osx-arm64` and `osx-x64`.
The macOS helper remains an intact `VstAudioWorker.app` bundle. Packaged
`buildTransitive/VstAudio.targets` preserves these paths in build and publish
outputs instead of allowing the SDK to flatten native subdirectories. On Unix,
`Host()` adds worker execute bits corresponding to its existing read bits before
first use; explicit and environment-selected workers are not chmodded.

`python bindings/dotnet/stage_native.py` builds the release C ABI crate and
stages it with the worker using `bindings/python/stage_worker.py`'s optional
`--destination`. It honours `CARGO_TARGET_DIR` and `CARGO_BUILD_TARGET`.
`bindings/dotnet/build_manylinux.sh` stages `linux-x64` inside the same
`manylinux_2_28` container as Python. In `ci.yml`, the `dotnet-native` jobs
stage each runtime identifier on its own runner, `dotnet-pack` merges the four
`runtimes/` trees into one package and checks that every library and worker is
in it, and the `dotnet-test` jobs install that package on all four targets and
run the C ABI tests (`bindings/ffi/run_tests.py`) and the C# tests.

`python bindings/dotnet/run_tests.py` builds native fixtures, stages natives,
packs into `target/dotnet-pack`, then runs xUnit tests against that nupkg with a
fresh package cache under `target/dotnet-packages`. Pass `--package-dir DIR` to
test an already packed package instead. Tests exercise VST2/VST3 scanning,
exact Float32/Float64 rendering, parameters, state, errors, disposal and bundled
worker lookup; without fixture environment variables, native cases are skipped
with instructions for running the fixture builder.

Trusted publisher registrations:

| Registry | Owner | Repository | Workflow | Environment | Other |
|---|---|---|---|---|---|
| crates.io | `115dkk` | `vst-audio` | `release.yml` | none | |
| PyPI | `115dkk` | `vst-audio` | `release.yml` | `pypi` | project `vst-audio` |
| NuGet | `115dkk` | `vst-audio` | `release.yml` | `nuget` | package glob `VstAudio*` |

The `nuget` job reads the nuget.org profile name from the repository variable
`NUGET_USER`. GitHub creates a missing environment on first use without
protection rules; restrict the `pypi` and `nuget` environments to the `main`
branch in the repository settings.

### conda-forge

The conda-forge recipe lives in `bindings/python/conda` in the rattler-build
format (`recipe.yaml`, `build.py`), and CI builds it from the checkout. It is
submitted to `conda-forge/staged-recipes` after a `python-v<version>` release
whose archive contains it; that copy replaces the `path` source with the tag's
GitHub archive URL and sha256, since PyPI has no sdist (ADR 0006).

- Build: `build.py` runs `stage_worker.py` with the build environment's Python
  (3.11 or newer), which builds the locked release worker and stages it with
  its notices; the host Python then installs the abi3 extension with
  `maturin --locked`, and `cargo-bundle-licenses` writes the dependency
  licences of the worker and of the extension into the package's licence files.
- One package per platform serves every Python from conda-forge's
  `python_min` (`python-abi3`), and the recipe test runs `abi3audit`.
- glibc: `variants.yaml` raises the Linux floor to 2.28, the floor of the PyPI
  wheel. Built against conda-forge's default 2.17 sysroot, the worker does not
  link: it uses `memfd_create` (glibc 2.27) and `statx` (2.28).
- Licence: `(MIT OR Apache-2.0) AND BSD-3-Clause AND ISC`, with `LICENSE-MIT`,
  `LICENSE-APACHE`, `LICENSE-ISC-DPF`, the generated BSD-3-Clause VST2 notice
  and the two dependency licence files.
- The recipe's own tests import the package, run `pip check`, start the
  bundled worker (`--help`) and run `abi3audit`. conda-forge has no display and
  no fixtures, so numeric and editor checks stay in this repository's CI.
- conda-forge cross-compiles `osx-arm64` and does not run its tests there. The
  `conda` job in `ci.yml` therefore builds the recipe natively on all four
  targets with conda-forge's rattler-build, installs the package into a fresh
  environment and runs `bindings/python/conda/run_tests.py`: the pytest suite
  against the native fixtures with the bundled worker, failing on any skipped
  test, plus the glibc ceiling on Linux and the worker bundle's signature on
  macOS. `ci-pinning.yaml` stands in for conda-forge's global pinning in that
  job and is not submitted.
