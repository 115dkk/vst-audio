namespace VstAudio;

/// <summary>A plugin instance in its own worker. Calls are serialized and block the calling thread.</summary>
/// <remarks>Offline processing compensates reported latency. This is not a realtime audio callback API.</remarks>
public sealed class Plugin : IDisposable
{
    private readonly object gate = new();
    private readonly PluginHandle handle;
    private readonly Native.InstanceInfo configuration;
    private bool blocksTaken;

    internal Plugin(PluginHandle handle)
    {
        this.handle = handle;
        Native.Check(Native.PluginInfo(handle, out configuration));
    }

    private Native.InstanceInfo Info
    {
        get
        {
            lock (gate)
            {
                ThrowIfDisposed();
                Native.Check(Native.PluginInfo(handle, out var info));
                return info;
            }
        }
    }

    /// <summary>The number of planar input channels.</summary>
    public int InputChannels => checked((int)Info.InputChannels);
    /// <summary>The number of planar output channels.</summary>
    public int OutputChannels => checked((int)Info.OutputChannels);
    /// <summary>The configured sample rate in hertz.</summary>
    public int SampleRate => checked((int)Info.SampleRate);
    /// <summary>The configured frames per native processing block.</summary>
    public int BlockSize => checked((int)Info.BlockSize);
    /// <summary>The native sample precision of this instance.</summary>
    public SampleFormat SampleFormat => (SampleFormat)Info.SampleFormat;
    /// <summary>The plugin's reported latency in frames, compensated during rendering.</summary>
    public int Latency => checked((int)Info.Latency);

    /// <summary>Renders Float32 input[channel][frame], returning OutputChannels arrays of frames + tail samples.</summary>
    /// <remarks>Channels must have equal lengths. For a zero-input instrument, use the span overload to specify a nonzero frame count.</remarks>
    /// <param name="input">Planar input channels.</param>
    /// <param name="tail">Additional zero-input frames to render.</param>
    /// <param name="timeout">Per-block timeout, defaulting to 10 seconds.</param>
    public float[][] Process(float[][] input, int tail = 0, TimeSpan? timeout = null)
    {
        lock (gate)
        {
            ThrowIfDisposed();
            ThrowIfBlocksTaken();
            var info = configuration;
            ValidateFormat(info, SampleFormat.Float32);
            float[] planar = Flatten(input, info, tail, out int frames, out int outputLength);
            float[] output = new float[outputLength];
            Process(planar, frames, tail, output, timeout);
            return Split(output, checked((int)info.OutputChannels), frames + tail);
        }
    }

    /// <summary>Renders Float64 input[channel][frame] without converting through Float32.</summary>
    /// <remarks>Channels must have equal lengths. For a zero-input instrument, use the span overload to specify a nonzero frame count.</remarks>
    /// <param name="input">Planar input channels.</param>
    /// <param name="tail">Additional zero-input frames to render.</param>
    /// <param name="timeout">Per-block timeout, defaulting to 10 seconds.</param>
    public double[][] Process(double[][] input, int tail = 0, TimeSpan? timeout = null)
    {
        lock (gate)
        {
            ThrowIfDisposed();
            ThrowIfBlocksTaken();
            var info = configuration;
            ValidateFormat(info, SampleFormat.Float64);
            double[] planar = Flatten(input, info, tail, out int frames, out int outputLength);
            double[] output = new double[outputLength];
            Process(planar, frames, tail, output, timeout);
            return Split(output, checked((int)info.OutputChannels), frames + tail);
        }
    }

    /// <summary>Renders flat planar Float32 buffers. Input and output must not overlap.</summary>
    /// <param name="input">Exactly InputChannels * frames samples, with each channel contiguous.</param>
    /// <param name="frames">Input frames per channel.</param>
    /// <param name="tail">Additional zero-input frames.</param>
    /// <param name="output">Exactly OutputChannels * (frames + tail) writable samples.</param>
    /// <param name="timeout">Per-block timeout, defaulting to 10 seconds.</param>
    public unsafe void Process(ReadOnlySpan<float> input, int frames, int tail, Span<float> output, TimeSpan? timeout = null)
    {
        lock (gate)
        {
            ThrowIfDisposed();
            ThrowIfBlocksTaken();
            ValidateBuffers(input, frames, tail, output, configuration, SampleFormat.Float32);
            uint milliseconds = Native.Milliseconds(timeout);
            fixed (float* source = input)
            fixed (float* destination = output)
                Native.Check(Native.ProcessF32(handle, source, (nuint)input.Length, (nuint)frames, (nuint)tail, destination, (nuint)output.Length, milliseconds));
        }
    }

    /// <summary>Renders flat planar Float64 buffers without precision conversion. Buffers must not overlap.</summary>
    /// <param name="input">Exactly InputChannels * frames samples, with each channel contiguous.</param>
    /// <param name="frames">Input frames per channel.</param>
    /// <param name="tail">Additional zero-input frames.</param>
    /// <param name="output">Exactly OutputChannels * (frames + tail) writable samples.</param>
    /// <param name="timeout">Per-block timeout, defaulting to 10 seconds.</param>
    public unsafe void Process(ReadOnlySpan<double> input, int frames, int tail, Span<double> output, TimeSpan? timeout = null)
    {
        lock (gate)
        {
            ThrowIfDisposed();
            ThrowIfBlocksTaken();
            ValidateBuffers(input, frames, tail, output, configuration, SampleFormat.Float64);
            uint milliseconds = Native.Milliseconds(timeout);
            fixed (double* source = input)
            fixed (double* destination = output)
                Native.Check(Native.ProcessF64(handle, source, (nuint)input.Length, (nuint)frames, (nuint)tail, destination, (nuint)output.Length, milliseconds));
        }
    }

    /// <summary>Transfers the only experimental realtime block endpoint from this plugin.</summary>
    /// <remarks>After this succeeds, all offline <c>Process</c> overloads throw <see cref="InvalidOperationException"/>.</remarks>
    [System.Diagnostics.CodeAnalysis.Experimental("VSTA0001")]
    public BlockProcessor TakeBlockProcessor()
    {
        lock (gate)
        {
            ThrowIfDisposed();
            if (blocksTaken) throw new InvalidOperationException("The block processor has already been taken.");
            Native.Check(Native.TakeBlocks(handle, out nint result));
            var blockHandle = new BlockHandle(result);
            try
            {
                var processor = new BlockProcessor(blockHandle);
                blocksTaken = true;
                return processor;
            }
            catch
            {
                blockHandle.Dispose();
                throw;
            }
        }
    }

    /// <summary>Copies the current parameter list. The default control deadline is 10 seconds.</summary>
    public IReadOnlyList<Parameter> GetParameters(TimeSpan? timeout = null)
    {
        lock (gate)
        {
            ThrowIfDisposed();
            Native.Check(Native.Parameters(handle, Native.Milliseconds(timeout), out nint result));
            using var list = new ParameterListHandle(result);
            var parameters = new Parameter[checked((int)Native.ParameterListLength(list))];
            for (int index = 0; index < parameters.Length; index++)
            {
                Native.Check(Native.ParameterListGet(list, (nuint)index, out var info));
                parameters[index] = new Parameter(info);
            }
            return Array.AsReadOnly(parameters);
        }
    }

    /// <summary>Sets a normalized parameter value. The default control deadline is 10 seconds.</summary>
    public void SetParameter(uint id, double value, TimeSpan? timeout = null)
    {
        lock (gate)
        {
            ThrowIfDisposed();
            Native.Check(Native.SetParameter(handle, id, value, Native.Milliseconds(timeout)));
        }
    }

    /// <summary>Copies the plugin state into managed storage. The default control deadline is 10 seconds.</summary>
    public unsafe byte[] SaveState(TimeSpan? timeout = null)
    {
        lock (gate)
        {
            ThrowIfDisposed();
            Native.Check(Native.SaveState(handle, Native.Milliseconds(timeout), out nint result));
            using var bytes = new BytesHandle(result);
            int length = checked((int)Native.BytesLength(bytes));
            byte[] state = new ReadOnlySpan<byte>(Native.BytesData(bytes), length).ToArray();
            GC.KeepAlive(bytes);
            return state;
        }
    }

    /// <summary>Restores a previously saved state. The default control deadline is 10 seconds.</summary>
    public unsafe void RestoreState(ReadOnlySpan<byte> state, TimeSpan? timeout = null)
    {
        lock (gate)
        {
            ThrowIfDisposed();
            uint milliseconds = Native.Milliseconds(timeout);
            fixed (byte* pointer = state)
                Native.Check(Native.RestoreState(handle, pointer, (nuint)state.Length, milliseconds));
        }
    }

    /// <summary>Opens the plugin's editor in its worker. The default control deadline is 10 seconds.</summary>
    public void OpenEditor(TimeSpan? timeout = null)
    {
        lock (gate)
        {
            ThrowIfDisposed();
            Native.Check(Native.OpenEditor(handle, Native.Milliseconds(timeout)));
        }
    }

    /// <summary>Closes the plugin's worker editor. The default control deadline is 10 seconds.</summary>
    public void CloseEditor(TimeSpan? timeout = null)
    {
        lock (gate)
        {
            ThrowIfDisposed();
            Native.Check(Native.CloseEditor(handle, Native.Milliseconds(timeout)));
        }
    }

    /// <summary>Stops and releases the plugin. May run on any thread; repeated calls are harmless.</summary>
    public void Dispose()
    {
        lock (gate) handle.Dispose();
    }

    private void ThrowIfDisposed() => ObjectDisposedException.ThrowIf(handle.IsClosed, this);

    private void ThrowIfBlocksTaken()
    {
        if (blocksTaken) throw new InvalidOperationException("The realtime block processor owns this plugin's audio endpoint.");
    }

    private static void ValidateFormat(Native.InstanceInfo info, SampleFormat format)
    {
        if (info.SampleFormat != (int)format)
            throw new ArgumentException($"This instance requires {(SampleFormat)info.SampleFormat} samples.");
    }

    private static (int Input, int Output) Lengths(Native.InstanceInfo info, int frames, int tail)
    {
        ArgumentOutOfRangeException.ThrowIfNegative(frames);
        ArgumentOutOfRangeException.ThrowIfNegative(tail);
        try
        {
            return (checked((int)info.InputChannels * frames), checked((int)info.OutputChannels * checked(frames + tail)));
        }
        catch (OverflowException error)
        {
            throw new ArgumentException("The planar buffer lengths exceed the managed buffer limit.", error);
        }
    }

    private static void ValidateBuffers<T>(ReadOnlySpan<T> input, int frames, int tail, Span<T> output, Native.InstanceInfo info, SampleFormat format)
    {
        ValidateFormat(info, format);
        var lengths = Lengths(info, frames, tail);
        if (input.Length != lengths.Input || output.Length != lengths.Output)
            throw new ArgumentException("Planar buffer lengths must match the channel counts, frames and tail.");
        // Rust forms an immutable input slice and an exclusive output slice.
        if (input.Overlaps(output))
            throw new ArgumentException("Input and output buffers must not overlap.");
    }

    private static T[] Flatten<T>(T[][] input, Native.InstanceInfo info, int tail, out int frames, out int outputLength)
    {
        ArgumentNullException.ThrowIfNull(input);
        if (input.Length != info.InputChannels)
            throw new ArgumentException("Input channel count does not match this instance.", nameof(input));
        frames = input.Length == 0 ? 0 : input[0]?.Length ?? throw new ArgumentException("Input channels cannot be null.", nameof(input));
        foreach (T[] channel in input)
            if (channel is null || channel.Length != frames)
                throw new ArgumentException("Input channels must be non-null and have equal lengths.", nameof(input));
        var lengths = Lengths(info, frames, tail);
        outputLength = lengths.Output;
        T[] planar = new T[lengths.Input];
        for (int channel = 0; channel < input.Length; channel++)
            input[channel].CopyTo(planar, channel * frames);
        return planar;
    }

    private static T[][] Split<T>(T[] planar, int channels, int frames)
    {
        var output = new T[channels][];
        for (int channel = 0; channel < channels; channel++)
            output[channel] = planar.AsSpan(channel * frames, frames).ToArray();
        return output;
    }
}
