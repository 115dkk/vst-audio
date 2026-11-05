namespace VstAudio;

/// <summary>The result of one experimental realtime block call.</summary>
public enum BlockStatus
{
    /// <summary>The exact due output block was written.</summary>
    Delivered = 0,
    /// <summary>The exact due block was not ready and output was left untouched.</summary>
    Missed = 1,
    /// <summary>The sample format or planar buffer lengths were invalid.</summary>
    InvalidBuffer = 2,
}

/// <summary>A block-synchronous realtime endpoint. One thread may call it at a time.</summary>
/// <remarks>Buffers are planar and contain <c>channels * Quantum</c> samples. Dispose may run on another thread; SafeHandle keeps an in-flight native call alive.</remarks>
[System.Diagnostics.CodeAnalysis.Experimental("VSTA0001")]
public sealed class BlockProcessor : IDisposable
{
    private readonly BlockHandle handle;
    private readonly Native.BlockInfo info;

    internal BlockProcessor(BlockHandle handle)
    {
        this.handle = handle;
        Native.Check(Native.BlocksInfo(handle, out info));
    }

    /// <summary>The number of planar input channels.</summary>
    public int InputChannels => checked((int)info.InputChannels);
    /// <summary>The number of planar output channels.</summary>
    public int OutputChannels => checked((int)info.OutputChannels);
    /// <summary>The exact frames in every block plane.</summary>
    public int Quantum => checked((int)info.Quantum);
    /// <summary>The fixed pipeline depth in blocks.</summary>
    public int Depth => checked((int)info.Depth);
    /// <summary>The native sample precision fixed at endpoint creation.</summary>
    public SampleFormat SampleFormat => (SampleFormat)info.SampleFormat;

    /// <summary>Gets the current total latency in frames. This is safe to call from the audio thread.</summary>
    public int GetLatencyFrames()
    {
        unsafe
        {
            Native.BlocksInfo(handle, out var current);
            return checked((int)current.TotalLatency);
        }
    }

    /// <summary>Submits one flat-planar Float32 block without waiting.</summary>
    public unsafe BlockStatus Process(ReadOnlySpan<float> input, Span<float> output)
    {
        if (input.Length != checked(InputChannels * Quantum) || output.Length != checked(OutputChannels * Quantum))
            return BlockStatus.InvalidBuffer;
        int status;
        fixed (float* source = input)
        fixed (float* destination = output)
            status = Native.BlocksProcessF32(handle, source, (nuint)input.Length, destination, (nuint)output.Length);
        // Only a panic caught at the C ABI returns anything else; never on the normal path.
        if ((uint)status > (uint)BlockStatus.InvalidBuffer)
            Native.Check(status);
        return (BlockStatus)status;
    }

    /// <summary>Submits one flat-planar Float64 block without waiting.</summary>
    public unsafe BlockStatus Process(ReadOnlySpan<double> input, Span<double> output)
    {
        if (input.Length != checked(InputChannels * Quantum) || output.Length != checked(OutputChannels * Quantum))
            return BlockStatus.InvalidBuffer;
        int status;
        fixed (double* source = input)
        fixed (double* destination = output)
            status = Native.BlocksProcessF64(handle, source, (nuint)input.Length, destination, (nuint)output.Length);
        // Only a panic caught at the C ABI returns anything else; never on the normal path.
        if ((uint)status > (uint)BlockStatus.InvalidBuffer)
            Native.Check(status);
        return (BlockStatus)status;
    }

    /// <summary>Releases this endpoint after any in-flight native call returns. Repeated calls are harmless.</summary>
    public void Dispose() => handle.Dispose();
}
