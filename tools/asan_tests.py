"""Instrument the native worker, public fixtures and tests with AddressSanitizer."""
import json
import os
from pathlib import Path
import subprocess
import sys
import uuid

if sys.platform != "linux":
    raise SystemExit("This native sanitizer lane currently requires Linux")
ROOT=Path(__file__).resolve().parents[1]
output=ROOT/"test-results/asan"
output.mkdir(parents=True,exist_ok=True)
report_prefix="report-"+uuid.uuid4().hex
env=dict(os.environ,
    RUSTUP_TOOLCHAIN="nightly-2026-10-20",
    CARGO_BUILD_TARGET="x86_64-unknown-linux-gnu",
    CARGO_TARGET_DIR=os.environ.get("VST_AUDIO_ASAN_TARGET",str(ROOT/"target/asan")),
    # Export the statically linked runtime so dlopen'ed instrumented plugins can
    # resolve __asan_* without loading a second incompatible runtime.
    RUSTFLAGS="-Zsanitizer=address -C link-arg=-Wl,--export-dynamic",
    ASAN_OPTIONS=f"detect_leaks=1:halt_on_error=1:log_path={output}/{report_prefix}",
    RUST_BACKTRACE="1")
result=subprocess.run(["xvfb-run","-a",sys.executable,"tools/native_tests.py"],cwd=ROOT,env=env,capture_output=True,text=True,timeout=600)
log=result.stdout+result.stderr
(output/"native.log").write_text(log,encoding="utf-8")
print(log[-32000:],flush=True)
findings=[]
for report in output.glob(report_prefix+".*"):
    contents=report.read_text(errors="replace")
    if "ERROR:" in contents or "SUMMARY:" in contents:
        findings.append(report.name)
summary={"toolchain":"nightly-2026-10-20","native_exit":result.returncode,"findings":findings,"passed":result.returncode==0 and not findings}
(output/"summary.json").write_text(json.dumps(summary,indent=2))
if not summary["passed"]: raise SystemExit(1)
