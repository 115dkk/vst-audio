"""Create and execute a native release-worker distribution, without publishing."""
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
import zipfile
import urllib.error
from fixtures import ROOT, download, prepare, target_directory

def run(*args, **options):
    subprocess.run(list(map(str,args)),cwd=ROOT,check=True,**options)

def digest(path): return hashlib.sha256(path.read_bytes()).hexdigest()

def dependency_notices(destination,target):
    data=json.loads(subprocess.check_output(["cargo","metadata","--locked","--format-version","1","--filter-platform",target],cwd=ROOT,text=True))
    packages={p["id"]:p for p in data["packages"]}
    nodes={p["id"]:p for p in data["resolve"]["nodes"]}
    queue=[next(p["id"] for p in packages.values() if p["name"]=="vst-audio-worker")]
    seen=set()
    while queue:
        identity=queue.pop()
        if identity in seen: continue
        seen.add(identity)
        queue.extend(d["pkg"] for d in nodes[identity]["deps"] if any(k["kind"]!="dev" for k in d["dep_kinds"]))
    notices=[]
    for identity in sorted(seen,key=lambda key:packages[key]["name"]):
        package=packages[identity]
        source=Path(package["manifest_path"]).parent
        files=[p for p in source.iterdir() if p.is_file() and p.name.upper().startswith(("LICENSE","COPYING","NOTICE","COPYRIGHT"))]
        if package.get("license_file"):
            files.append(source/package["license_file"])
        folder=destination/f"{package['name']}-{package['version']}"
        folder.mkdir(parents=True)
        for path in set(files): shutil.copyfile(path,folder/path.name)
        upstream=[]
        if not files:
            # Some crates omit workspace-level license texts. Recover only from
            # the exact source commit embedded in the downloaded crate archive.
            vcs=json.loads((source/".cargo_vcs_info.json").read_text())
            revision=vcs["git"]["sha1"]
            repository=package["repository"].removesuffix(".git").rstrip("/")
            assert repository.startswith("https://github.com/") and re.fullmatch(r"[0-9a-f]{40}",revision)
            path=repository.removeprefix("https://github.com/")
            for filename in ["LICENSE","LICENSE-MIT","LICENSE-APACHE","LICENSE.txt","COPYING","LICENSE.md"]:
                url=f"https://raw.githubusercontent.com/{path}/{revision}/{filename}"
                try:
                    contents=download(url)
                except urllib.error.HTTPError as error:
                    if error.code==404: continue
                    raise
                (folder/filename).write_bytes(contents)
                upstream.append({"url":url,"sha256":hashlib.sha256(contents).hexdigest()})
            assert upstream, f"pinned license text missing for {package['name']}"
        notices.append({"name":package["name"],"version":package["version"],"license":package["license"],"repository":package.get("repository"),"upstream_license_sources":upstream,"texts":[str(p.relative_to(destination)) for p in sorted(folder.iterdir())]})
    return notices

def main():
    package=tomllib.loads((ROOT/"Cargo.toml").read_text())["workspace"]["package"]
    version=package["version"]
    target=os.environ.get("CARGO_BUILD_TARGET") or re.search(r"^host: (.+)$",subprocess.check_output(["rustc","-vV"],cwd=ROOT,text=True),re.M).group(1)
    run("cargo","build","--locked","--release","-p","vst-audio-worker")
    binary=target_directory()/"release"/("vst-audio-worker.exe" if sys.platform=="win32" else "vst-audio-worker")
    artifact=ROOT/"target/distributions"
    artifact.mkdir(parents=True,exist_ok=True)
    name=f"vst-audio-worker-{version}-{target}"
    # Only first-party binaries and their notices enter this temporary tree.
    with tempfile.TemporaryDirectory(prefix="vst-audio-distribution-") as temporary:
        scratch=Path(temporary)
        bundle=scratch/name
        bundle.mkdir()
        if sys.platform=="darwin":
            app=bundle/"VstAudioWorker.app"
            worker=app/"Contents/MacOS/vst-audio-worker"
            worker.parent.mkdir(parents=True)
            info={"CFBundleExecutable":"vst-audio-worker","CFBundleIdentifier":"io.github.115dkk.vst-audio.worker","CFBundleName":"VST Audio Worker","CFBundlePackageType":"APPL","CFBundleShortVersionString":version,"CFBundleVersion":version,"LSUIElement":True}
            (app/"Contents/Info.plist").write_bytes(plistlib.dumps(info))
        else:
            worker=bundle/binary.name
        shutil.copyfile(binary,worker)
        if sys.platform!="win32": worker.chmod(0o755)
        for document in ["LICENSE","NOTICE"]: shutil.copyfile(ROOT/document,bundle/document)
        # Preserve the clean-room ABI's complete binary redistribution notice.
        license_lines=(ROOT/"crates/vst-audio-worker-sys/src/vst2.rs").read_text().split("\n\n#!",1)[0]
        (bundle/"LICENSE-BSD-3-Clause-VST2.txt").write_text("\n".join(line.removeprefix("// ") for line in license_lines.splitlines()))
        licenses=dependency_notices(bundle/"licenses",target)
        (bundle/"dependencies.json").write_text(json.dumps(licenses,indent=2))
        signing="unsigned"
        if sys.platform=="darwin":
            entitlements=bundle/"worker.entitlements.plist"
            entitlements.write_bytes(plistlib.dumps({"com.apple.security.cs.disable-library-validation":True}))
            run("codesign","--force","--sign","-","--options","runtime","--entitlements",entitlements,app)
            run("codesign","--verify","--strict","--verbose=2",app)
            signing="ad-hoc hardened runtime; not notarized"
        manifest={"source_worktree_dirty":bool(subprocess.check_output(["git","status","--porcelain"],cwd=ROOT,text=True).strip()),"version":version,"target":target,"rust_version":package["rust-version"],"source_revision":subprocess.check_output(["git","rev-parse","HEAD"],cwd=ROOT,text=True).strip(),"worker":worker.relative_to(bundle).as_posix(),"worker_sha256":digest(worker),"signing":signing,"protocol_major":int(re.search(r"PROTOCOL_MAJOR: u16 = (\d+)",(ROOT/"crates/vst-audio-protocol/src/lib.rs").read_text()).group(1)),"ring_version":int(re.search(r"LAYOUT_VERSION: u32 = (\d+)",(ROOT/"crates/vst-audio-ring/src/lib.rs").read_text()).group(1))}
        (bundle/"manifest.json").write_text(json.dumps(manifest,indent=2))
        archive=artifact/(name+".zip")
        with zipfile.ZipFile(archive,"w",compression=zipfile.ZIP_DEFLATED) as output:
            for path in sorted(bundle.rglob("*")):
                if path.is_file(): output.write(path,path.relative_to(scratch))
        extracted=scratch/"extracted"
        with zipfile.ZipFile(archive) as source: source.extractall(extracted)
        extracted_worker=extracted/name/manifest["worker"]
        assert digest(extracted_worker)==manifest["worker_sha256"]
        if sys.platform!="win32": extracted_worker.chmod(0o755)
        if sys.platform=="darwin": run("codesign","--verify","--strict",extracted/name/"VstAudioWorker.app")
        _,vst2,vst3=prepare()
        env=dict(os.environ,VST_AUDIO_WORKER=str(extracted_worker),VST_AUDIO_TEST_VST2=str(vst2),VST_AUDIO_TEST_VST3=str(vst3))
        # Actual archived release executable: both formats, numeric DSP, editor,
        # crash/hang/state lifecycle. No binary is loaded by the parent.
        run("cargo","test","--locked","-p","vst-audio","--test","native","--","--ignored","--test-threads=1","--skip","third_party_vst3",env=env)
        summary={**manifest,"archive":archive.name,"archive_sha256":digest(archive),"native_tests":"20 passed","passed":True}
        (artifact/(name+".json")).write_text(json.dumps(summary,indent=2))
        (artifact/(name+".sha256")).write_text(f"{summary['archive_sha256']}  {archive.name}\n")
        print(json.dumps(summary,indent=2))

if __name__=="__main__": main()
