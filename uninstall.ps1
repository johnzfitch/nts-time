#Requires -RunAsAdministrator
# Removes the nts-time task and binary and restores Windows' own time sync from the saved backup.
# Run logs in %ProgramData%\nts-time are kept.
$ErrorActionPreference = 'Stop'

$binDir   = Join-Path $env:ProgramFiles 'nts-time'
$dataDir  = Join-Path $env:ProgramData 'nts-time'
$backup   = Join-Path $dataDir 'w32time-backup.json'
$w32Param = 'HKLM:\SYSTEM\CurrentControlSet\Services\W32Time\Parameters'
$w32Config = 'HKLM:\SYSTEM\CurrentControlSet\Services\W32Time\Config'
$w32Tasks = '\Microsoft\Windows\Time Synchronization\'

Unregister-ScheduledTask -TaskPath '\nts-time\' -TaskName 'Sync' -Confirm:$false -ErrorAction SilentlyContinue
Remove-Item -Recurse -Force $binDir -ErrorAction SilentlyContinue

if (Test-Path $backup) {
    $b = Get-Content $backup -Raw | ConvertFrom-Json
    Set-ItemProperty $w32Param -Name Type -Value $b.Type
    Set-ItemProperty $w32Param -Name NtpServer -Value $b.NtpServer
    Set-Service W32Time -StartupType $b.StartType
    if ($null -ne $b.UtilizeSslTimeData) { Set-ItemProperty $w32Config -Name UtilizeSslTimeData -Type DWord -Value $b.UtilizeSslTimeData }
    foreach ($t in $b.Tasks) {
        if ($t.state -ne 'Disabled') { Enable-ScheduledTask -TaskPath $w32Tasks -TaskName $t.name | Out-Null }
    }
    Rename-Item $backup "w32time-backup.restored-$(Get-Date -Format yyyyMMdd-HHmmss).json"
    Write-Host "restored W32Time: Type=$($b.Type) NtpServer=$($b.NtpServer)"
} else {
    # Windows 11 defaults
    Set-ItemProperty $w32Param -Name Type -Value 'NTP'
    Set-ItemProperty $w32Param -Name NtpServer -Value 'time.windows.com,0x9'
    Set-ItemProperty $w32Config -Name UtilizeSslTimeData -Type DWord -Value 1
    Get-ScheduledTask -TaskPath $w32Tasks | Enable-ScheduledTask | Out-Null
    Write-Host 'no backup found; restored Windows defaults (time.windows.com, NTP)'
}

Start-Service W32Time
w32tm /config /update | Out-Null
w32tm /resync
