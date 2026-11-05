using System.IO;

namespace VstAudioExamples;

/// <summary>The command line both example apps accept.</summary>
/// <param name="Plugin">Absolute path of the VST2 binary or bundle, or the .vst3 bundle.</param>
/// <param name="Worker">Worker executable, or null for VST_AUDIO_WORKER and then the bundled worker.</param>
/// <param name="Smoke">Run the automatic lifecycle check and exit.</param>
/// <param name="Summary">Absolute path of a file that receives the smoke summary line, or null.</param>
public sealed record Options(string Plugin, string? Worker, bool Smoke, string? Summary)
{
    public const string Usage = "Usage: --plugin <path> [--worker <path>] [--smoke] [--summary <file>]";

    /// <summary>The plugin's file or bundle name, shown in the window and the summary.</summary>
    public string PluginName => Path.GetFileName(Path.TrimEndingDirectorySeparator(Plugin));

    /// <summary>Parses this process's command line.</summary>
    /// <remarks>
    /// Unpackaged WinUI apps do not receive their arguments in <c>OnLaunched</c>, so both apps read
    /// <see cref="Environment.GetCommandLineArgs"/>, whose first element is the program itself.
    /// </remarks>
    /// <exception cref="UsageException">The arguments are missing or malformed.</exception>
    public static Options FromCommandLine() => Parse(Environment.GetCommandLineArgs()[1..]);

    /// <exception cref="UsageException">The arguments are missing or malformed.</exception>
    public static Options Parse(IReadOnlyList<string> args)
    {
        bool smoke = args.Contains("--smoke");
        string? plugin = null, worker = null, summary = null;
        for (int index = 0; index < args.Count; index++)
        {
            switch (args[index])
            {
                case "--smoke": break;
                case "--plugin": plugin = Value(ref index); break;
                case "--worker": worker = Value(ref index); break;
                case "--summary": summary = Path.GetFullPath(Value(ref index)); break;
                default: throw new UsageException($"Unknown argument: {args[index]}", smoke);
            }
        }
        if (plugin is null)
            throw new UsageException("--plugin is required.", smoke);
        return new Options(Path.GetFullPath(plugin), worker, smoke, summary);

        string Value(ref int index)
        {
            if (index + 1 >= args.Count || args[index + 1].StartsWith("--", StringComparison.Ordinal))
                throw new UsageException($"{args[index]} needs a value.", smoke);
            return args[++index];
        }
    }
}

/// <summary>Bad command-line arguments. The apps report it and exit with code 2.</summary>
public sealed class UsageException(string message, bool smoke) : Exception($"{message}\n{Options.Usage}")
{
    /// <summary>Whether <c>--smoke</c> was given; a smoke run never waits for a message box.</summary>
    public bool Smoke { get; } = smoke;
}
