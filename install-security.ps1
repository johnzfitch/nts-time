# Shared installer path/ACL checks. Dot-sourcing this file does not change the machine.
# Use SIDs so this also works on non-English Windows installations.

function New-ProtectedDirectoryAcl {
    $acl = New-Object System.Security.AccessControl.DirectorySecurity
    $acl.SetSecurityDescriptorSddlForm('O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FRFX;;;BU)')
    return $acl
}

function Assert-NoReparsePoint {
    param([Parameter(Mandatory = $true)][string]$Path)
    $part = [IO.Path]::GetFullPath($Path)
    while ($part) {
        try {
            $item = Get-Item -LiteralPath $part -Force -ErrorAction Stop
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
                throw "Refusing reparse point in installation path: $part"
            }
        } catch [System.Management.Automation.ItemNotFoundException] { }
        $parent = [IO.Path]::GetDirectoryName($part)
        if ($parent -eq $part) { break }
        $part = $parent
    }
}

function Assert-TrustedInstallItem {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [switch]$Directory
    )
    Assert-NoReparsePoint -Path $Path
    if (-not (Test-Path -LiteralPath $Path)) { return }
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if ([bool]$item.PSIsContainer -ne [bool]$Directory) {
        throw "Unexpected file/directory at installation path: $Path"
    }
    $acl = Get-Acl -LiteralPath $Path -ErrorAction Stop
    $raw = New-Object Security.AccessControl.RawSecurityDescriptor($acl.GetSecurityDescriptorBinaryForm(), 0)
    if ($null -eq $raw.DiscretionaryAcl) { throw "Refusing installation item with a null DACL: $Path" }
    # Program Files normally inherits a full-control rule for Windows TrustedInstaller.
    # Accept that privileged service on existing items; the installed DACL is still replaced.
    $trusted = @('S-1-5-18', 'S-1-5-32-544',
                 'S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464')
    $owner = $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value
    if ($owner -notin $trusted) {
        throw "Refusing installation item owned by an untrusted principal: $Path ($owner)"
    }
    # Changing a DACL cannot revoke an already-open writable handle. Refuse insecure
    # existing objects instead of trusting a permission repair to make their content safe.
    $writeRights = [Security.AccessControl.FileSystemRights]::Write -bor
                   [Security.AccessControl.FileSystemRights]::Delete -bor
                   [Security.AccessControl.FileSystemRights]::DeleteSubdirectoriesAndFiles -bor
                   [Security.AccessControl.FileSystemRights]::ChangePermissions -bor
                   [Security.AccessControl.FileSystemRights]::TakeOwnership -bor
                   0x10000000 -bor 0x40000000 # GENERIC_ALL, GENERIC_WRITE
    foreach ($rule in $acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
        if ($rule.PropagationFlags -band [Security.AccessControl.PropagationFlags]::InheritOnly) { continue }
        if ($rule.AccessControlType -eq [Security.AccessControl.AccessControlType]::Allow -and
            ($rule.FileSystemRights -band $writeRights) -and
            $rule.IdentityReference.Value -notin $trusted) {
            throw "Refusing installation item writable by an untrusted principal: $Path"
        }
    }
}

function Initialize-ProtectedInstallDirectory {
    param([Parameter(Mandatory = $true)][string]$Path)
    Assert-TrustedInstallItem -Path $Path -Directory
    if (-not (Test-Path -LiteralPath $Path)) {
        # No -Force: if another process creates this path after the check, fail closed.
        New-Item -ItemType Directory -Path $Path -ErrorAction Stop | Out-Null
    }
    Set-Acl -LiteralPath $Path -AclObject (New-ProtectedDirectoryAcl) -ErrorAction Stop
    Assert-TrustedInstallItem -Path $Path -Directory
}
