# Temporary discovery probe: how the pinned EqualizerAPO-XT Editor exposes its VST
# card and panel to UI Automation, what it writes on save, and how Benchmark runs.
# Prints facts only; never fails the job.
param(
    [Parameter(Mandatory)] [string] $Root,
    [Parameter(Mandatory)] [string] $App,
    [Parameter(Mandatory)] [string] $Vst3,
    [Parameter(Mandatory)] [string] $Vst2
)
$ErrorActionPreference = "Continue"
New-Item -ItemType Directory -Force -Path $Root | Out-Null
Write-Host "PROBE app files: $((Get-ChildItem $App -File | Select-Object -ExpandProperty Name) -join ', ')"

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms
Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class ProbeWin {
    public delegate bool EnumProc(IntPtr hwnd, IntPtr data);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr data);
    [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr parent, EnumProc f, IntPtr data);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassNameW(IntPtr hwnd, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr hwnd, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out RECT r);
    [DllImport("user32.dll")] public static extern IntPtr GetParent(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hwnd);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    public static string Describe(IntPtr h) {
        var c = new StringBuilder(256); GetClassNameW(h, c, 256);
        var t = new StringBuilder(256); GetWindowTextW(h, t, 256);
        RECT r; GetWindowRect(h, out r); uint pid; GetWindowThreadProcessId(h, out pid);
        return String.Format("0x{0:x} pid {1} class '{2}' title '{3}' visible {4} rect [{5},{6},{7},{8}]", h.ToInt64(), pid, c, t, IsWindowVisible(h), r.Left, r.Top, r.Right, r.Bottom);
    }
    public static List<IntPtr> TopLevel(uint pid) {
        var list = new List<IntPtr>();
        EnumWindows((h, d) => { uint p; GetWindowThreadProcessId(h, out p); if (p == pid) list.Add(h); return true; }, IntPtr.Zero);
        return list;
    }
    public static List<IntPtr> Children(IntPtr parent) {
        var list = new List<IntPtr>();
        EnumChildWindows(parent, (h, d) => { list.Add(h); return true; }, IntPtr.Zero);
        return list;
    }
}
"@

function Dump-Windows($process, $label) {
    Write-Host "PROBE windows ($label):"
    foreach ($h in [ProbeWin]::TopLevel([uint32]$process.Id)) {
        if (-not [ProbeWin]::IsWindowVisible($h)) { continue }
        Write-Host "  TOP $([ProbeWin]::Describe($h))"
        foreach ($c in [ProbeWin]::Children($h)) { Write-Host "    CHILD $([ProbeWin]::Describe($c))" }
    }
}
function Dump-Uia($hwnd, $limit) {
    $element = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    $all = $element.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
    Write-Host "PROBE uia under 0x$('{0:x}' -f $hwnd.ToInt64()): $($all.Count) elements"
    $i = 0
    foreach ($e in $all) {
        if ($i++ -ge $limit) { break }
        $c = $e.Current
        $r = $c.BoundingRectangle
        Write-Host ("  [{0}] {1} name='{2}' id='{3}' class='{4}' rect=[{5},{6},{7},{8}] enabled={9}" -f $i, $c.ControlType.ProgrammaticName, $c.Name, $c.AutomationId, $c.ClassName, [int]$r.Left, [int]$r.Top, [int]$r.Right, [int]$r.Bottom, $c.IsEnabled)
    }
}

foreach ($case in @(@{ Name = "vst3"; Path = $Vst3 }, @{ Name = "vst2"; Path = $Vst2 })) {
    $cfg = Join-Path $Root "cfg-$($case.Name)"
    New-Item -ItemType Directory -Force -Path $cfg | Out-Null
    $config = Join-Path $cfg "config.txt"
    # No byte-order mark: it would become part of the first command's name.
    [System.IO.File]::WriteAllText($config, "VSTPlugin: Library `"$($case.Path)`"`r`n", (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "PROBE ==== $($case.Name): $($case.Path)"
    $editor = Start-Process -FilePath (Join-Path $App "Editor.exe") -ArgumentList "`"$config`"" -WorkingDirectory $App -PassThru
    $deadline = (Get-Date).AddSeconds(40)
    while ((Get-Date) -lt $deadline) {
        $editor.Refresh()
        if ($editor.HasExited) { break }
        if ($editor.MainWindowHandle -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 250
    }
    if ($editor.HasExited) { Write-Host "PROBE editor exited with $($editor.ExitCode)"; continue }
    Start-Sleep -Seconds 6
    $editor.Refresh()
    Dump-Windows $editor "after start"
    $main = $editor.MainWindowHandle
    $mainElement = [System.Windows.Automation.AutomationElement]::FromHandle($main)
    # Dismiss start-up message boxes (an uninstalled Editor asks about the registry).
    foreach ($attempt in 1..3) {
        $boxes = $mainElement.FindAll([System.Windows.Automation.TreeScope]::Descendants,
            (New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ClassNameProperty, "QMessageBox")))
        if ($boxes.Count -eq 0) { break }
        foreach ($box in $boxes) {
            Write-Host "PROBE message box '$($box.Current.Name)'"
            foreach ($label in @("No", "OK", "Cancel")) {
                $answer = $box.FindFirst([System.Windows.Automation.TreeScope]::Descendants,
                    (New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, $label)))
                if ($answer) {
                    Write-Host "PROBE answering '$label'"
                    try { $answer.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke() } catch { Write-Host "PROBE answer failed: $_" }
                    break
                }
            }
        }
        Start-Sleep -Seconds 2
    }
    $nameCondition = { param($n) New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, $n) }
    $button = $mainElement.FindFirst([System.Windows.Automation.TreeScope]::Descendants, (& $nameCondition "Open panel"))
    if (-not $button) {
        $expand = $mainElement.FindFirst([System.Windows.Automation.TreeScope]::Descendants, (& $nameCondition ">"))
        if ($expand) {
            Write-Host "PROBE expanding the card"
            try { $expand.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke() } catch { Write-Host "PROBE expand failed: $_" }
            Start-Sleep -Seconds 3
        }
        Dump-Uia $main 400
        $button = $mainElement.FindFirst([System.Windows.Automation.TreeScope]::Descendants, (& $nameCondition "Open panel"))
    }
    if ($button) {
        $r = $button.Current.BoundingRectangle
        Write-Host "PROBE open panel at [$([int]$r.Left),$([int]$r.Top),$([int]$r.Right),$([int]$r.Bottom)] patterns $(($button.GetSupportedPatterns() | ForEach-Object { $_.ProgrammaticName }) -join ',')"
        try { $button.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke() } catch { Write-Host "PROBE invoke failed: $_" }
        Start-Sleep -Seconds 6
        $editor.Refresh()
        Dump-Windows $editor "after open panel"
        foreach ($h in [ProbeWin]::TopLevel([uint32]$editor.Id)) {
            if ($h -ne $main -and [ProbeWin]::IsWindowVisible($h)) { Dump-Uia $h 200 }
        }
        Dump-Uia $main 400
    } else {
        Write-Host "PROBE no 'Open panel' element"
    }
    [ProbeWin]::SetForegroundWindow($main) | Out-Null
    Start-Sleep -Milliseconds 500
    [System.Windows.Forms.SendKeys]::SendWait("^s")
    Start-Sleep -Seconds 3
    Write-Host "PROBE config after Ctrl+S:"
    Get-Content -LiteralPath $config | ForEach-Object { Write-Host "  | $($_.Substring(0, [Math]::Min(400, $_.Length)))" }
    Get-ChildItem $cfg | ForEach-Object { Write-Host "  file $($_.Name) $($_.Length)" }
    $editor.CloseMainWindow() | Out-Null
    Start-Sleep -Seconds 3
    $editor.Refresh()
    if (-not $editor.HasExited) {
        Dump-Windows $editor "after close request"
        Stop-Process -Id $editor.Id -Force
    }

    $out = Join-Path $Root "out-$($case.Name).wav"
    $bench = & (Join-Path $App "Benchmark.exe") --config-path $cfg -c 2 -r 48000 -l 2 -o $out --batchsize 512 --nopause -v 2>&1
    Write-Host "PROBE benchmark exit $LASTEXITCODE"
    $bench | Select-Object -Last 40 | ForEach-Object { Write-Host "  > $_" }
}
