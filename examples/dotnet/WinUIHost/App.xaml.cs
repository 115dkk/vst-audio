using System.Runtime.InteropServices;
using Microsoft.UI.Xaml;
using VstAudioExamples;

namespace WinUIHost;

public partial class App : Application
{
    private const uint MessageBoxError = 0x10; // MB_OK | MB_ICONERROR
    private MainWindow? window;

    public App()
    {
        InitializeComponent();
    }

    protected override void OnLaunched(Microsoft.UI.Xaml.LaunchActivatedEventArgs args)
    {
        // An unpackaged app's args.Arguments is empty, so the session reads the process command line.
        Options options;
        try
        {
            options = Options.FromCommandLine();
        }
        catch (UsageException error)
        {
            Console.Error.WriteLine(error.Message);
            if (!error.Smoke)
                MessageBoxW(0, error.Message, "VstAudio WinUI 3 example", MessageBoxError);
            Environment.Exit(2);
            return;
        }
        window = new MainWindow(new Session("winui", options));
        window.Activate();
    }

    // WinUI 3 has no window-less message box; ContentDialog needs a XamlRoot.
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int MessageBoxW(nint owner, string text, string caption, uint type);
}
