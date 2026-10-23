"""Required native Wayland fixtures under an isolated Weston compositor."""
import os
from pathlib import Path
import shutil
import re
import subprocess
import tempfile
import time
from fixtures import prepare

ROOT = Path(__file__).resolve().parents[1]

def main():
    if not shutil.which("weston"):
        raise RuntimeError("weston is required")
    worker, fixture, vst3 = prepare()
    base = dict(os.environ, VST_AUDIO_WORKER=str(worker),
                VST_AUDIO_TEST_VST2=str(fixture), VST_AUDIO_TEST_VST3=str(vst3))
    base.pop("DISPLAY", None)
    base.pop("WAYLAND_SOCKET", None)
    failures = []
    def run(test, name, env):
        result = subprocess.run(["cargo", "test", "--locked", "-p", "vst-audio", "--test", test, name,
            "--", "--ignored", "--nocapture", "--test-threads=1"], cwd=ROOT, env=env)
        if result.returncode:
            failures.append((test, name, env.get("VST_AUDIO_TEST_SCALE")))
    for scale, desktop in ((1, False), (2, False), (1, True)):
        with tempfile.TemporaryDirectory(prefix="vst-audio-wayland-") as directory:
            os.chmod(directory, 0o700)
            env = dict(base, XDG_RUNTIME_DIR=directory, WAYLAND_DISPLAY="vst-audio-test",
                       VST_AUDIO_TEST_SCALE=str(scale))
            log_path = ROOT / "test-results" / f"weston-scale-{scale}-desktop-{desktop}.log"
            log_path.parent.mkdir(parents=True, exist_ok=True)
            # Screenshots count fixture-colored pixels over the whole output, so
            # the desktop must not draw anything that changes: no panel clock,
            # a solid background instead of the default gradient, and no startup
            # fade or window animations (a slow runner screenshotted the black
            # startup fade still covering the first editor).
            config = Path(directory) / "weston.ini"
            config.write_text("[shell]\npanel-position=none\nbackground-color=0xff000000\nlocking=false\n"
                              "startup-animation=none\nanimation=none\nclose-animation=none\nfocus-animation=none\n")
            with log_path.open("w") as log:
                weston = subprocess.Popen(["weston", "--backend=headless", "--renderer=pixman",
                    f"--config={config}", "--debug", "--socket=vst-audio-test", "--width=1280", "--height=800",
                    "--idle-time=0", f"--scale={scale}"] + (["--xwayland"] if desktop else []), env=env, stdout=log, stderr=log)
                try:
                    deadline = time.monotonic() + 10
                    while not (Path(directory) / "vst-audio-test").exists():
                        if weston.poll() is not None or time.monotonic() > deadline:
                            raise RuntimeError(f"Weston startup failed: {log_path.read_text()}")
                        time.sleep(0.02)
                    time.sleep(1)
                    if desktop:
                        display = re.search(r"xserver listening on display (:\d+)", log_path.read_text())
                        if not display:
                            raise RuntimeError(f"Weston XWayland unavailable: {log_path.read_text()}")
                        env["DISPLAY"] = display.group(1)
                        run("wayland", "wayland_attach_rejection_falls_back_only_for_auto",
                            dict(env, VST_AUDIO_REJECT_WAYLAND="1"))
                        run("wayland", "stretch_uses_the_desktop_xwayland", env)
                        # Existing tests default to Auto. Remove Wayland discovery
                        # only in the workers, retaining the desktop's XWayland.
                        env.pop("WAYLAND_DISPLAY")
                        for test, name in (("native", "editor_round_trip"),
                                           ("application", "editor_visibility_focus_scaling_errors_and_pixels_roundtrip"),
                                           ("embedding", "")):
                            run(test, name, env)
                    else:
                        run("wayland", "native_wayland_pixels_lifecycle_scale_and_sandbox",
                            dict(env, VST_AUDIO_VIEW_LIFECYCLE="1"))
                        run("wayland", "failed_wayland_recreation_closes_editor",
                            dict(env, VST_AUDIO_REJECT_RECREATED_VIEW="1"))
                        run("wayland", "wayland_selection_and_vst2_rejection", env)
                        run("wayland", "initial_content_scale_precedes_attachment",
                            dict(env, VST_AUDIO_EXPECT_ATTACH_SCALE=str(scale)))
                        run("wayland", "rejected_content_scale_preserves_actual_scale",
                            dict(env, VST_AUDIO_REJECT_SCALE="1"))
                        marker = Path(directory) / "kill-ready"
                        child = subprocess.Popen(["cargo", "test", "--locked", "-p", "vst-audio", "--test", "wayland",
                            "compositor_disconnect_closes_editor_and_preserves_audio", "--", "--ignored", "--nocapture"],
                            cwd=ROOT, env=dict(env, VST_AUDIO_KILL_MARKER=str(marker)))
                        deadline = time.monotonic() + 20
                        while not marker.exists() and child.poll() is None and time.monotonic() < deadline:
                            time.sleep(0.01)
                        weston.terminate()
                        weston.wait(timeout=5)
                        if child.wait(timeout=15):
                            failures.append(("wayland", "disconnect", scale))
                finally:
                    weston.terminate()
                    try:
                        weston.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        weston.kill()
                        weston.wait()

    if failures:
        raise RuntimeError(f"Wayland test failures: {failures}")

if __name__ == "__main__":
    main()
