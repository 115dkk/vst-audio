using System.Runtime.InteropServices;
using VstAudio;
using Xunit;

[assembly: CollectionBehavior(DisableTestParallelization = true)]

namespace VstAudio.Tests;

public sealed class MatrixTheoryAttribute : TheoryAttribute
{
    public MatrixTheoryAttribute()
    {
        string[] variables = ["VST_AUDIO_WORKER", "VST_AUDIO_TEST_VST2", "VST_AUDIO_TEST_VST3"];
        if (variables.Any(name => string.IsNullOrEmpty(Environment.GetEnvironmentVariable(name))))
            Skip = "Native worker and VST2/VST3 fixtures required; run python bindings/dotnet/run_tests.py.";
    }
}

public sealed class AdvancedTheoryAttribute : TheoryAttribute
{
    public AdvancedTheoryAttribute()
    {
        string[] variables = ["VST_AUDIO_WORKER", "VST_AUDIO_ADVANCED_VST3"];
        if (variables.Any(name => string.IsNullOrEmpty(Environment.GetEnvironmentVariable(name))))
            Skip = "Native worker and advanced VST3 fixture required; run python bindings/dotnet/run_tests.py.";
    }
}

public sealed class HostTests
{
    private static string Fixture(PluginFormat format) => Environment.GetEnvironmentVariable(
        format == PluginFormat.Vst2 ? "VST_AUDIO_TEST_VST2" : "VST_AUDIO_TEST_VST3")!;

    private static float[][] Signal(int frames) =>
    [
        Enumerable.Range(0, frames).Select(i => (i % 251 - 125) / 256f).ToArray(),
        Enumerable.Range(0, frames).Select(i => (i * 7 % 257 - 128) / 512f).ToArray(),
    ];

    private static float[][] Expected(float[][] input, int tail)
    {
        int frames = input[0].Length;
        var output = Enumerable.Range(0, 6).Select(_ => new float[frames + tail]).ToArray();
        for (int frame = 0; frame < frames; frame++)
        {
            float left = input[0][frame], right = input[1][frame];
            float[] matrix = [left, right, (left + right) / 2, (left + right) / 2, left / 2, right / 2];
            for (int channel = 0; channel < 6; channel++) output[channel][frame] = matrix[channel];
        }
        return output;
    }

    private static void EqualChannels<T>(T[][] expected, T[][] actual)
    {
        Assert.Equal(expected.Length, actual.Length);
        for (int channel = 0; channel < expected.Length; channel++) Assert.Equal(expected[channel], actual[channel]);
    }

    public static IEnumerable<object[]> RenderCases()
    {
        foreach (PluginFormat format in new[] { PluginFormat.Vst2, PluginFormat.Vst3 })
            foreach (int frames in new[] { 0, 1, 63, 129, 1061 })
                foreach (int tail in new[] { 0, 193 })
                    foreach (bool span in new[] { false, true })
                        yield return [format, frames, tail, span];
    }

    [MatrixTheory]
    [InlineData(PluginFormat.Vst2)]
    [InlineData(PluginFormat.Vst3)]
    public void ScanLoadAndProperties(PluginFormat format)
    {
        using var host = new Host();
        Assert.Equal(Path.GetFullPath(Environment.GetEnvironmentVariable("VST_AUDIO_WORKER")!), host.WorkerPath);
        PluginClass info = Assert.Single(host.Scan(Fixture(format), format));
        Assert.Equal("Test matrix", info.Name);
        Assert.Equal("Test fixtures", info.Vendor);
        Assert.NotEmpty(info.Category);
        Assert.Equal(16, info.Id.Length);
        Assert.Equal(info.Name, Assert.Single(host.Scan(Fixture(format))).Name);
        using var plugin = host.Load(Fixture(format), format, new LoadOptions { ClassId = info.Id, SampleRate = 44100, BlockSize = 128 });
        Assert.Equal(2, plugin.InputChannels);
        Assert.Equal(6, plugin.OutputChannels);
        Assert.Equal(44100, plugin.SampleRate);
        Assert.Equal(128, plugin.BlockSize);
        Assert.Equal(64, plugin.Latency);
        Assert.Equal(SampleFormat.Float32, plugin.SampleFormat);
        Assert.Equal("Test matrix", Assert.Single(host.Scan(Fixture(format), format, sandboxed: true)).Name);
    }

    [MatrixTheory]
    [MemberData(nameof(RenderCases))]
    public void ExactMatrixRender(PluginFormat format, int frames, int tail, bool span)
    {
        using var host = new Host();
        using var plugin = host.Load(Fixture(format), options: new LoadOptions { BlockSize = 128 });
        float[][] input = Signal(frames);
        float[][] expected = Expected(input, tail);
        if (span)
        {
            float[] output = new float[6 * (frames + tail)];
            plugin.Process(input.SelectMany(channel => channel).ToArray(), frames, tail, output);
            Assert.Equal(expected.SelectMany(channel => channel).ToArray(), output);
        }
        else
        {
            EqualChannels(expected, plugin.Process(input, tail));
        }
        EqualChannels(Signal(frames), input);
    }

    [MatrixTheory]
    [InlineData(PluginFormat.Vst2)]
    [InlineData(PluginFormat.Vst3)]
    public void RepeatedRenderAndHostLifetime(PluginFormat format)
    {
        var host = new Host();
        using var plugin = host.Load(Fixture(format), format, new LoadOptions { BlockSize = 128 });
        host.Dispose();
        host.Dispose();
        Assert.Throws<ObjectDisposedException>(() => host.Scan(Fixture(format)));
        Assert.Throws<ObjectDisposedException>(() => host.Load(Fixture(format)));
        Assert.Throws<ObjectDisposedException>(() => host.WorkerPath);
        foreach (int frames in new[] { 129, 1, 1061 })
            EqualChannels(Expected(Signal(frames), 193), plugin.Process(Signal(frames), 193));
    }

    [MatrixTheory]
    [InlineData(PluginFormat.Vst2)]
    [InlineData(PluginFormat.Vst3)]
    public void ParametersAndState(PluginFormat format)
    {
        using var host = new Host();
        using var plugin = host.Load(Fixture(format), format);
        Parameter parameter = plugin.GetParameters()[0];
        Assert.Equal("Width", parameter.Name);
        Assert.NotNull(parameter.Unit);
        plugin.SetParameter(parameter.Id, 0.25);
        Assert.Equal(0.25, plugin.GetParameters().Single(p => p.Id == parameter.Id).Value);
        byte[] saved = plugin.SaveState();
        Assert.NotEmpty(saved);
        plugin.RestoreState(saved);
        Assert.Equal(saved, plugin.SaveState());
    }

    [MatrixTheory]
    [InlineData(PluginFormat.Vst2)]
    [InlineData(PluginFormat.Vst3)]
    public void ArgumentValidation(PluginFormat format)
    {
        using var host = new Host();
        using var plugin = host.Load(Fixture(format), format);
        Assert.Throws<ArgumentException>(() => plugin.Process(new float[1][]));
        Assert.Throws<ArgumentException>(() => plugin.Process(new[] { new float[1], new float[2] }));
        Assert.Throws<ArgumentException>(() => plugin.Process(new float[2][]));
        Assert.Throws<ArgumentException>(() => plugin.Process(new[] { new double[1], new double[1] }));
        Assert.Throws<ArgumentException>(() => plugin.Process(new double[2], 1, 0, new double[6]));
        Assert.Throws<ArgumentException>(() => plugin.Process(new float[1], 1, 0, new float[6]));
        Assert.Throws<ArgumentException>(() => plugin.Process(new float[2], 1, 0, new float[5]));
        float[] overlapping = new float[6];
        Assert.Throws<ArgumentException>(() => plugin.Process(overlapping.AsSpan(0, 2), 1, 0, overlapping));
        Assert.Throws<ArgumentOutOfRangeException>(() => plugin.Process(Signal(1), -1));
        Assert.Throws<ArgumentOutOfRangeException>(() => plugin.Process([], -1, 0, Array.Empty<float>()));
        Assert.Throws<ArgumentException>(() => plugin.Process([], int.MaxValue, 1, Array.Empty<float>()));
        TimeSpan negative = TimeSpan.FromTicks(-1);
        Assert.Throws<ArgumentOutOfRangeException>(() => plugin.Process(Signal(1), timeout: negative));
        Assert.Throws<ArgumentOutOfRangeException>(() => plugin.GetParameters(negative));
        Assert.Throws<ArgumentOutOfRangeException>(() => plugin.SetParameter(0, 0.5, negative));
        Assert.Throws<ArgumentOutOfRangeException>(() => plugin.SaveState(negative));
        Assert.Throws<ArgumentOutOfRangeException>(() => plugin.RestoreState([], negative));
        Assert.Throws<ArgumentOutOfRangeException>(() => plugin.OpenEditor(negative));
        Assert.Throws<ArgumentOutOfRangeException>(() => plugin.CloseEditor(negative));
        Assert.Throws<ArgumentOutOfRangeException>(() => host.Load(Fixture(format), options: new LoadOptions { Timeout = negative }));
        Assert.Throws<ArgumentException>(() => host.Load(Fixture(format), options: new LoadOptions { ClassId = new byte[15] }));
        Assert.Throws<ArgumentException>(() => host.Load(Fixture(format), options: new LoadOptions { SampleFormat = (SampleFormat)99 }));
        Assert.Throws<ArgumentOutOfRangeException>(() => host.Load(Fixture(format), options: new LoadOptions { SampleRate = -1 }));
        Assert.Throws<ArgumentOutOfRangeException>(() => host.Load(Fixture(format), options: new LoadOptions { BlockSize = 0 }));
        Assert.Contains("format", Assert.Throws<ArgumentException>(() => host.Scan("unknown.extension")).Message);
        Assert.Contains("format", Assert.Throws<ArgumentException>(() => host.Scan(Fixture(format), (PluginFormat)99)).Message);
        // Very large timeouts are clamped rather than overflowing u32 milliseconds.
        Assert.NotEmpty(plugin.GetParameters(TimeSpan.MaxValue));
    }

    [MatrixTheory]
    [InlineData(PluginFormat.Vst2)]
    [InlineData(PluginFormat.Vst3)]
    public async Task DisposeOnAnotherThreadAndRejectLaterCalls(PluginFormat format)
    {
        using var host = new Host();
        var plugin = host.Load(Fixture(format), format);
        await Task.Run(plugin.Dispose);
        plugin.Dispose();
        Assert.Throws<ObjectDisposedException>(() => plugin.InputChannels);
        Assert.Throws<ObjectDisposedException>(() => plugin.OutputChannels);
        Assert.Throws<ObjectDisposedException>(() => plugin.SampleRate);
        Assert.Throws<ObjectDisposedException>(() => plugin.BlockSize);
        Assert.Throws<ObjectDisposedException>(() => plugin.SampleFormat);
        Assert.Throws<ObjectDisposedException>(() => plugin.Latency);
        Assert.Throws<ObjectDisposedException>(() => plugin.Process(Signal(1)));
        Assert.Throws<ObjectDisposedException>(() => plugin.Process(new double[2][]));
        Assert.Throws<ObjectDisposedException>(() => plugin.Process(new float[2], 1, 0, new float[6]));
        Assert.Throws<ObjectDisposedException>(() => plugin.Process(new double[2], 1, 0, new double[6]));
        Assert.Throws<ObjectDisposedException>(() => plugin.GetParameters());
        Assert.Throws<ObjectDisposedException>(() => plugin.SetParameter(0, 0.5));
        Assert.Throws<ObjectDisposedException>(() => plugin.SaveState());
        Assert.Throws<ObjectDisposedException>(() => plugin.RestoreState([]));
        Assert.Throws<ObjectDisposedException>(() => plugin.OpenEditor());
        Assert.Throws<ObjectDisposedException>(() => plugin.CloseEditor());
    }

    [Fact]
    public void ExplicitMissingWorker()
    {
        string missing = Path.Combine(AppContext.BaseDirectory, "missing-worker-" + Guid.NewGuid());
        var error = Assert.Throws<WorkerNotFoundException>(() => new Host(missing));
        Assert.Equal("worker_not_found", error.Kind);
        Assert.Contains(missing, error.Message);
    }

    [MatrixTheory]
    [InlineData(PluginFormat.Vst2)]
    [InlineData(PluginFormat.Vst3)]
    public void BundledWorkerAndUnixPermissions(PluginFormat format)
    {
        string? previous = Environment.GetEnvironmentVariable("VST_AUDIO_WORKER");
        Environment.SetEnvironmentVariable("VST_AUDIO_WORKER", null);
        string rid = OperatingSystem.IsWindows() ? "win-x64" : OperatingSystem.IsLinux() ? "linux-x64"
            : RuntimeInformation.ProcessArchitecture == Architecture.Arm64 ? "osx-arm64" : "osx-x64";
        string directory = Path.Combine(AppContext.BaseDirectory, "runtimes", rid, "native");
        string worker = Path.Combine(directory, OperatingSystem.IsWindows() ? "vst-audio-worker.exe"
            : OperatingSystem.IsMacOS() ? "VstAudioWorker.app/Contents/MacOS/vst-audio-worker" : "vst-audio-worker");
        UnixFileMode? originalMode = null;
        try
        {
            if (!OperatingSystem.IsWindows())
            {
                originalMode = File.GetUnixFileMode(worker);
                File.SetUnixFileMode(worker, UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.GroupRead | UnixFileMode.OtherRead);
            }
            using var host = new Host();
            Assert.Equal(Path.GetFullPath(worker), host.WorkerPath);
            Assert.Equal("Test matrix", Assert.Single(host.Scan(Fixture(format), format)).Name);
            if (!OperatingSystem.IsWindows())
                Assert.Equal(UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute
                    | UnixFileMode.GroupRead | UnixFileMode.GroupExecute | UnixFileMode.OtherRead | UnixFileMode.OtherExecute,
                    File.GetUnixFileMode(worker));
        }
        finally
        {
            Environment.SetEnvironmentVariable("VST_AUDIO_WORKER", previous);
            if (!OperatingSystem.IsWindows() && originalMode is { } mode) File.SetUnixFileMode(worker, mode);
        }
    }

    [MatrixTheory]
    [InlineData(PluginFormat.Vst2)]
    [InlineData(PluginFormat.Vst3)]
    public void F32OnlyFixtureRejectsF64(PluginFormat format)
    {
        using var host = new Host();
        var error = Assert.Throws<StartException>(() => host.Load(Fixture(format), format, new LoadOptions { SampleFormat = SampleFormat.Float64 }));
        Assert.NotEmpty(error.Kind);
        Assert.NotEmpty(error.Message);
    }

    [MatrixTheory]
    [InlineData(PluginFormat.Vst2)]
    public void NativeErrorKinds(PluginFormat format)
    {
        using var host = new Host();
        var missing = Assert.Throws<StartException>(() => host.Load(Path.Combine(AppContext.BaseDirectory, "missing.vst3")));
        Assert.Contains(missing.Kind, new[] { "start", "io" });
        var startup = Assert.Throws<StartException>(() => host.Load(Fixture(format), format, new LoadOptions { Timeout = TimeSpan.Zero }));
        Assert.Equal("cancelled", startup.Kind);
        using var plugin = host.Load(Fixture(format), format, new LoadOptions { BlockSize = 128 });
        plugin.RestoreState([1, 2]); // Public fixture state: version 1, hang on audio processing.
        var render = Assert.Throws<RenderException>(() => plugin.Process(Signal(257), timeout: TimeSpan.FromMilliseconds(20)));
        Assert.Equal("timed_out", render.Kind);
        Assert.NotEmpty(render.Message);
    }
}
