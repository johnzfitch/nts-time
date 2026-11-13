# No administrator rights, installation, service/task changes, or clock changes required.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = Split-Path $PSScriptRoot -Parent

foreach ($name in @('install.ps1', 'uninstall.ps1', 'install-security.ps1')) {
    $tokens = $null
    $errors = $null
    [void][Management.Automation.Language.Parser]::ParseFile((Join-Path $root $name), [ref]$tokens, [ref]$errors)
    if ($errors.Count) { throw "$name syntax errors: $errors" }
}
. (Join-Path $root 'install-security.ps1')

$script:checks = 0
function Assert-True($Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
    $script:checks++
}
function Assert-Throws([scriptblock]$Action, [string]$Pattern) {
    try { & $Action } catch {
        Assert-True ($_.Exception.Message -like $Pattern) "Unexpected error: $_"
        return
    }
    throw "Expected error matching $Pattern"
}

# Real Windows security descriptor construction; no filesystem writes.
$acl = New-ProtectedDirectoryAcl
Assert-True $acl.AreAccessRulesProtected 'DACL must not inherit writable parent rules'
Assert-True ($acl.GetOwner([Security.Principal.SecurityIdentifier]).Value -eq 'S-1-5-32-544') 'Owner must be Administrators'
$rules = @($acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))
Assert-True ($rules.Count -eq 3) 'Expected exactly SYSTEM, Administrators and Users rules'
foreach ($rule in $rules) {
    $sid = $rule.IdentityReference.Value
    if ($sid -eq 'S-1-5-32-545') {
        Assert-True (($rule.FileSystemRights -band [Security.AccessControl.FileSystemRights]::Write) -eq 0) 'Users may not write'
        Assert-True (($rule.FileSystemRights -band [Security.AccessControl.FileSystemRights]::ChangePermissions) -eq 0) 'Users may not change the DACL'
    } else {
        Assert-True ($sid -in @('S-1-5-18', 'S-1-5-32-544')) 'Unexpected principal'
    }
}

# Provider mocks exercise fail-closed decisions without touching Program Files or ProgramData.
$script:targetPath = 'C:\nts-time-installer-test\data'
$script:mockAcl = New-ProtectedDirectoryAcl
$script:exists = $true
$script:directory = $true
$script:reparsePath = ''
$script:setFails = $false
$script:setCalls = 0
function Test-Path { param($LiteralPath) return ($LiteralPath -ne $script:targetPath -or $script:exists) }
function Get-Item {
    [CmdletBinding()] param($LiteralPath, [switch]$Force)
    $attributes = [IO.FileAttributes]::Normal
    if ($LiteralPath -eq $script:reparsePath) { $attributes = [IO.FileAttributes]::ReparsePoint }
    return [pscustomobject]@{ Attributes = $attributes; PSIsContainer = $script:directory }
}
function Get-Acl { [CmdletBinding()] param($LiteralPath) return $script:mockAcl }
function Set-Acl {
    [CmdletBinding()] param($LiteralPath, $AclObject)
    if ($script:setFails) { throw 'simulated Set-Acl failure' }
    $script:mockAcl = $AclObject
    $script:setCalls++
}
function New-Item { [CmdletBinding()] param($Path, $ItemType) $script:exists = $true }

Initialize-ProtectedInstallDirectory -Path $script:targetPath
Assert-True ($script:setCalls -eq 1) 'Valid existing directory must be protected'
$script:exists = $false
Initialize-ProtectedInstallDirectory -Path $script:targetPath
Assert-True ($script:exists -and $script:setCalls -eq 2) 'New directory must be protected'

$script:mockAcl.SetOwner((New-Object Security.Principal.SecurityIdentifier('S-1-5-32-545')))
Assert-Throws { Initialize-ProtectedInstallDirectory -Path $script:targetPath } '*owned by an untrusted principal*'
Assert-True ($script:setCalls -eq 2) 'Must not repair an untrusted-owned directory'

$script:mockAcl = New-ProtectedDirectoryAcl
$everyone = New-Object Security.Principal.SecurityIdentifier('S-1-1-0')
$writeRule = New-Object Security.AccessControl.FileSystemAccessRule($everyone, 'Write', 'Allow')
$script:mockAcl.AddAccessRule($writeRule)
Assert-Throws { Initialize-ProtectedInstallDirectory -Path $script:targetPath } '*writable by an untrusted principal*'
Assert-True ($script:setCalls -eq 2) 'Must not silently trust previously writable content'

$script:mockAcl = New-ProtectedDirectoryAcl
$trustedInstaller = New-Object Security.Principal.SecurityIdentifier('S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464')
$script:mockAcl.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule($trustedInstaller, 'FullControl', 'Allow')))
Assert-TrustedInstallItem -Path $script:targetPath -Directory
$script:checks++
$script:mockAcl = New-ProtectedDirectoryAcl
$script:mockAcl.SetSecurityDescriptorSddlForm('O:BAG:BAD:NO_ACCESS_CONTROL')
Assert-Throws { Assert-TrustedInstallItem -Path $script:targetPath -Directory } '*null DACL*'
$script:mockAcl = New-ProtectedDirectoryAcl
$script:reparsePath = $script:targetPath
Assert-Throws { Initialize-ProtectedInstallDirectory -Path $script:targetPath } '*reparse point*'
$script:reparsePath = 'C:\nts-time-installer-test'
Assert-Throws { Initialize-ProtectedInstallDirectory -Path $script:targetPath } '*reparse point*'
$script:reparsePath = ''

$script:directory = $false
Assert-Throws { Initialize-ProtectedInstallDirectory -Path $script:targetPath } '*Unexpected file/directory*'
Assert-TrustedInstallItem -Path $script:targetPath
$script:mockAcl.AddAccessRule($writeRule)
Assert-Throws { Assert-TrustedInstallItem -Path $script:targetPath } '*writable by an untrusted principal*'
$script:mockAcl = New-ProtectedDirectoryAcl
$script:directory = $true
$script:setFails = $true
Assert-Throws { Initialize-ProtectedInstallDirectory -Path $script:targetPath } '*simulated Set-Acl failure*'

Write-Output "Installer security tests: $script:checks assertions passed (PowerShell $($PSVersionTable.PSVersion))"
