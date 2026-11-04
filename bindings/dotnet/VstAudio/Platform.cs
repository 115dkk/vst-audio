using System.Runtime.InteropServices;

namespace VstAudio;

internal static class Platform
{
    internal static string Rid => (RuntimeInformation.ProcessArchitecture, OperatingSystem.IsWindows(), OperatingSystem.IsLinux(), OperatingSystem.IsMacOS()) switch
    {
        (Architecture.X64, true, _, _) => "win-x64",
        (Architecture.X64, _, true, _) => "linux-x64",
        (Architecture.X64, _, _, true) => "osx-x64",
        (Architecture.Arm64, _, _, true) => "osx-arm64",
        _ => throw new PlatformNotSupportedException("VstAudio supports win-x64, linux-x64, osx-arm64 and osx-x64."),
    };

    internal static string ResolveWorker(string? explicitPath)
    {
        string rid = Rid;
        if (explicitPath is not null)
        {
            if (!File.Exists(explicitPath))
                throw new WorkerNotFoundException($"Worker not found: {explicitPath}");
            return Path.GetFullPath(explicitPath);
        }

        string worker = OperatingSystem.IsWindows() ? "vst-audio-worker.exe"
            : OperatingSystem.IsMacOS() ? "VstAudioWorker.app/Contents/MacOS/vst-audio-worker" : "vst-audio-worker";
        string? environment = Environment.GetEnvironmentVariable("VST_AUDIO_WORKER");
        if (!string.IsNullOrEmpty(environment) && File.Exists(environment))
            return Path.GetFullPath(environment);
        string bundled = Path.Combine(AppContext.BaseDirectory, "runtimes", rid, "native", worker);
        string adjacent = Path.Combine(AppContext.BaseDirectory, worker);
        foreach (string candidate in new[] { bundled, adjacent })
        {
            if (!File.Exists(candidate))
                continue;
            if (!OperatingSystem.IsWindows())
            {
                UnixFileMode mode = File.GetUnixFileMode(candidate);
                UnixFileMode executable = mode;
                if ((mode & UnixFileMode.UserRead) != 0) executable |= UnixFileMode.UserExecute;
                if ((mode & UnixFileMode.GroupRead) != 0) executable |= UnixFileMode.GroupExecute;
                if ((mode & UnixFileMode.OtherRead) != 0) executable |= UnixFileMode.OtherExecute;
                if (executable != mode)
                    File.SetUnixFileMode(candidate, executable);
            }
            return Path.GetFullPath(candidate);
        }
        throw new WorkerNotFoundException($"Worker not found. Tried VST_AUDIO_WORKER ({environment ?? "not set"}), {bundled}, {adjacent}.");
    }
}
