"""The Matrix fixtures are f32-only; advanced fixtures provide native f64."""
import os

import numpy as np
import pytest
import vst_audio

REQUIRED = ("VST_AUDIO_WORKER", "VST_AUDIO_ADVANCED_VST2", "VST_AUDIO_ADVANCED_VST3")
if any(not os.environ.get(name) for name in REQUIRED):
    pytest.skip("native f64 advanced fixtures required; run python run_tests.py", allow_module_level=True)


@pytest.mark.parametrize("format", ["vst2", "vst3"])
@pytest.mark.parametrize("tail", [0, 193])
@pytest.mark.parametrize("frames", [0, 1, 1061])
def test_f64_synth_silence(format, tail, frames):
    path = os.environ[f"VST_AUDIO_ADVANCED_{format.upper()}"]
    with vst_audio.Host().load(path, format, sample_format="f64", block_size=128) as plugin:
        assert (plugin.input_channels, plugin.output_channels, plugin.latency) == (0, 2, 0)
        result = plugin.process(np.zeros((0, frames), np.float64), tail=tail)
        assert result.dtype == np.float64
        np.testing.assert_array_equal(result, np.zeros((2, frames + tail), np.float64))
        with pytest.raises(TypeError, match="float64"):
            plugin.process(np.zeros((0, frames), np.float32))


@pytest.mark.parametrize("order", ["C", "F"])
@pytest.mark.parametrize("tail", [0, 193])
def test_f64_vst3_effect_and_multiclass_selection(order, tail):
    path = os.environ["VST_AUDIO_ADVANCED_VST3"]
    host = vst_audio.Host()
    classes = host.scan(path)
    assert len(classes) == 2
    effect = next(info for info in classes if info.name == "Test sidechain")
    with host.load(path, class_id=effect.id, sample_format="f64", block_size=128) as plugin:
        assert plugin.input_channels >= 2
        assert plugin.latency == 0
        audio = np.zeros((plugin.input_channels, 1061), np.float64, order=order)
        # Values intentionally distinguish f64 from a round trip through f32.
        audio[0] = np.arange(1061) * 0.000000123456789012345
        audio[1] = -audio[0] * 0.987654321012345
        expected = np.zeros((plugin.output_channels, 1061 + tail), np.float64)
        expected[:2, :1061] = audio[:2]
        np.testing.assert_array_equal(plugin.process(audio, tail=tail), expected)
