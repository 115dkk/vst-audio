using Xunit;

namespace VstAudio.Tests;

public sealed class Float64Tests
{
    private static string Fixture => Environment.GetEnvironmentVariable("VST_AUDIO_ADVANCED_VST3")!;

    [AdvancedTheory]
    [InlineData(0, 0)]
    [InlineData(0, 193)]
    [InlineData(1, 0)]
    [InlineData(1, 193)]
    [InlineData(1061, 0)]
    [InlineData(1061, 193)]
    public void SynthSilence(int frames, int tail)
    {
        using var host = new Host();
        using var plugin = host.Load(Fixture, options: new LoadOptions { SampleFormat = SampleFormat.Float64, BlockSize = 128 });
        Assert.Equal(0, plugin.InputChannels);
        Assert.Equal(2, plugin.OutputChannels);
        Assert.Equal(0, plugin.Latency);
        Assert.Equal(SampleFormat.Float64, plugin.SampleFormat);
        double[] output = Enumerable.Repeat(-99.0, 2 * (frames + tail)).ToArray();
        plugin.Process(ReadOnlySpan<double>.Empty, frames, tail, output);
        Assert.Equal(new double[output.Length], output);
        Assert.Throws<ArgumentException>(() => plugin.Process(ReadOnlySpan<float>.Empty, frames, tail, new float[output.Length]));
        Assert.Throws<ArgumentException>(() => plugin.Process(Array.Empty<float[]>()));
        double[][] jagged = plugin.Process(Array.Empty<double[]>(), tail);
        Assert.Equal(2, jagged.Length);
        foreach (double[] channel in jagged) Assert.Equal(new double[tail], channel);
    }

    [AdvancedTheory]
    [InlineData(0, false)]
    [InlineData(193, false)]
    [InlineData(0, true)]
    [InlineData(193, true)]
    public void EffectClassSelectionPreservesF64(int tail, bool span)
    {
        using var host = new Host();
        IReadOnlyList<PluginClass> classes = host.Scan(Fixture);
        Assert.Equal(2, classes.Count);
        PluginClass effect = Assert.Single(classes, info => info.Name == "Test sidechain");
        using var plugin = host.Load(Fixture, options: new LoadOptions
        {
            ClassId = effect.Id,
            SampleFormat = SampleFormat.Float64,
            BlockSize = 128,
        });
        Assert.True(plugin.InputChannels >= 2);
        Assert.Equal(0, plugin.Latency);
        const int frames = 1061;
        double[][] input = Enumerable.Range(0, plugin.InputChannels).Select(_ => new double[frames]).ToArray();
        double[][] expected = Enumerable.Range(0, plugin.OutputChannels).Select(_ => new double[frames + tail]).ToArray();
        for (int frame = 0; frame < frames; frame++)
        {
            input[0][frame] = frame * 0.000000123456789012345;
            input[1][frame] = -input[0][frame] * 0.987654321012345;
            expected[0][frame] = input[0][frame];
            expected[1][frame] = input[1][frame];
        }
        for (int render = 0; render < 2; render++)
        {
            if (span)
            {
                double[] output = new double[plugin.OutputChannels * (frames + tail)];
                plugin.Process(input.SelectMany(channel => channel).ToArray(), frames, tail, output);
                Assert.Equal(expected.SelectMany(channel => channel).ToArray(), output);
            }
            else
            {
                double[][] output = plugin.Process(input, tail);
                Assert.Equal(expected.Length, output.Length);
                for (int channel = 0; channel < expected.Length; channel++) Assert.Equal(expected[channel], output[channel]);
            }
        }
        Assert.Throws<ArgumentException>(() => plugin.Process(new double[plugin.InputChannels - 1][]));
        Assert.Throws<ArgumentException>(() => plugin.Process(new[] { new double[1], new double[2] }));
        Assert.Throws<ArgumentException>(() => plugin.Process(Array.Empty<double>(), 1, 0, new double[plugin.OutputChannels]));
    }
}
