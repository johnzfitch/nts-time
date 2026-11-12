#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Installs nts-time: NTS-authenticated clock sync that replaces Windows' plain-NTP time sync.
.DESCRIPTION
    Copies nts-time.exe to %ProgramFiles%\nts-time, registers a SYSTEM scheduled task
    (\nts-time\Sync) that runs at boot and every -IntervalMinutes, and stops W32Time from
    syncing (Type=NoSync, Secure Time Seeding off, its sync tasks disabled). The original
    W32Time settings are saved once to %ProgramData%\nts-time\w32time-backup.json, and
    uninstall.ps1 restores them. Works in Windows PowerShell 5.1 and PowerShell 7.
.PARAMETER Servers
    Comma-separated host=operator list passed to nts-time --servers. Default: built-in list.
.PARAMETER IntervalMinutes
    How often to sync. Default 15.
.PARAMETER DryRun
    Only run nts-time once without --apply and print what it would do. Changes nothing.
#>
param(
    [string]$Servers = '',
    [ValidateRange(1, 1440)][int]$IntervalMinutes = 15,
    [switch]$DryRun
)
$ErrorActionPreference = 'Stop'

# The exe sits next to this script in a release zip, or under target\release in a source checkout.
$src = @(
    (Join-Path $PSScriptRoot 'nts-time.exe'),
    (Join-Path $PSScriptRoot 'target\release\nts-time.exe')
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $src) { throw "nts-time.exe not found next to install.ps1 or in target\release (run: cargo build --release)" }

$extra = @()
if ($Servers) { $extra = @('--servers', $Servers) }

if ($DryRun) {
    & $src @extra
    exit $LASTEXITCODE
}

$binDir    = Join-Path $env:ProgramFiles 'nts-time'
$dataDir   = Join-Path $env:ProgramData 'nts-time'
$exe       = Join-Path $binDir 'nts-time.exe'
$log       = Join-Path $dataDir 'runs.jsonl'
$backup    = Join-Path $dataDir 'w32time-backup.json'
$w32Param  = 'HKLM:\SYSTEM\CurrentControlSet\Services\W32Time\Parameters'
$w32Config = 'HKLM:\SYSTEM\CurrentControlSet\Services\W32Time\Config'
$w32Tasks  = '\Microsoft\Windows\Time Synchronization\'
$taskPath  = '\nts-time\'
$taskName  = 'Sync'

New-Item -ItemType Directory -Force $binDir, $dataDir | Out-Null
Start-Transcript -Append -Path (Join-Path $dataDir 'install.log') | Out-Null
try {
    # Check the servers are reachable and agree before changing anything.
    Write-Host 'checking NTS servers (no clock change)...'
    & $src @extra
    if ($LASTEXITCODE -ne 0) { throw "nts-time dry run failed with exit code $LASTEXITCODE; nothing was changed" }

    # Binary under Program Files (admin-only write); data dir writable only by SYSTEM/Administrators.
    $running = Get-ScheduledTask -TaskPath $taskPath -TaskName $taskName -ErrorAction SilentlyContinue
    if ($running -and $running.State -eq 'Running') { Stop-ScheduledTask -TaskPath $taskPath -TaskName $taskName }
    Copy-Item $src $exe -Force
    icacls $dataDir /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' '*S-1-5-32-545:(OI)(CI)RX' | Out-Null
    Write-Host "installed $exe  sha256 $((Get-FileHash $exe -Algorithm SHA256).Hash)"

    # Save the original W32Time settings once, so a reinstall never overwrites the real originals.
    if (-not (Test-Path $backup)) {
        $p = Get-ItemProperty $w32Param
        [ordered]@{
            saved              = (Get-Date).ToString('o')
            Type               = $p.Type
            NtpServer          = $p.NtpServer
            UtilizeSslTimeData = (Get-ItemProperty $w32Config).UtilizeSslTimeData
            StartType          = (Get-Service W32Time).StartType.ToString()
            Tasks              = @(Get-ScheduledTask -TaskPath $w32Tasks | ForEach-Object {
                                     @{ name = $_.TaskName; state = $_.State.ToString() } })
        } | ConvertTo-Json -Depth 4 | Set-Content -Encoding UTF8 $backup
        Write-Host "saved W32Time settings to $backup"
    } else {
        Write-Host "keeping existing backup $backup"
    }

    # Stop plain NTP: no sync source for W32Time, and its sync tasks disabled.
    Set-ItemProperty $w32Param -Name Type -Value 'NoSync'
    # Secure Time Seeding sets the clock from unauthenticated TLS handshake data; turn it off too.
    Set-ItemProperty $w32Config -Name UtilizeSslTimeData -Type DWord -Value 0
    Get-ScheduledTask -TaskPath $w32Tasks | Disable-ScheduledTask | Out-Null
    if ((Get-Service W32Time).Status -ne 'Stopped') { Stop-Service W32Time -Force }
    Write-Host 'W32Time: Type=NoSync, Secure Time Seeding off, Time Synchronization tasks disabled'

    # SYSTEM task: at boot (after 1 min) and every -IntervalMinutes.
    $argList = @('--apply', '--log', "`"$log`"") + ($extra | ForEach-Object { "`"$_`"" })
    $action   = New-ScheduledTaskAction -Execute $exe -Argument ($argList -join ' ')
    $boot     = New-ScheduledTaskTrigger -AtStartup
    $boot.Delay = 'PT1M'
    $repeat   = New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(1) `
                    -RepetitionInterval (New-TimeSpan -Minutes $IntervalMinutes)
    $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Minutes 30) `
                    -MultipleInstances IgnoreNew -StartWhenAvailable `
                    -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    $principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
    Register-ScheduledTask -TaskPath $taskPath -TaskName $taskName -Action $action `
        -Trigger $boot, $repeat -Settings $settings -Principal $principal `
        -Description 'Sets the clock from NTS-authenticated time servers (nts-time). Undo: uninstall.ps1' `
        -Force | Out-Null
    Write-Host "registered task $taskPath$taskName (every $IntervalMinutes min and at boot)"

    # First real run now, and show its result.
    Start-ScheduledTask -TaskPath $taskPath -TaskName $taskName
    $deadline = (Get-Date).AddMinutes(6)
    do { Start-Sleep 2 } while ((Get-ScheduledTask -TaskPath $taskPath -TaskName $taskName).State -eq 'Running' -and (Get-Date) -lt $deadline)
    $info = Get-ScheduledTaskInfo -TaskPath $taskPath -TaskName $taskName
    Write-Host "first run: LastTaskResult=$($info.LastTaskResult) at $($info.LastRunTime)"
    if (Test-Path $log) {
        $last = Get-Content $log -Tail 1 | ConvertFrom-Json
        Write-Host "action: $($last.action)"
        Write-Host "selection: $($last.selection.reason)"
    }
} finally {
    Stop-Transcript | Out-Null
}
