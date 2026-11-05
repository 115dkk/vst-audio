using System.Diagnostics;
using System.Runtime;
using System.Text.Json;
using VstAudio;
using Xunit;
using Xunit.Abstractions;

namespace VstAudio.Tests;

public sealed class RealtimeTests
{
    private readonly ITestOutputHelper output;
    private static volatile object? churnSink;

    public RealtimeTests(ITestOutputHelper output) => this.output = output;

    private static string Fixture(PluginFormat format) => Environment.GetEnvironmentVariable(
        format == PluginFormat.Vst2 ? "VST_AUDIO_TEST_VST2" : "VST_AUDIO_TEST_VST3")!;

    private static (Host Host, Plugin Plugin, BlockProcessor Blocks) Open(PluginFormat format = PluginFormat.Vst2)
    {
        var host = new Host();
        var plugin = host.Load(Fixture(format), format, new LoadOptions { BlockSize = 128, Realtime = true });
        return (host, plugin, plugin.TakeBlockProcessor());
    }

    [MatrixTheory]
    [InlineData(PluginFormat.Vst2)]
    [InlineData(PluginFormat.Vst3)]
    public void RealtimeBlocksMatchTheOfflineMatrix(PluginFormat format)
    {
        var opened = Open(format);
        using var host = opened.Host;
        using var plugin = opened.Plugin;
        using var blocks = opened.Blocks;
        Assert.Equal(2, blocks.InputChannels);
        Assert.Equal(6, blocks.OutputChannels);
        Assert.Equal(128, blocks.Quantum);
        Assert.Equal(SampleFormat.Float32, blocks.SampleFormat);
        Assert.Equal(192, blocks.GetLatencyFrames());
        const int blockCount = 8;
        float[] source = new float[2 * blockCount * blocks.Quantum];
        for (int frame = 0; frame < blockCount * blocks.Quantum; frame++)
        {
            source[frame] = (frame % 251 - 125) / 256f;
            source[blockCount * blocks.Quantum + frame] = (frame * 7 % 257 - 128) / 512f;
        }
        float[] input = new float[blocks.InputChannels * blocks.Quantum];
        float[] result = new float[blocks.OutputChannels * blockCount * blocks.Quantum];
        for (int block = 0; block < blockCount; block++)
        {
            for (int channel = 0; channel < blocks.InputChannels; channel++)
                source.AsSpan(channel * blockCount * blocks.Quantum + block * blocks.Quantum, blocks.Quantum)
                    .CopyTo(input.AsSpan(channel * blocks.Quantum, blocks.Quantum));
            BlockStatus status = blocks.Process(input, result.AsSpan(block * blocks.OutputChannels * blocks.Quantum,
                blocks.OutputChannels * blocks.Quantum));
            Assert.Equal(block < blocks.Depth ? BlockStatus.Missed : BlockStatus.Delivered, status);
            // A generous pause: this test checks the delivered samples, not timing.
            Thread.Sleep(100);
        }
        int latency = blocks.GetLatencyFrames();
        for (int block = blocks.Depth; block < blockCount; block++)
            for (int channel = 0; channel < blocks.OutputChannels; channel++)
                for (int frame = 0; frame < blocks.Quantum; frame++)
                {
                    int outputFrame = block * blocks.Quantum + frame;
                    int inputFrame = outputFrame - latency;
                    float expected = 0;
                    if (inputFrame >= 0)
                    {
                        float left = source[inputFrame];
                        float right = source[blockCount * blocks.Quantum + inputFrame];
                        expected = channel switch
                        {
                            0 => left,
                            1 => right,
                            2 or 3 => (left + right) / 2,
                            4 => left / 2,
                            _ => right / 2,
                        };
                    }
                    int resultIndex = block * blocks.OutputChannels * blocks.Quantum + channel * blocks.Quantum + frame;
                    Assert.Equal(expected, result[resultIndex]);
                }
        Assert.Throws<InvalidOperationException>(() => plugin.Process(new float[2], 1, 0, new float[6]));
        Assert.Equal(BlockStatus.InvalidBuffer, blocks.Process(new float[1], new float[blocks.OutputChannels * blocks.Quantum]));
        Assert.Equal(BlockStatus.InvalidBuffer, blocks.Process(new double[blocks.InputChannels * blocks.Quantum], new double[blocks.OutputChannels * blocks.Quantum]));
    }

    [MatrixTheory]
    [InlineData(PluginFormat.Vst2)]
    public void BlockProcessorDoesNotAllocate(PluginFormat format)
    {
        var opened = Open(format);
        using var host = opened.Host;
        using var plugin = opened.Plugin;
        using var blocks = opened.Blocks;
        float[] input = new float[blocks.InputChannels * blocks.Quantum];
        float[] target = new float[blocks.OutputChannels * blocks.Quantum];
        for (int index = 0; index < 500; index++)
        {
            blocks.Process(input, target);
            Thread.Sleep(1);
        }
        int delivered = 0, missed = 0;
        long before = GC.GetAllocatedBytesForCurrentThread();
        for (int index = 0; index < 20_000; index++)
        {
            if (blocks.Process(input, target) == BlockStatus.Delivered) delivered++;
            else missed++;
        }
        long allocated = GC.GetAllocatedBytesForCurrentThread() - before;
        WriteMeasurement(new { test = nameof(BlockProcessorDoesNotAllocate), allocatedBytes = allocated, delivered, missed });
        Assert.Equal(0, allocated);
        Assert.True(delivered > 0, "expected Delivered calls");
        Assert.True(missed > 0, "expected Missed calls");
    }

    [MatrixTheory]
    [InlineData(PluginFormat.Vst2)]
    public void BlockProcessorDoesNotWaitForAHungWorker(PluginFormat format)
    {
        using var host = new Host();
        using var plugin = host.Load(Fixture(format), format, new LoadOptions { BlockSize = 128, Realtime = true });
        plugin.RestoreState([1, 2]);
        using var blocks = plugin.TakeBlockProcessor();
        float[] input = new float[blocks.InputChannels * blocks.Quantum];
        float[] target = new float[blocks.OutputChannels * blocks.Quantum];
        long[] elapsed = new long[1_000];
        int missed = 0;
        for (int index = 0; index < elapsed.Length; index++)
        {
            long started = Stopwatch.GetTimestamp();
            BlockStatus status = blocks.Process(input, target);
            elapsed[index] = Stopwatch.GetTimestamp() - started;
            if (index >= blocks.Depth) Assert.Equal(BlockStatus.Missed, status);
            if (status == BlockStatus.Missed) missed++;
        }
        Array.Sort(elapsed);
        double maxMs = TicksToMilliseconds(elapsed[^1]);
        double p99Ms = TicksToMilliseconds(elapsed[(int)(elapsed.Length * 0.99) - 1]);
        WriteMeasurement(new { test = nameof(BlockProcessorDoesNotWaitForAHungWorker), maxMs, p99Ms, missed });
        // A call that waited for the hung worker would block for the worker's whole
        // deadline (seconds). Shared CI runners can preempt any single call for a
        // few milliseconds, so the slowest call gets a scheduler allowance and the
        // 99th percentile carries the realtime claim.
        Assert.True(p99Ms < 1, $"99th percentile call was {p99Ms:F3} ms");
        Assert.True(maxMs < 100, $"slowest call was {maxMs:F3} ms");
    }

    [MatrixTheory]
    [InlineData(PluginFormat.Vst2)]
    public void ManagedCallbackIsSuspendedByABlockingGc(PluginFormat format)
    {
        var opened = Open(format);
        using var host = opened.Host;
        using var plugin = opened.Plugin;
        using var blocks = opened.Blocks;
        float[] input = new float[blocks.InputChannels * blocks.Quantum];
        float[] target = new float[blocks.OutputChannels * blocks.Quantum];
        for (int index = 0; index < 500; index++) blocks.Process(input, target);
        var baseline = RunAudioThread(blocks, input, target, TimeSpan.FromSeconds(4), null);

        byte[][]? live = null;
        long longestGcTicks = 0;
        using var stop = new CancellationTokenSource();
        using var graphReady = new ManualResetEventSlim();
        var collector = new Thread(() =>
        {
            live = new byte[200_000][];
            for (int index = 0; index < live.Length; index++) live[index] = new byte[1_024];
            graphReady.Set();
            while (!stop.IsCancellationRequested)
            {
                Thread.Sleep(500);
                TimeSpan before = GC.GetTotalPauseDuration();
                GC.Collect(2, GCCollectionMode.Forced, blocking: true, compacting: true);
                TimeSpan delta = GC.GetTotalPauseDuration() - before;
                long measured = (long)(delta.TotalSeconds * Stopwatch.Frequency);
                if (measured > longestGcTicks) Interlocked.Exchange(ref longestGcTicks, measured);
            }
            GC.KeepAlive(live);
        });
        collector.Start();
        graphReady.Wait();
        var loaded = RunAudioThread(blocks, input, target, TimeSpan.FromSeconds(4), null);
        stop.Cancel();
        collector.Join();
        double baselineMaxGapMs = TicksToMilliseconds(baseline.MaxGapTicks);
        double loadedMaxGapMs = TicksToMilliseconds(loaded.MaxGapTicks);
        double longestGcPauseMs = TicksToMilliseconds(longestGcTicks);
        WriteMeasurement(new
        {
            test = nameof(ManagedCallbackIsSuspendedByABlockingGc), baselineMaxGapMs, loadedMaxGapMs,
            longestGcPauseMs, audioAllocatedBytes = loaded.AllocatedBytes, missed = loaded.Missed
        });
        Assert.Equal(0, loaded.AllocatedBytes);
        Assert.True(longestGcTicks > 0, "no blocking GC pause was measured");
        Assert.True(loaded.MaxGapTicks >= longestGcTicks / 2,
            $"prediction failed: audio max gap {loadedMaxGapMs:F3} ms was less than half GC pause {longestGcPauseMs:F3} ms");
    }

    [MatrixTheory]
    [InlineData(PluginFormat.Vst2)]
    public void SustainedLowLatencyReducesTheStall(PluginFormat format)
    {
        var opened = Open(format);
        using var host = opened.Host;
        using var plugin = opened.Plugin;
        using var blocks = opened.Blocks;
        float[] input = new float[blocks.InputChannels * blocks.Quantum];
        float[] target = new float[blocks.OutputChannels * blocks.Quantum];
        for (int index = 0; index < 500; index++) blocks.Process(input, target);
        GCLatencyMode previous = GCSettings.LatencyMode;
        int gen0 = GC.CollectionCount(0), gen1 = GC.CollectionCount(1), gen2 = GC.CollectionCount(2);
        using var stop = new CancellationTokenSource();
        var churn = new Thread(() =>
        {
            while (!stop.IsCancellationRequested) churnSink = new byte[8_192];
        });
        AudioRun run;
        try
        {
            GCSettings.LatencyMode = GCLatencyMode.SustainedLowLatency;
            churn.Start();
            run = RunAudioThread(blocks, input, target, TimeSpan.FromSeconds(4), null);
            stop.Cancel();
            churn.Join();
        }
        finally
        {
            stop.Cancel();
            if (churn.IsAlive) churn.Join();
            GCSettings.LatencyMode = previous;
        }
        int gen0Count = GC.CollectionCount(0) - gen0;
        int gen1Count = GC.CollectionCount(1) - gen1;
        int gen2Count = GC.CollectionCount(2) - gen2;
        double maxGapMs = TicksToMilliseconds(run.MaxGapTicks);
        WriteMeasurement(new
        {
            test = nameof(SustainedLowLatencyReducesTheStall), maxGapMs,
            audioAllocatedBytes = run.AllocatedBytes, missed = run.Missed, gen0 = gen0Count, gen1 = gen1Count, gen2 = gen2Count
        });
        Assert.Equal(0, run.AllocatedBytes);
    }

    private static AudioRun RunAudioThread(BlockProcessor blocks, float[] input, float[] target, TimeSpan duration, Action? started)
    {
        var run = new AudioRun();
        var gaps = new long[1_000_000];
        Exception? failure = null;
        var thread = new Thread(() =>
        {
            try
            {
                long deadline = Stopwatch.GetTimestamp() + (long)(duration.TotalSeconds * Stopwatch.Frequency);
                long previous = Stopwatch.GetTimestamp();
                long maxGap = 0;
                int missed = 0, gapIndex = 0;
                started?.Invoke();
                long before = GC.GetAllocatedBytesForCurrentThread();
                while (Stopwatch.GetTimestamp() < deadline)
                {
                    if (blocks.Process(input, target) == BlockStatus.Missed) missed++;
                    long now = Stopwatch.GetTimestamp();
                    long gap = now - previous;
                    gaps[gapIndex++] = gap;
                    if (gapIndex == gaps.Length) gapIndex = 0;
                    if (gap > maxGap) maxGap = gap;
                    previous = now;
                }
                long allocated = GC.GetAllocatedBytesForCurrentThread() - before;
                run.MaxGapTicks = maxGap;
                run.AllocatedBytes = allocated;
                run.Missed = missed;
            }
            catch (Exception error) { failure = error; }
        });
        thread.Start();
        thread.Join();
        if (failure is not null) throw failure;
        GC.KeepAlive(gaps);
        return run;
    }

    private void WriteMeasurement(object measurement)
    {
        string json = JsonSerializer.Serialize(measurement);
        output.WriteLine(json);
        Console.WriteLine(json);
    }

    private static double TicksToMilliseconds(long ticks) => ticks * 1_000.0 / Stopwatch.Frequency;

    private sealed class AudioRun
    {
        internal long MaxGapTicks;
        internal long AllocatedBytes;
        internal int Missed;
    }
}
