"""Public API tests against real, isolated native fixture processes."""
import os
from pathlib import Path

import numpy as np
import pytest
import vst_audio

REQUIRED = ("VST_AUDIO_WORKER", "VST_AUDIO_TEST_VST2", "VST_AUDIO_TEST_VST3")
if any(not os.environ.get(name) for name in REQUIRED):
    pytest.skip("native worker and VST2/VST3 fixtures required; run python run_tests.py", allow_module_level=True)


@pytest.fixture(params=["vst2", "vst3"])
def fixture(request):
    return Path(os.environ[f"VST_AUDIO_TEST_{request.param.upper()}"])


def signal(frames, dtype=np.float32, order="C"):
    # Binary fractions are exact in both precisions, including matrix sums.
    index = np.arange(frames)
    return np.array([(index % 251 - 125) / 256, (index * 7 % 257 - 128) / 512],
                    dtype=dtype, order=order)


def expected(audio, tail):
    left, right = audio
    matrix = np.stack([left, right, (left + right) / 2, (left + right) / 2, left / 2, right / 2])
    return np.pad(matrix, ((0, 0), (0, tail)))


def test_scan_load_and_class_selection(fixture):
    host = vst_audio.Host()
    assert host.worker == Path(os.environ["VST_AUDIO_WORKER"]).resolve()
    classes = host.scan(fixture)
    assert len(classes) == 1
    info = classes[0]
    assert isinstance(info.id, bytes) and len(info.id) == 16
    assert info.name == "Test matrix"
    assert info.vendor == "Test fixtures"
    assert info.category
    with pytest.raises(AttributeError):
        info.name = "changed"
    with host.load(fixture, class_id=info.id, sample_rate=44100, block_size=128) as plugin:
        assert (plugin.input_channels, plugin.output_channels) == (2, 6)
        assert (plugin.sample_rate, plugin.block_size, plugin.latency) == (44100, 128, 64)


@pytest.mark.parametrize("frames", [0, 1, 63, 129, 1061])
@pytest.mark.parametrize("tail", [0, 193])
@pytest.mark.parametrize("order", ["C", "F"])
def test_exact_matrix_render(fixture, frames, tail, order):
    audio = signal(frames, order=order)
    original = audio.copy()
    with vst_audio.Host().load(fixture, block_size=128) as plugin:
        result = plugin.process(audio, tail=tail)
        assert result.dtype == np.float32
        assert result.shape == (6, frames + tail)
        np.testing.assert_array_equal(result, expected(audio, tail))
        np.testing.assert_array_equal(audio, original)


def test_repeated_render(fixture):
    with vst_audio.Host().load(fixture, block_size=128) as plugin:
        for frames in (129, 1, 1061):
            audio = signal(frames)
            np.testing.assert_array_equal(plugin.process(audio), expected(audio, 0))


@pytest.mark.parametrize("dtype", [np.float64, np.int32, np.float16])
def test_wrong_dtype(fixture, dtype):
    with vst_audio.Host().load(fixture) as plugin:
        with pytest.raises(TypeError, match="float32"):
            plugin.process(np.zeros((2, 65), dtype))


@pytest.mark.parametrize("shape", [(128,), (2, 4, 5), (1, 128), (3, 128)])
def test_wrong_shape(fixture, shape):
    with vst_audio.Host().load(fixture) as plugin:
        with pytest.raises(ValueError):
            plugin.process(np.zeros(shape, np.float32))


def test_parameters_and_state(fixture):
    with vst_audio.Host().load(fixture) as plugin:
        parameters = plugin.parameters()
        assert parameters and parameters[0].name == "Width"
        parameter = parameters[0]
        with pytest.raises(AttributeError):
            parameter.value = 0.5
        plugin.set_parameter(parameter.id, 0.25)
        current = {p.id: p.value for p in plugin.parameters()}
        assert current[parameter.id] == 0.25
        saved = plugin.save_state()
        assert isinstance(saved, bytes) and saved
        plugin.restore_state(saved)
        assert plugin.save_state() == saved


def test_close_twice(fixture):
    plugin = vst_audio.Host().load(fixture)
    plugin.close()
    plugin.close()
    with pytest.raises(vst_audio.RenderError) as error:
        plugin.process(signal(1))
    assert error.value.kind == "not_running"


def test_f32_only_fixture_rejects_f64(fixture):
    with pytest.raises(vst_audio.StartError) as error:
        vst_audio.Host().load(fixture, sample_format="f64")
    assert isinstance(error.value.kind, str)


def test_missing_worker(monkeypatch, tmp_path):
    monkeypatch.delenv("VST_AUDIO_WORKER", raising=False)
    bundled = tmp_path / "_worker/vst-audio-worker"
    monkeypatch.setattr(vst_audio, "_bundled_worker", lambda: bundled)
    with pytest.raises(vst_audio.WorkerNotFoundError, match="VST_AUDIO_WORKER") as error:
        vst_audio.Host()
    assert str(bundled) in str(error.value)


def test_bundled_worker(monkeypatch, fixture):
    monkeypatch.delenv("VST_AUDIO_WORKER", raising=False)
    host = vst_audio.Host()
    assert "_worker" in host.worker.parts
    assert host.scan(fixture)


def test_format_and_class_validation(fixture):
    host = vst_audio.Host()
    with pytest.raises(ValueError, match="format"):
        host.scan("plugin.unknown")
    with pytest.raises(ValueError, match="16 bytes"):
        host.load(fixture, class_id=b"bad")
    with pytest.raises(ValueError, match="sample_format"):
        host.load(fixture, sample_format="int16")
    with pytest.raises(ValueError, match="timeout"):
        host.load(fixture, timeout=float("nan"))


def test_hung_worker_render_timeout():
    path = os.environ["VST_AUDIO_TEST_VST2"]
    with vst_audio.Host().load(path, block_size=128) as plugin:
        # Public fixture state: version 1, mode 2 (hang on audio processing).
        plugin.restore_state(bytes([1, 2]))
        with pytest.raises(vst_audio.RenderError) as error:
            plugin.process(signal(257), timeout=0.02)
        assert error.value.kind == "timed_out"


def test_load_timeout():
    path = os.environ["VST_AUDIO_TEST_VST2"]
    with pytest.raises(vst_audio.StartError) as error:
        vst_audio.Host().load(path, timeout=0.0)
    assert error.value.kind == "cancelled"


def test_missing_plugin(tmp_path):
    with pytest.raises(vst_audio.StartError) as error:
        vst_audio.Host().load(tmp_path / "missing.vst3")
    assert error.value.kind in {"start", "io"}
