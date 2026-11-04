namespace VstAudio;

/// <summary>A worker or native hosting operation failed.</summary>
public class VstAudioException : Exception
{
    /// <summary>Creates an exception with the native message and stable error kind.</summary>
    public VstAudioException(string message, string kind) : base(message) => Kind = kind;

    /// <summary>The native error kind, suitable for programmatic handling.</summary>
    public string Kind { get; }
}

/// <summary>No worker executable was found at the requested or bundled locations.</summary>
public sealed class WorkerNotFoundException : VstAudioException
{
    /// <summary>Creates a worker lookup error describing the locations tried.</summary>
    public WorkerNotFoundException(string message) : base(message, "worker_not_found") { }
}

/// <summary>Scanning or starting a plugin failed; Kind identifies the native StartErrorKind.</summary>
public sealed class StartException : VstAudioException
{
    /// <summary>Creates an exception with the native startup error and kind.</summary>
    public StartException(string message, string kind) : base(message, kind) { }
}

/// <summary>Offline rendering failed, rather than returning silence or a partial render.</summary>
public sealed class RenderException : VstAudioException
{
    /// <summary>Creates an exception with kind invalid_buffer, not_running, recovered, timed_out or lost.</summary>
    public RenderException(string message, string kind) : base(message, kind) { }
}

/// <summary>A control request exceeded its deadline.</summary>
public sealed class RequestTimeoutException : VstAudioException
{
    /// <summary>Creates an exception with the native timeout message and kind.</summary>
    public RequestTimeoutException(string message, string kind = "request_timeout") : base(message, kind) { }
}
