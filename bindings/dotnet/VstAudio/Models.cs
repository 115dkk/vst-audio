namespace VstAudio;

/// <summary>The plugin ABI to load or scan.</summary>
public enum PluginFormat
{
    /// <summary>VST2.</summary>
    Vst2 = 1,
    /// <summary>VST3.</summary>
    Vst3 = 2,
}

/// <summary>The native sample precision of an instance.</summary>
public enum SampleFormat
{
    /// <summary>32-bit floating point.</summary>
    Float32 = 0,
    /// <summary>64-bit floating point, requiring plugin support.</summary>
    Float64 = 1,
}

/// <summary>Configuration used when starting a plugin worker.</summary>
public sealed class LoadOptions
{
    /// <summary>A 16-byte class ID returned by scanning, or null to use the default class.</summary>
    public ReadOnlyMemory<byte>? ClassId { get; init; }
    /// <summary>Sample rate in hertz. Defaults to 48000.</summary>
    public int SampleRate { get; init; } = 48000;
    /// <summary>Frames per processing block. Defaults to 512.</summary>
    public int BlockSize { get; init; } = 512;
    /// <summary>Native sample precision. Defaults to Float32.</summary>
    public SampleFormat SampleFormat { get; init; } = SampleFormat.Float32;
    /// <summary>Startup deadline. Defaults to 30 seconds; negative values are invalid.</summary>
    public TimeSpan Timeout { get; init; } = TimeSpan.FromSeconds(30);
    /// <summary>Tell the plugin that it runs in realtime and enable taking its experimental block processor.</summary>
    [System.Diagnostics.CodeAnalysis.Experimental("VSTA0001")]
    public bool Realtime { get; init; }
}

/// <summary>A plugin class discovered in an isolated scan worker.</summary>
public sealed class PluginClass
{
    internal unsafe PluginClass(Native.ClassInfo info)
    {
        Id = new ReadOnlySpan<byte>(info.Id, 16).ToArray();
        Name = Native.Text(info.Name, info.NameLength);
        Vendor = Native.Text(info.Vendor, info.VendorLength);
        Category = Native.Text(info.Category, info.CategoryLength);
    }

    /// <summary>The 16-byte class ID used for explicit class selection.</summary>
    public ReadOnlyMemory<byte> Id { get; }
    /// <summary>The plugin's class name.</summary>
    public string Name { get; }
    /// <summary>The plugin vendor.</summary>
    public string Vendor { get; }
    /// <summary>The category reported by the plugin.</summary>
    public string Category { get; }
}

/// <summary>A copied snapshot of a plugin parameter.</summary>
public sealed class Parameter
{
    internal unsafe Parameter(Native.ParameterInfo info)
    {
        Id = info.Id;
        Name = Native.Text(info.Name, info.NameLength);
        Unit = Native.Text(info.Unit, info.UnitLength);
        Value = info.Value;
        Default = info.HasDefault != 0 ? info.Default : null;
        Steps = info.HasSteps != 0 ? info.Steps : null;
        Automatable = info.Automatable != 0;
        ReadOnly = info.ReadOnly != 0;
        Hidden = info.Hidden != 0;
    }

    /// <summary>The plugin parameter ID.</summary>
    public uint Id { get; }
    /// <summary>The parameter name.</summary>
    public string Name { get; }
    /// <summary>The parameter's display unit.</summary>
    public string Unit { get; }
    /// <summary>The current normalized value.</summary>
    public double Value { get; }
    /// <summary>The default normalized value, if reported.</summary>
    public double? Default { get; }
    /// <summary>The number of discrete steps, if reported.</summary>
    public uint? Steps { get; }
    /// <summary>Whether the plugin permits automation.</summary>
    public bool Automatable { get; }
    /// <summary>Whether the parameter is read-only.</summary>
    public bool ReadOnly { get; }
    /// <summary>Whether the parameter is hidden.</summary>
    public bool Hidden { get; }
}
