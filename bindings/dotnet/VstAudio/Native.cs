using System.Reflection;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

namespace VstAudio;

internal static unsafe partial class Native
{
    private const string Library = "vst_audio_ffi";

    static Native()
    {
        NativeLibrary.SetDllImportResolver(typeof(Native).Assembly, Resolve);
    }

    private static nint Resolve(string name, Assembly assembly, DllImportSearchPath? searchPath)
    {
        if (name != Library)
            return 0;
        string rid = Platform.Rid;
        if (NativeLibrary.TryLoad(name, assembly, searchPath, out nint handle))
            return handle;
        string filename = OperatingSystem.IsWindows() ? "vst_audio_ffi.dll"
            : OperatingSystem.IsMacOS() ? "libvst_audio_ffi.dylib" : "libvst_audio_ffi.so";
        return NativeLibrary.Load(Path.Combine(AppContext.BaseDirectory, "runtimes", rid, "native", filename));
    }

    internal static uint Milliseconds(TimeSpan? timeout, int defaultSeconds = 10)
    {
        TimeSpan value = timeout ?? TimeSpan.FromSeconds(defaultSeconds);
        ArgumentOutOfRangeException.ThrowIfLessThan(value, TimeSpan.Zero, nameof(timeout));
        return (uint)Math.Min(value.Ticks / TimeSpan.TicksPerMillisecond, uint.MaxValue);
    }

    internal static string Text(byte* data, nuint length) =>
        Encoding.UTF8.GetString(new ReadOnlySpan<byte>(data, checked((int)length)));

    internal static void Check(int status)
    {
        if (status == 0)
            return;
        // Both strings belong to this OS thread. Read them before any other ABI call.
        string message = ErrorText(false);
        string kind = ErrorText(true);
        throw status switch
        {
            1 => new ArgumentException(message),
            2 => new StartException(message, kind),
            3 => new RenderException(message, kind),
            4 => new RequestTimeoutException(message, kind),
            _ => new VstAudioException(message, kind),
        };
    }

    private static string ErrorText(bool kind)
    {
        nuint length = kind ? LastErrorKind(null, 0) : LastError(null, 0);
        byte[] bytes = new byte[checked((int)length)];
        fixed (byte* pointer = bytes)
        {
            if (kind)
                LastErrorKind(pointer, length);
            else
                LastError(pointer, length);
        }
        return Encoding.UTF8.GetString(bytes);
    }

    [LibraryImport(Library, EntryPoint = "vsta_last_error")]
    private static partial nuint LastError(byte* buffer, nuint capacity);
    [LibraryImport(Library, EntryPoint = "vsta_last_error_kind")]
    private static partial nuint LastErrorKind(byte* buffer, nuint capacity);
    [LibraryImport(Library, EntryPoint = "vsta_host_new")]
    internal static partial int HostNew(byte* worker, nuint length, out nint host);
    [LibraryImport(Library, EntryPoint = "vsta_host_free")]
    internal static partial void HostFree(nint host);
    [LibraryImport(Library, EntryPoint = "vsta_host_scan")]
    internal static partial int HostScan(HostHandle host, byte* path, nuint length, int format, byte sandboxed, out nint list);
    [LibraryImport(Library, EntryPoint = "vsta_host_load")]
    internal static partial int HostLoad(HostHandle host, byte* path, nuint length, int format, byte* classId, uint sampleRate, uint blockSize, int sampleFormat, uint timeout, out nint plugin);
    [LibraryImport(Library, EntryPoint = "vsta_class_list_free")]
    internal static partial void ClassListFree(nint list);
    [LibraryImport(Library, EntryPoint = "vsta_class_list_len")]
    internal static partial nuint ClassListLength(ClassListHandle list);
    [LibraryImport(Library, EntryPoint = "vsta_class_list_get")]
    internal static partial int ClassListGet(ClassListHandle list, nuint index, out ClassInfo info);
    [LibraryImport(Library, EntryPoint = "vsta_plugin_free")]
    internal static partial void PluginFree(nint plugin);
    [LibraryImport(Library, EntryPoint = "vsta_plugin_info")]
    internal static partial int PluginInfo(PluginHandle plugin, out InstanceInfo info);
    [LibraryImport(Library, EntryPoint = "vsta_plugin_process_f32")]
    internal static partial int ProcessF32(PluginHandle plugin, float* input, nuint inputLength, nuint frames, nuint tail, float* output, nuint outputLength, uint timeout);
    [LibraryImport(Library, EntryPoint = "vsta_plugin_process_f64")]
    internal static partial int ProcessF64(PluginHandle plugin, double* input, nuint inputLength, nuint frames, nuint tail, double* output, nuint outputLength, uint timeout);
    [LibraryImport(Library, EntryPoint = "vsta_plugin_parameters")]
    internal static partial int Parameters(PluginHandle plugin, uint timeout, out nint list);
    [LibraryImport(Library, EntryPoint = "vsta_parameter_list_free")]
    internal static partial void ParameterListFree(nint list);
    [LibraryImport(Library, EntryPoint = "vsta_parameter_list_len")]
    internal static partial nuint ParameterListLength(ParameterListHandle list);
    [LibraryImport(Library, EntryPoint = "vsta_parameter_list_get")]
    internal static partial int ParameterListGet(ParameterListHandle list, nuint index, out ParameterInfo info);
    [LibraryImport(Library, EntryPoint = "vsta_plugin_set_parameter")]
    internal static partial int SetParameter(PluginHandle plugin, uint id, double value, uint timeout);
    [LibraryImport(Library, EntryPoint = "vsta_plugin_save_state")]
    internal static partial int SaveState(PluginHandle plugin, uint timeout, out nint bytes);
    [LibraryImport(Library, EntryPoint = "vsta_bytes_free")]
    internal static partial void BytesFree(nint bytes);
    [LibraryImport(Library, EntryPoint = "vsta_bytes_len")]
    internal static partial nuint BytesLength(BytesHandle bytes);
    [LibraryImport(Library, EntryPoint = "vsta_bytes_data")]
    internal static partial byte* BytesData(BytesHandle bytes);
    [LibraryImport(Library, EntryPoint = "vsta_plugin_restore_state")]
    internal static partial int RestoreState(PluginHandle plugin, byte* data, nuint length, uint timeout);
    [LibraryImport(Library, EntryPoint = "vsta_plugin_open_editor")]
    internal static partial int OpenEditor(PluginHandle plugin, uint timeout);
    [LibraryImport(Library, EntryPoint = "vsta_plugin_close_editor")]
    internal static partial int CloseEditor(PluginHandle plugin, uint timeout);

    [StructLayout(LayoutKind.Sequential)]
    internal struct ClassInfo
    {
        internal fixed byte Id[16];
        internal byte* Name;
        internal nuint NameLength;
        internal byte* Vendor;
        internal nuint VendorLength;
        internal byte* Category;
        internal nuint CategoryLength;
    }

    [StructLayout(LayoutKind.Sequential)]
    internal struct InstanceInfo
    {
        internal uint InputChannels, OutputChannels, SampleRate, BlockSize;
        internal int SampleFormat;
        internal uint Latency;
    }

    [StructLayout(LayoutKind.Sequential)]
    internal struct ParameterInfo
    {
        internal uint Id;
        internal byte* Name;
        internal nuint NameLength;
        internal byte* Unit;
        internal nuint UnitLength;
        internal double Value, Default;
        internal byte HasDefault;
        internal uint Steps;
        internal byte HasSteps, Automatable, ReadOnly, Hidden;
    }
}

internal sealed class HostHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    internal HostHandle(nint value) : base(true) => SetHandle(value);
    protected override bool ReleaseHandle() { Native.HostFree(handle); return true; }
}

internal sealed class PluginHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    internal PluginHandle(nint value) : base(true) => SetHandle(value);
    protected override bool ReleaseHandle() { Native.PluginFree(handle); return true; }
}

internal sealed class ClassListHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    internal ClassListHandle(nint value) : base(true) => SetHandle(value);
    protected override bool ReleaseHandle() { Native.ClassListFree(handle); return true; }
}

internal sealed class ParameterListHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    internal ParameterListHandle(nint value) : base(true) => SetHandle(value);
    protected override bool ReleaseHandle() { Native.ParameterListFree(handle); return true; }
}

internal sealed class BytesHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    internal BytesHandle(nint value) : base(true) => SetHandle(value);
    protected override bool ReleaseHandle() { Native.BytesFree(handle); return true; }
}
