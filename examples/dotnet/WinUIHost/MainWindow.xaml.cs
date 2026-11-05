using Microsoft.UI.Dispatching;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using VstAudioExamples;

namespace WinUIHost;

/// <summary>Draws the session's state. Every VstAudio call runs on the thread pool inside the session.</summary>
public sealed partial class MainWindow : Window
{
    private readonly Session session;
    private readonly DispatcherQueueTimer heartbeat;
    private bool exiting;
    private bool closeAllowed;

    public MainWindow(Session session)
    {
        InitializeComponent();
        this.session = session;
        heartbeat = DispatcherQueue.CreateTimer();
        heartbeat.Interval = Session.HeartbeatInterval;
        heartbeat.Tick += (_, _) => session.Heartbeat();
        session.Changed += Refresh;
        Refresh(session.State);
        AppWindow.Closing += OnClosing;
    }

    private async void OnLoaded(object sender, RoutedEventArgs e)
    {
        heartbeat.Start();
        if (session.Options.Smoke)
            await ExitAsync(await session.RunSmokeAsync());
        else
            await session.LoadAsync();
    }

    private async void OnOpenEditor(object sender, RoutedEventArgs e) => await session.OpenEditorAsync();

    private async void OnCloseEditor(object sender, RoutedEventArgs e) => await session.CloseEditorAsync();

    private async void OnRender(object sender, RoutedEventArgs e) => await session.RenderAsync();

    private void Refresh(SessionState state)
    {
        PluginText.Text = state.Plugin;
        StatusText.Text = state.Status;
        DetailsText.Text = state.Details;
        OpenEditorButton.IsEnabled = state.Ready;
        CloseEditorButton.IsEnabled = state.Ready;
        RenderButton.IsEnabled = state.Ready;
    }

    // Closing by hand stops the plugin off the UI thread first, then exits with 0.
    private void OnClosing(AppWindow sender, AppWindowClosingEventArgs args)
    {
        if (closeAllowed)
            return;
        args.Cancel = true;
        _ = ExitAsync(0);
    }

    private async Task ExitAsync(int code)
    {
        if (exiting)
            return;
        exiting = true;
        heartbeat.Stop();
        await session.ShutdownAsync();
        closeAllowed = true;
        Close();
        Environment.Exit(code);
    }
}
