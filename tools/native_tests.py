"""Build public fixtures, then exercise real worker processes. No external secrets."""
import os
from pathlib import Path
import subprocess
from fixtures import prepare, advanced_paths

ROOT = Path(__file__).resolve().parents[1]

def run(*args, env=None):
    subprocess.run(args, cwd=ROOT, env=env, check=True)

worker, fixture, vst3 = prepare()
advanced_vst2,advanced_vst3=advanced_paths()
env = dict(os.environ, VST_AUDIO_WORKER=str(worker),
           VST_AUDIO_ADVANCED_VST2=str(advanced_vst2), VST_AUDIO_ADVANCED_VST3=str(advanced_vst3),
           VST_AUDIO_TEST_VST2=str(fixture), VST_AUDIO_TEST_VST3=str(vst3))
run("cargo", "test", "--locked", "-p", "vst-audio", "--test", "native",
    "--", "--ignored", "--test-threads=1", "--skip", "third_party_vst3", env=env)
run("cargo", "test", "--locked", "-p", "vst-audio", "--test", "processes",
    "--", "--ignored", "--nocapture", "--test-threads=1", env=env)
run("cargo", "test", "--locked", "-p", "vst-audio-worker-sys", "--test", "lifecycle",
    "--", "--ignored", "--test-threads=1", env=env)
run("cargo", "test", "--locked", "-p", "vst-audio", "--test", "application",
    "--", "--ignored", "--test-threads=1", "--skip", "audio_thread_automate_is_reported_without_allocating",
    "--skip", "parameter_edit_overflow_keeps_latest_value_and_reports_refresh", env=env)
run("cargo", "test", "--locked", "-p", "vst-audio", "--test", "application",
    "audio_thread_automate_is_reported_without_allocating",
    "--", "--ignored", "--nocapture", "--test-threads=1",
    env=dict(env, VST_AUDIO_TEST_AUDIO_AUTOMATE="1"))
run("cargo", "test", "--locked", "-p", "vst-audio", "--test", "application",
    "parameter_edit_overflow_keeps_latest_value_and_reports_refresh",
    "--", "--ignored", "--nocapture", "--test-threads=1",
    env=dict(env, VST_AUDIO_TEST_EDIT_FLOOD="1"))

run("cargo", "test", "--locked", "-p", "vst-audio", "--test", "features", "--", "--ignored", "--test-threads=1", env=env)

run("cargo", "test", "--locked", "-p", "vst-audio", "--test", "long_callbacks", "--", "--ignored", "--nocapture", "--test-threads=1", env=env)
run("cargo", "test", "--locked", "-p", "vst-audio", "--test", "stress", "--", "--ignored", "--nocapture", "--test-threads=1", env=env)

run("cargo", "test", "--locked", "-p", "vst-audio", "--test", "embedding",
    "--", "--ignored", "--nocapture", "--test-threads=1", env=env)

run("cargo", "test", "--locked", "-p", "vst-audio", "--test", "vst2_hardening",
    "--", "--ignored", "--nocapture", "--test-threads=1", env=env)
for variable, test in [("VST_AUDIO_TEST_CONFIGURE_LATENCY", "callbacks_time_latency_and_opaque_state"),
                       ("VST_AUDIO_TEST_SHELL", "shell_children"),
                       ("VST_AUDIO_TEST_VST3_RESIZE", "resize_reaches_application_floating_and_embedded"),
                       ("VST_AUDIO_TEST_NO_CHUNKS", "zero_program_parameter_state"),
                       ("VST_AUDIO_TEST_OPEN_COUNTS", "callbacks_time_latency_and_opaque_state"),
                       ("VST_AUDIO_TEST_NARROW_ARRANGEMENT", "narrow_arrangement_keeps_declared_channels")]:
    run("cargo", "test", "--locked", "-p", "vst-audio", "--test", "vst2_hardening", test,
        "--", "--ignored", "--nocapture", "--test-threads=1", env=dict(env, **{variable: "1"}))
for count in (1024, 1025):
    run("cargo", "test", "--locked", "-p", "vst-audio", "--test", "vst2_hardening", "shell_child_limit",
        "--", "--ignored", "--nocapture", "--test-threads=1",
        env=dict(env, VST_AUDIO_TEST_SHELL="1", VST_AUDIO_TEST_SHELL_CHILDREN=str(count)))

# Reserve a loopback probe port and let the Rust test own the listener. A separate
# environment keeps probe inputs out of unrelated worker tests.
import socket
import tempfile
with tempfile.TemporaryDirectory(prefix="vst-audio-probe-") as directory:
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    probe_env = dict(env, VST_AUDIO_PROBE_PORT=str(port),
                     VST_AUDIO_PROBE_FILE=str(Path(directory) / "probe.txt"))
    if os.name == "nt" and os.environ.get("GITHUB_ACTIONS"):
        # Disposable CI runner only: let the network-allowed AppContainer reach the
        # loopback probe, as an administrator would for a local licence service.
        run("CheckNetIsolation.exe", "LoopbackExempt", "-a", "-n=vst-audio.worker.network")
        probe_env["VST_AUDIO_LOOPBACK_EXEMPT"] = "1"
    run("cargo", "test", "--locked", "-p", "vst-audio", "--test", "sandbox",
        "--", "--ignored", "--nocapture", "--test-threads=1", env=probe_env)

# Encode only our public fixture captures. No third-party pixels or binaries.
import struct
import zlib
for source in (ROOT/"test-results/editor").glob("*.ppm"):
    magic, dimensions, maximum, pixels=source.read_bytes().split(b"\n",3)
    assert magic==b"P6" and maximum==b"255"
    width,height=map(int,dimensions.split())
    assert len(pixels)==width*height*3
    def chunk(kind,body):
        data=kind+body
        return struct.pack(">I",len(body))+data+struct.pack(">I",zlib.crc32(data))
    raw=b"".join(b"\0"+pixels[y*width*3:(y+1)*width*3] for y in range(height))
    png=b"\x89PNG\r\n\x1a\n"+chunk(b"IHDR",struct.pack(">IIBBBBB",width,height,8,2,0,0,0))+chunk(b"IDAT",zlib.compress(raw))+chunk(b"IEND",b"")
    source.with_suffix(".png").write_bytes(png)
