using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Text.Json;
using System.Text.Json.Serialization;
using VstAudio;

namespace VstAudioExamples;

/// <summary>Everything a window draws. Views redraw from this snapshot only.</summary>
/// <param name="Plugin">The plugin's file or bundle name.</param>
/// <param name="Status">One line about the current or last operation.</param>
/// <param name="Details">Several lines: the plugin's configuration, the last render or the last error.</param>
/// <param name="Ready">A plugin is loaded and no VstAudio call is in flight. Enables all three buttons.</param>
public sealed record SessionState(string Plugin, string Status, string Details, bool Ready);

/// <summary>Toolkit-independent controller shared by the WPF and WinUI 3 examples.</summary>
/// <remarks>
/// Every VstAudio call blocks until the worker answers, so each one runs on the thread pool and the UI
/// thread never calls VstAudio. Call the public members on the UI thread: each awaits its work, and the
/// await resumes on the UI thread's synchronization context, where the session updates
/// <see cref="State"/> and raises <see cref="Changed"/>. The session never touches UI types.
/// </remarks>
public sealed class Session
{
    public const int SampleRate = 48000;
    public const int BlockSize = 256;
    public static readonly TimeSpan HeartbeatInterval = TimeSpan.FromMilliseconds(50);
    // A smoke run that has not finished by then fails, even if every reply is slow but still arriving.
    // The test runner gives the process a longer outer timeout.
    public static readonly TimeSpan SmokeDeadline = TimeSpan.FromSeconds(45);
    private static readonly TimeSpan ShutdownTimeout = TimeSpan.FromSeconds(15);
    private static readonly JsonSerializerOptions SummaryJson = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    };

    private readonly object gate = new();
    private readonly string toolkit;
    private readonly Summary summary;
    private Plugin? plugin; // guarded by gate
    private bool closed; // guarded by gate
    private Task? loading; // guarded by gate
    private int heartbeats;
    private string configuration = "";

    /// <param name="toolkit">"wpf" or "winui", written to the smoke summary.</param>
    /// <param name="options">The parsed command line.</param>
    public Session(string toolkit, Options options)
    {
        this.toolkit = toolkit;
        Options = options;
        summary = new Summary { Toolkit = toolkit, Plugin = options.PluginName };
        State = new SessionState(options.PluginName, "Opening plugin…", "", Ready: false);
    }

    public Options Options { get; }

    public SessionState State { get; private set; }

    private bool Closed
    {
        get { lock (gate) return closed; }
    }

    /// <summary>Raised on the UI thread after every change of <see cref="State"/>.</summary>
    public event Action<SessionState>? Changed;

    /// <summary>Call from a UI timer every <see cref="HeartbeatInterval"/>.</summary>
    public void Heartbeat() => Interlocked.Increment(ref heartbeats);

    /// <summary>Starts the worker and loads the plugin. Call once, after the window exists.</summary>
    /// <returns>False after reporting a failure.</returns>
    public async Task<bool> LoadAsync()
    {
        Task<Loaded> work;
        lock (gate)
        {
            if (loading is not null || closed)
                return false;
            work = Task.Run(Load);
            loading = work;
        }
        Loaded loaded;
        try { loaded = await work; }
        catch (OperationCanceledException) when (Closed) { return false; }
        catch (Exception error) { Fail("load", error); return false; }
        summary.Worker = loaded.Worker;
        summary.Inputs = loaded.Inputs;
        summary.Outputs = loaded.Outputs;
        summary.Latency = loaded.Latency;
        configuration = string.Create(CultureInfo.InvariantCulture,
            $"{loaded.Inputs} in, {loaded.Outputs} out, latency {loaded.Latency} frames\n" +
            $"{loaded.SampleRate} Hz, {loaded.BlockSize}-frame blocks\n" +
            $"Worker: {loaded.Worker}");
        Update(State with { Status = "Ready", Details = configuration, Ready = true });
        return true;
    }

    /// <summary>Opens the plugin's editor. Its window belongs to the worker process.</summary>
    /// <returns>False if no plugin is ready or after reporting a failure.</returns>
    public async Task<bool> OpenEditorAsync()
    {
        if (Begin("Opening editor…") is not { } current)
            return false;
        try { await Task.Run(() => current.OpenEditor()); }
        catch (Exception error) { Fail("open editor", error); return false; }
        Update(State with { Status = "Editor open in the worker process", Ready = true });
        return true;
    }

    /// <summary>Closes the plugin's editor in the worker process.</summary>
    /// <returns>False if no plugin is ready or after reporting a failure.</returns>
    public async Task<bool> CloseEditorAsync()
    {
        if (Begin("Closing editor…") is not { } current)
            return false;
        try { await Task.Run(() => current.CloseEditor()); }
        catch (Exception error) { Fail("close editor", error); return false; }
        Update(State with { Status = "Editor closed", Ready = true });
        return true;
    }

    /// <summary>Renders half a second of quiet sines through the plugin and checks the output.</summary>
    /// <returns>False if no plugin is ready or after reporting a failure.</returns>
    public async Task<bool> RenderAsync()
    {
        if (Begin("Rendering a test tone…") is not { } current)
            return false;
        Rendered rendered;
        try { rendered = await Task.Run(() => Render(current)); }
        catch (Exception error) { Fail("render", error); return false; }
        summary.Peaks = rendered.Peaks;
        string peaks = string.Join(", ", rendered.Peaks.Select(peak => peak.ToString("F3", CultureInfo.InvariantCulture)));
        string last = string.Create(CultureInfo.InvariantCulture,
            $"Last render: {rendered.Frames} frames in {rendered.Seconds:F3} s, output peaks {peaks}");
        Update(State with { Status = "Rendered", Details = $"{configuration}\n{last}", Ready = true });
        return true;
    }

    /// <summary>
    /// Loads, opens and closes the editor, renders, checks the result and writes one JSON summary line to
    /// standard output and to <see cref="Options.Summary"/>. Gives up after <see cref="SmokeDeadline"/>.
    /// </summary>
    /// <returns>The process exit code: 0, or 1 on the first failure.</returns>
    public async Task<int> RunSmokeAsync()
    {
        Task<bool> sequence = SmokeSequenceAsync();
        int code;
        if (await Task.WhenAny(sequence, Task.Delay(SmokeDeadline)) == sequence)
            code = await sequence ? 0 : 1;
        else
        {
            Fail("smoke", new TimeoutException($"no result within {SmokeDeadline.TotalSeconds:F0} s"));
            code = 1;
        }
        return WriteSummary(code);
    }

    /// <summary>
    /// Disposes the plugin on the thread pool, which also stops its worker. Waits up to 15 s for a call in
    /// flight. A load in flight disposes its plugin itself when it finishes. Safe to call more than once.
    /// </summary>
    public async Task ShutdownAsync()
    {
        Plugin? current;
        Task? pending;
        lock (gate)
        {
            closed = true;
            (current, plugin) = (plugin, null);
            pending = loading;
        }
        Task stop = Task.WhenAll(pending ?? Task.CompletedTask, Task.Run(() => current?.Dispose()));
        if (await Task.WhenAny(stop, Task.Delay(ShutdownTimeout)) != stop)
            Console.Error.WriteLine($"{toolkit}: the plugin did not stop within {ShutdownTimeout.TotalSeconds:F0} s");
    }

    /// <summary>Quiet sines, one pitch per channel: 0.25 sin(2π (440 + 110 channel) t).</summary>
    public static float[][] TestTone(int channels, int frames, int sampleRate)
    {
        var tone = new float[channels][];
        for (int channel = 0; channel < channels; channel++)
        {
            double frequency = 440.0 + 110.0 * channel;
            tone[channel] = new float[frames];
            for (int frame = 0; frame < frames; frame++)
                tone[channel][frame] = (float)(0.25 * Math.Sin(2 * Math.PI * frequency * frame / sampleRate));
        }
        return tone;
    }

    // Thread pool ------------------------------------------------------------

    private Loaded Load()
    {
        // The plugin keeps its worker after the host is disposed.
        using var host = new Host(Options.Worker);
        Plugin loaded = host.Load(Options.Plugin, null, new LoadOptions { SampleRate = SampleRate, BlockSize = BlockSize });
        try
        {
            var result = new Loaded(host.WorkerPath, loaded.InputChannels, loaded.OutputChannels, loaded.Latency,
                loaded.SampleRate, loaded.BlockSize);
            lock (gate)
            {
                if (!closed)
                {
                    plugin = loaded;
                    return result;
                }
            }
            throw new OperationCanceledException("The window closed while the plugin was loading.");
        }
        catch
        {
            loaded.Dispose();
            throw;
        }
    }

    private static Rendered Render(Plugin plugin)
    {
        int inputs = plugin.InputChannels;
        int outputs = plugin.OutputChannels;
        int frames = plugin.SampleRate / 2;
        float[][] input = TestTone(inputs, frames, plugin.SampleRate);
        var clock = Stopwatch.StartNew();
        float[][] output = inputs > 0 ? plugin.Process(input) : RenderWithoutInput(plugin, outputs, frames);
        double seconds = clock.Elapsed.TotalSeconds;
        if (output.Length != outputs || output.Any(channel => channel.Length != frames))
            throw new InvalidDataException(
                $"render returned {output.Length} channels of [{string.Join(", ", output.Select(channel => channel.Length))}] frames, " +
                $"expected {outputs} of {frames}");
        if (output.Any(channel => channel.Any(sample => !float.IsFinite(sample))))
            throw new InvalidDataException("render returned a non-finite sample");
        double[] peaks = output.Select(channel => Math.Round(channel.Max(sample => (double)Math.Abs(sample)), 4)).ToArray();
        return new Rendered(frames, seconds, peaks);
    }

    // The jagged overload takes its frame count from the input channels, so an instrument
    // without inputs needs the span overload to render a nonzero length.
    private static float[][] RenderWithoutInput(Plugin plugin, int outputs, int frames)
    {
        var planar = new float[outputs * frames];
        plugin.Process(ReadOnlySpan<float>.Empty, frames, 0, planar);
        return Enumerable.Range(0, outputs).Select(channel => planar[(channel * frames)..((channel + 1) * frames)]).ToArray();
    }

    // UI thread --------------------------------------------------------------

    private async Task<bool> SmokeSequenceAsync()
    {
        if (!await LoadAsync() || !await OpenEditorAsync() || !await CloseEditorAsync() || !await RenderAsync())
            return false;
        if (Volatile.Read(ref heartbeats) == 0)
        {
            Fail("smoke", new InvalidOperationException("the UI timer never ran while the plugin was busy"));
            return false;
        }
        if (summary.Inputs > 0 && !(summary.Peaks ?? []).Any(peak => peak > 0))
        {
            Fail("smoke", new InvalidOperationException("the plugin returned silence for a test tone"));
            return false;
        }
        return true;
    }

    private Plugin? Begin(string status)
    {
        Plugin? current;
        lock (gate)
            current = plugin;
        if (!State.Ready || current is null)
            return null;
        Update(State with { Status = status, Ready = false });
        return current;
    }

    private void Fail(string label, Exception error)
    {
        string message = $"{error.GetType().Name}: {error.Message}";
        Console.Error.WriteLine($"{toolkit}: {label} failed: {message}");
        summary.Error ??= $"{label}: {error.Message}";
        bool loaded;
        lock (gate)
            loaded = plugin is not null;
        string details = $"{configuration}\n{message}".TrimStart('\n');
        Update(label switch
        {
            "load" => State with { Status = "The plugin could not be opened. Check its path and restart.", Details = message, Ready = false },
            // A smoke timeout can leave a call in flight, so the buttons keep their state.
            "smoke" => State with { Status = "The smoke test failed.", Details = details },
            _ => State with { Status = $"Could not {label}.", Details = details, Ready = loaded },
        });
    }

    private int WriteSummary(int code)
    {
        summary.Heartbeats = Volatile.Read(ref heartbeats);
        summary.Exit = code;
        if (Options.Summary is { } path)
        {
            try
            {
                File.WriteAllText(path, JsonSerializer.Serialize(summary, SummaryJson) + "\n");
            }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException)
            {
                Console.Error.WriteLine($"{toolkit}: could not write {path}: {error.Message}");
                summary.Error ??= $"summary: {error.Message}";
                summary.Exit = code = 1;
            }
        }
        Console.Out.WriteLine(JsonSerializer.Serialize(summary, SummaryJson));
        Console.Out.Flush();
        return code;
    }

    private void Update(SessionState state)
    {
        State = state;
        Changed?.Invoke(state);
    }

    private sealed record Loaded(string Worker, int Inputs, int Outputs, int Latency, int SampleRate, int BlockSize);

    private sealed record Rendered(int Frames, double Seconds, double[] Peaks);

    // Property order is the JSON order; null properties are left out.
    private sealed class Summary
    {
        public required string Toolkit { get; init; }
        public required string Plugin { get; init; }
        public string? Worker { get; set; }
        public int? Inputs { get; set; }
        public int? Outputs { get; set; }
        public int? Latency { get; set; }
        public int Heartbeats { get; set; }
        public double[]? Peaks { get; set; }
        public int Exit { get; set; }
        public string? Error { get; set; }
    }
}
