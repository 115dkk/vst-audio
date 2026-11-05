using System.Windows;
using VstAudioExamples;

namespace WpfHost;

public partial class App : Application
{
    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        Options options;
        try
        {
            options = Options.FromCommandLine();
        }
        catch (UsageException error)
        {
            Console.Error.WriteLine(error.Message);
            if (!error.Smoke)
                MessageBox.Show(error.Message, "VstAudio WPF example", MessageBoxButton.OK, MessageBoxImage.Error);
            Shutdown(2);
            return;
        }
        MainWindow = new MainWindow(new Session("wpf", options));
        MainWindow.Show();
    }
}
