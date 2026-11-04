using System.Text;

namespace VstAudio;

/// <summary>Scans and loads plugins in separate native worker processes.</summary>
public sealed class Host : IDisposable
{
    private readonly object gate = new();
    private readonly HostHandle handle;
    private readonly string workerPath;

    /// <summary>Creates a host with an explicit worker, or resolves the environment and bundled worker locations.</summary>
    /// <exception cref="WorkerNotFoundException">No worker executable exists at the requested locations.</exception>
    /// <exception cref="PlatformNotSupportedException">The process OS or architecture is unsupported.</exception>
    public unsafe Host(string? workerPath = null)
    {
        this.workerPath = Platform.ResolveWorker(workerPath);
        byte[] path = Encoding.UTF8.GetBytes(this.workerPath);
        fixed (byte* pointer = path)
        {
            Native.Check(Native.HostNew(pointer, (nuint)path.Length, out nint result));
            handle = new HostHandle(result);
        }
    }

    /// <summary>The resolved absolute worker executable path.</summary>
    public string WorkerPath
    {
        get { lock (gate) { ThrowIfDisposed(); return workerPath; } }
    }

    /// <summary>Scans a plugin in a disposable worker. A null format requests native extension inference.</summary>
    /// <param name="path">The plugin binary or bundle path.</param>
    /// <param name="format">VST2, VST3, or null to infer from the extension.</param>
    /// <param name="sandboxed">Use the worker's default sandbox policy.</param>
    public unsafe IReadOnlyList<PluginClass> Scan(string path, PluginFormat? format = null, bool sandboxed = false)
    {
        lock (gate)
        {
            ThrowIfDisposed();
            ArgumentNullException.ThrowIfNull(path);
            byte[] bytes = Encoding.UTF8.GetBytes(path);
            fixed (byte* pointer = bytes)
            {
                Native.Check(Native.HostScan(handle, pointer, (nuint)bytes.Length, (int?)format ?? 0, sandboxed ? (byte)1 : (byte)0, out nint result));
                using var list = new ClassListHandle(result);
                var classes = new PluginClass[checked((int)Native.ClassListLength(list))];
                for (int index = 0; index < classes.Length; index++)
                {
                    Native.Check(Native.ClassListGet(list, (nuint)index, out var info));
                    classes[index] = new PluginClass(info);
                }
                return Array.AsReadOnly(classes);
            }
        }
    }

    /// <summary>Starts a plugin worker synchronously. The returned plugin owns its lifetime independently of this host.</summary>
    /// <param name="path">The plugin binary or bundle path.</param>
    /// <param name="format">VST2, VST3, or null to infer from the extension.</param>
    /// <param name="options">Startup configuration, or null for defaults.</param>
    public unsafe Plugin Load(string path, PluginFormat? format = null, LoadOptions? options = null)
    {
        lock (gate)
        {
            ThrowIfDisposed();
            ArgumentNullException.ThrowIfNull(path);
            options ??= new LoadOptions();
            if (options.ClassId is { Length: not 16 })
                throw new ArgumentException("ClassId must contain exactly 16 bytes.", nameof(options));
            ArgumentOutOfRangeException.ThrowIfNegativeOrZero(options.SampleRate);
            ArgumentOutOfRangeException.ThrowIfNegativeOrZero(options.BlockSize);
            uint timeout = Native.Milliseconds(options.Timeout);
            byte[] bytes = Encoding.UTF8.GetBytes(path);
            fixed (byte* pointer = bytes)
            fixed (byte* classId = options.ClassId.GetValueOrDefault().Span)
            {
                Native.Check(Native.HostLoad(handle, pointer, (nuint)bytes.Length, (int?)format ?? 0, classId,
                    (uint)options.SampleRate, (uint)options.BlockSize, (int)options.SampleFormat, timeout, out nint result));
                var pluginHandle = new PluginHandle(result);
                try { return new Plugin(pluginHandle); }
                catch { pluginHandle.Dispose(); throw; }
            }
        }
    }

    /// <summary>Releases this host. Existing plugins remain usable. Repeated calls are harmless.</summary>
    public void Dispose()
    {
        lock (gate) handle.Dispose();
    }

    private void ThrowIfDisposed() => ObjectDisposedException.ThrowIf(handle.IsClosed, this);
}
