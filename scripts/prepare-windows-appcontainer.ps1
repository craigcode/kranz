[CmdletBinding()]
param(
    [Parameter()]
    [string[]] $Target = @("$env:SystemDrive\")
)

$ErrorActionPreference = 'Stop'
$metadataMask = [uint32] 0x00120088
$trustees = @(
    @{ Name = 'ALL APPLICATION PACKAGES'; Sid = 'S-1-15-2-1' },
    @{ Name = 'ALL RESTRICTED APPLICATION PACKAGES'; Sid = 'S-1-15-2-2' }
)

function Get-RuleSid {
    param([System.Security.AccessControl.FileSystemAccessRule] $Rule)

    return $Rule.IdentityReference.Translate(
        [System.Security.Principal.SecurityIdentifier]
    ).Value
}

foreach ($requestedRoot in ($Target | Sort-Object -Unique)) {
    if ([string]::IsNullOrWhiteSpace($requestedRoot)) {
        throw 'AppContainer host preparation received an empty target.'
    }
    $root = [System.IO.Path]::GetFullPath($requestedRoot)
    if ($root -notmatch '^[A-Za-z]:\\$') {
        throw "AppContainer host preparation target must be a local drive root (X:\): $requestedRoot"
    }

    $acl = Get-Acl -LiteralPath $root
    $changed = $false
    foreach ($trustee in $trustees) {
        $sid = [System.Security.Principal.SecurityIdentifier]::new($trustee.Sid)
        $explicitRules = @($acl.Access | Where-Object {
            -not $_.IsInherited -and (Get-RuleSid $_) -eq $trustee.Sid
        })
        $exactRules = @($explicitRules | Where-Object {
            $_.AccessControlType -eq [System.Security.AccessControl.AccessControlType]::Allow -and
            [uint32]([int32]$_.FileSystemRights) -eq $metadataMask -and
            $_.InheritanceFlags -eq [System.Security.AccessControl.InheritanceFlags]::None -and
            $_.PropagationFlags -eq [System.Security.AccessControl.PropagationFlags]::None
        })
        if ($exactRules.Count -gt 0) {
            continue
        }
        if ($explicitRules.Count -gt 0) {
            throw "$root already has a conflicting explicit ACE for $($trustee.Name) ($($trustee.Sid)); refusing to merge rights"
        }

        $rule = [System.Security.AccessControl.FileSystemAccessRule]::new(
            $sid,
            [System.Security.AccessControl.FileSystemRights] $metadataMask,
            [System.Security.AccessControl.InheritanceFlags]::None,
            [System.Security.AccessControl.PropagationFlags]::None,
            [System.Security.AccessControl.AccessControlType]::Allow
        )
        $null = $acl.AddAccessRule($rule)
        $changed = $true
    }

    if ($changed) {
        Set-Acl -LiteralPath $root -AclObject $acl
    }

    $verifiedAcl = Get-Acl -LiteralPath $root
    foreach ($trustee in $trustees) {
        $verified = @($verifiedAcl.Access | Where-Object {
            -not $_.IsInherited -and
            (Get-RuleSid $_) -eq $trustee.Sid -and
            $_.AccessControlType -eq [System.Security.AccessControl.AccessControlType]::Allow -and
            [uint32]([int32]$_.FileSystemRights) -eq $metadataMask -and
            $_.InheritanceFlags -eq [System.Security.AccessControl.InheritanceFlags]::None -and
            $_.PropagationFlags -eq [System.Security.AccessControl.PropagationFlags]::None
        })
        if ($verified.Count -eq 0) {
            throw "AppContainer host preparation did not produce the exact metadata ACE for $($trustee.Name) on $root"
        }
    }
    Write-Host "AppContainer host preparation verified: target=$root mask=0x$($metadataMask.ToString('x8')) inheritance=none"
}
