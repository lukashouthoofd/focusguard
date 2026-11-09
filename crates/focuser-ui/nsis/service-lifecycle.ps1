param(
  [Parameter(Mandatory = $true)]
  [ValidateSet('PreflightInstall', 'PrepareInstall', 'Install', 'PreflightRemove', 'Remove')]
  [string] $Mode,

  [Parameter(Mandatory = $true)]
  [string] $InstallDir,

  [string] $DeleteData = '0'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$serviceName = 'FocusGuardService'
$serviceMetadataPath = 'SOFTWARE\PublikHQ\FocusGuard'
$dataDir = Join-Path $env:ProgramData 'PublikHQ\FocusGuard'
$database = Join-Path $dataDir 'focusguard.db'
$installStatePath = Join-Path $dataDir 'install-state.json'
$databaseRollbackDir = Join-Path $dataDir 'installer-rollback'
$hostsFile = Join-Path $env:SystemRoot 'System32\drivers\etc\hosts'
$programFilesRoot = if ($env:ProgramW6432) { $env:ProgramW6432 } else { $env:ProgramFiles }
$serviceInstallDir = Join-Path $programFilesRoot 'PublikHQ\FocusGuard\Service'
$serviceExe = Join-Path $serviceInstallDir 'focusguard-service.exe'
$nativeHostExe = Join-Path $serviceInstallDir 'focuser-native.exe'
$packagedServiceExe = Join-Path $InstallDir 'focusguard-service.exe'
$packagedNativeHostExe = Join-Path $InstallDir 'focuser-native.exe'
$uiExe = Join-Path $InstallDir 'focusguard-ui.exe'
$diagnostics = Join-Path $dataDir 'service-diagnostics.log'
$identityPath = Join-Path $PSScriptRoot 'extension-identity.json'
$nativeManifestDir = Join-Path $serviceInstallDir 'native-messaging'
$chromeManifestPath = Join-Path $nativeManifestDir 'com.focuser.native.json'
$firefoxManifestPath = Join-Path $nativeManifestDir 'com.focuser.native.firefox.json'
$nativeRegistrySpecs = @(
  [pscustomobject]@{ Key = 'Software\Google\Chrome\NativeMessagingHosts\com.focuser.native'; Manifest = $chromeManifestPath },
  [pscustomobject]@{ Key = 'Software\Microsoft\Edge\NativeMessagingHosts\com.focuser.native'; Manifest = $chromeManifestPath },
  [pscustomobject]@{ Key = 'Software\Mozilla\NativeMessagingHosts\com.focuser.native'; Manifest = $firefoxManifestPath }
)

function Write-LifecycleTrace([string] $Message) {
  $tracePath = $env:FOCUSGUARD_LIFECYCLE_LOG
  if (-not $tracePath) { return }
  $line = "{0:o} [{1}] {2}{3}" -f [DateTime]::UtcNow, $Mode, $Message, [Environment]::NewLine
  [IO.File]::AppendAllText($tracePath, $line, [Text.UTF8Encoding]::new($false))
}

function Get-PathAclSummary([string] $Path) {
  if (-not (Test-Path -LiteralPath $Path)) { return 'Path does not exist' }
  $icacls = Join-Path $env:SystemRoot 'System32\icacls.exe'
  $output = & $icacls $Path 2>&1
  if ($LASTEXITCODE -ne 0) { return "icacls exited ${LASTEXITCODE}: $($output -join ' | ')" }
  return ($output -join ' | ')
}

function Invoke-Checked([string] $FilePath, [string[]] $Arguments, [int[]] $AllowedExitCodes = @(0)) {
  Write-LifecycleTrace "Running $([IO.Path]::GetFileName($FilePath)) $($Arguments -join ' ')"
  $output = & $FilePath @Arguments 2>&1
  $exitCode = $LASTEXITCODE
  if ($exitCode -notin $AllowedExitCodes) {
    throw "$([IO.Path]::GetFileName($FilePath)) exited ${exitCode}: $($output -join ' ')"
  }
  if ([IO.Path]::GetFileName($FilePath) -ieq 'icacls.exe' -and $output) {
    Write-LifecycleTrace "icacls output: $($output -join ' | ')"
  }
  Write-LifecycleTrace "Completed $([IO.Path]::GetFileName($FilePath)) with exit code $exitCode"
}

function Get-RegisteredService {
  Get-CimInstance Win32_Service -Filter "Name='$serviceName'" -ErrorAction Stop
}

function Get-ServiceMetadata {
  $views = if ([Environment]::Is64BitOperatingSystem) {
    @([Microsoft.Win32.RegistryView]::Registry64, [Microsoft.Win32.RegistryView]::Registry32)
  } else {
    @([Microsoft.Win32.RegistryView]::Default)
  }
  foreach ($view in $views) {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, $view)
    try {
      $key = $base.OpenSubKey($serviceMetadataPath, $false)
      if (-not $key) { continue }
      try {
        return [pscustomobject]@{
          OwnerSid = $key.GetValue('OwnerSid')
          InstallDir = $key.GetValue('InstallDir')
        }
      } finally { $key.Dispose() }
    } finally { $base.Dispose() }
  }
  return $null
}

function Set-ServiceMetadata([string] $OwnerSid, [string] $InstallDir) {
  $view = if ([Environment]::Is64BitOperatingSystem) {
    [Microsoft.Win32.RegistryView]::Registry64
  } else {
    [Microsoft.Win32.RegistryView]::Default
  }
  $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, $view)
  try {
    $key = $base.CreateSubKey($serviceMetadataPath)
    if (-not $key) { throw "Could not create HKLM registry key $serviceMetadataPath in view $view." }
    try {
      $key.SetValue('OwnerSid', $OwnerSid, [Microsoft.Win32.RegistryValueKind]::String)
      $key.SetValue('InstallDir', $InstallDir, [Microsoft.Win32.RegistryValueKind]::String)
    } finally { $key.Dispose() }
  } finally { $base.Dispose() }
}

function Remove-ServiceMetadata {
  $views = if ([Environment]::Is64BitOperatingSystem) {
    @([Microsoft.Win32.RegistryView]::Registry64, [Microsoft.Win32.RegistryView]::Registry32)
  } else {
    @([Microsoft.Win32.RegistryView]::Default)
  }
  foreach ($view in $views) {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, $view)
    try { $base.DeleteSubKeyTree($serviceMetadataPath, $false) }
    finally { $base.Dispose() }
  }
}

function Assert-OwnedServicePath {
  $registered = Get-RegisteredService
  if (-not $registered) { return }

  $match = [regex]::Match($registered.PathName, '^\s*"([^"]+focusguard-service\.exe)"(?:\s|$)', 'IgnoreCase')
  if (-not $match.Success) {
    throw "A service named $serviceName exists with an unrecognized executable path; refusing to modify it."
  }
  $registeredExe = [IO.Path]::GetFullPath($match.Groups[1].Value)
  if (-not [string]::Equals($registeredExe, $serviceExe, [StringComparison]::OrdinalIgnoreCase)) {
    throw "A service named $serviceName points to '$registeredExe', outside this installation; refusing to modify it."
  }
}

function Get-ExistingOwnerSid {
  $metadata = Get-ServiceMetadata
  if ($metadata -and $metadata.OwnerSid) { return [Security.Principal.SecurityIdentifier]::new($metadata.OwnerSid).Value }

  $registered = Get-RegisteredService
  if ($registered) {
    $match = [regex]::Match($registered.PathName, '--owner-sid\s+(S-\d+(?:-\d+)+)')
    if (-not $match.Success) { throw 'The existing FocusGuard service has no recognizable owner SID.' }
    return [Security.Principal.SecurityIdentifier]::new($match.Groups[1].Value).Value
  }
  return $null
}

function Get-InstallOwnerSid {
  $existing = Get-ExistingOwnerSid
  if ($existing) { return $existing }

  # Hosted CI has no interactive Explorer process. On an actual install, use
  # the shell in the installer's session so UAC alternate credentials still
  # bind policy to the person using the desktop.
  if ($env:GITHUB_ACTIONS -eq 'true' -and $env:RUNNER_OS -eq 'Windows') {
    return [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
  }

  $sessionId = (Get-Process -Id $PID).SessionId
  $owners = @(
    Get-CimInstance Win32_Process -Filter "Name='explorer.exe'" |
      Where-Object SessionId -eq $sessionId |
      ForEach-Object {
        $owner = Invoke-CimMethod -InputObject $_ -MethodName GetOwnerSid
        if ($owner.ReturnValue -eq 0 -and $owner.Sid) { $owner.Sid }
      } |
      Sort-Object -Unique
  )
  if ($owners.Count -ne 1) {
    throw 'Could not identify exactly one desktop user in the installer session; sign in to the account you want FocusGuard to protect and retry.'
  }
  return [Security.Principal.SecurityIdentifier]::new($owners[0]).Value
}

function Get-UiProcesses {
  $processes = @(Get-CimInstance Win32_Process -Filter "Name='focusguard-ui.exe'" -ErrorAction Stop)
  foreach ($process in $processes) {
    $owner = Invoke-CimMethod -InputObject $process -MethodName GetOwnerSid -ErrorAction Stop
    if ($owner.ReturnValue -ne 0 -or -not $owner.Sid) {
      throw "Could not verify the Windows account that owns focusguard-ui.exe process $($process.ProcessId)."
    }
    [pscustomobject]@{ ProcessId = $process.ProcessId; OwnerSid = $owner.Sid }
  }
}

function Assert-NoNativeHostProcess {
  $processes = @(Get-CimInstance Win32_Process -Filter "Name='focuser-native.exe'" -ErrorAction Stop)
  if ($processes.Count -gt 0) {
    $processIds = ($processes.ProcessId -join ', ')
    throw "FocusGuard's browser messaging host is active (process $processIds). Close browsers using the FocusGuard extension, then retry."
  }
}

function Get-ExtensionIdentity {
  if (-not (Test-Path -LiteralPath $identityPath)) { throw "Missing packaged extension identity file: $identityPath" }
  $identity = Get-Content -LiteralPath $identityPath -Raw | ConvertFrom-Json -ErrorAction Stop
  if ($identity.chrome_extension_id -notmatch '^[a-p]{32}$' -or
      -not $identity.chrome_public_key -or
      $identity.firefox_extension_id -ne 'focusguard@publikhq.com') {
    throw 'The packaged browser extension identity is invalid.'
  }
  $publicKey = [Convert]::FromBase64String($identity.chrome_public_key)
  $hash = [Security.Cryptography.SHA256]::Create().ComputeHash($publicKey)
  $derivedId = ''
  foreach ($byte in $hash[0..15]) {
    $derivedId += [char](97 + (($byte -shr 4) -band 15))
    $derivedId += [char](97 + ($byte -band 15))
  }
  if ($derivedId -ne $identity.chrome_extension_id) { throw 'The Chrome extension ID does not match its pinned public key.' }
  return $identity
}

function Get-NativeMessagingViews {
  if ([Environment]::Is64BitOperatingSystem) {
    return @([Microsoft.Win32.RegistryView]::Registry64, [Microsoft.Win32.RegistryView]::Registry32)
  }
  return @([Microsoft.Win32.RegistryView]::Default)
}

function Invoke-WithOwnerRegistryHive([string] $OwnerSid, [scriptblock] $Action) {
  $hiveName = $OwnerSid
  $loadedRoot = "Registry::HKEY_USERS\$hiveName"
  $temporaryHiveLoaded = $false
  if (-not (Test-Path -LiteralPath $loadedRoot)) {
    $profile = Get-OwnerProfile $OwnerSid
    $hiveFile = Join-Path $profile 'NTUSER.DAT'
    if (-not (Test-Path -LiteralPath $hiveFile)) { return }
    $hiveName = 'FocusGuardOwner_' + [guid]::NewGuid().ToString('N')
    Invoke-Checked "$env:SystemRoot\System32\reg.exe" @('load', "HKU\$hiveName", $hiveFile)
    $temporaryHiveLoaded = $true
  }

  try {
    foreach ($view in (Get-NativeMessagingViews)) {
      $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::Users, $view)
      try { & $Action $base $hiveName | Out-Null }
      finally { $base.Dispose() }
    }
  } finally {
    if ($temporaryHiveLoaded) {
      Invoke-Checked "$env:SystemRoot\System32\reg.exe" @('unload', "HKU\$hiveName")
    }
  }
}

function Get-LegacyNativeManifestPaths([string] $OwnerSid) {
  $profile = Get-OwnerProfile $OwnerSid
  @(
    (Join-Path $profile 'AppData\Roaming\focuser\FocusGuard\data\native-messaging\com.focuser.native.json'),
    (Join-Path $profile 'AppData\Roaming\focuser\FocusGuard\data\native-messaging\com.focuser.native.firefox.json'),
    (Join-Path $profile 'AppData\Roaming\publikhq\FocusGuard\data\native-messaging\com.focuser.native.json'),
    (Join-Path $profile 'AppData\Roaming\publikhq\FocusGuard\data\native-messaging\com.focuser.native.firefox.json')
  )
}

function Test-OwnedNativeManifest([string] $ManifestPath, [string] $ExpectedPath, [string] $OwnerSid) {
  if (-not $ManifestPath) { return $false }
  $normalized = [IO.Path]::GetFullPath($ManifestPath)
  if ([string]::Equals($normalized, [IO.Path]::GetFullPath($ExpectedPath), [StringComparison]::OrdinalIgnoreCase)) {
    return $true
  }
  foreach ($legacyPath in (Get-LegacyNativeManifestPaths $OwnerSid)) {
    if ([string]::Equals($normalized, [IO.Path]::GetFullPath($legacyPath), [StringComparison]::OrdinalIgnoreCase)) {
      return $true
    }
  }
  return $false
}

function Assert-NativeMessagingRegistryAvailable([string] $OwnerSid) {
  $identity = Get-ExtensionIdentity
  foreach ($view in (Get-NativeMessagingViews)) {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, $view)
    try {
      foreach ($spec in $nativeRegistrySpecs) {
        $key = $base.OpenSubKey($spec.Key, $false)
        if (-not $key) { continue }
        try {
          $current = $key.GetValue('')
          if ($current -and -not [string]::Equals([string]$current, [string]$spec.Manifest, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Native host registry key $($spec.Key) already points to '$current'; refusing to replace it."
          }
        } finally { $key.Dispose() }
      }
    } finally { $base.Dispose() }
  }

  if ($OwnerSid) {
    Invoke-WithOwnerRegistryHive $OwnerSid {
      param($base, $hiveName)
      foreach ($spec in $nativeRegistrySpecs) {
        $key = $base.OpenSubKey("$hiveName\$($spec.Key)", $false)
        if (-not $key) { continue }
        try { $current = [string]$key.GetValue('') }
        finally { $key.Dispose() }
        if (-not $current) { continue }
        $expected = if ($spec.Key -like '*Mozilla*') { $firefoxManifestPath } else { $chromeManifestPath }
        if (-not (Test-OwnedNativeManifest $current $expected $OwnerSid)) {
          throw "Per-user native host registry key $($spec.Key) already points to '$current'; refusing to replace it."
        }
      }
    } | Out-Null
  }
}

function Register-NativeMessagingHosts {
  $identity = Get-ExtensionIdentity
  New-Item -ItemType Directory -Force -Path $nativeManifestDir | Out-Null
  $chrome = @{
    name = 'com.focuser.native'
    description = 'FocusGuard Native Messaging Host'
    path = $nativeHostExe
    type = 'stdio'
    allowed_origins = @("chrome-extension://$($identity.chrome_extension_id)/")
  } | ConvertTo-Json -Depth 4
  $firefox = @{
    name = 'com.focuser.native'
    description = 'FocusGuard Native Messaging Host'
    path = $nativeHostExe
    type = 'stdio'
    allowed_extensions = @($identity.firefox_extension_id)
  } | ConvertTo-Json -Depth 4
  $utf8 = [Text.UTF8Encoding]::new($false)
  [IO.File]::WriteAllText($chromeManifestPath, $chrome, $utf8)
  [IO.File]::WriteAllText($firefoxManifestPath, $firefox, $utf8)

  foreach ($view in (Get-NativeMessagingViews)) {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, $view)
    try {
      foreach ($spec in $nativeRegistrySpecs) {
        $key = $base.OpenSubKey($spec.Key, $true)
        if (-not $key) { $key = $base.CreateSubKey($spec.Key) }
        if (-not $key) { throw "Could not create HKLM registry key $($spec.Key) in view $view." }
        try {
          $current = $key.GetValue('')
          if ($current -and -not [string]::Equals([string]$current, [string]$spec.Manifest, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Native host registry key $($spec.Key) already points to '$current'; refusing to replace it."
          }
          $key.SetValue('', $spec.Manifest, [Microsoft.Win32.RegistryValueKind]::String)
        } finally { $key.Dispose() }
      }
    } finally { $base.Dispose() }
  }
}

function Remove-NativeMessagingRegistrations {
  foreach ($view in (Get-NativeMessagingViews)) {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, $view)
    try {
      foreach ($spec in $nativeRegistrySpecs) {
        $key = $base.OpenSubKey($spec.Key, $true)
        if (-not $key) { continue }
        try {
          $current = $key.GetValue('')
          if ($current -and [string]::Equals([string]$current, [string]$spec.Manifest, [StringComparison]::OrdinalIgnoreCase)) {
            $key.DeleteValue('', $false)
            $parentPath = Split-Path -Parent $spec.Key
            $keyName = Split-Path -Leaf $spec.Key
            if ($key.SubKeyCount -eq 0 -and $key.ValueCount -eq 0) {
              $key.Dispose()
              $key = $null
              $parent = $base.OpenSubKey($parentPath, $true)
              if ($parent) { try { $parent.DeleteSubKey($keyName, $false) } finally { $parent.Dispose() } }
            }
          }
        } finally { if ($key) { $key.Dispose() } }
      }
    } finally { $base.Dispose() }
  }
}

function Remove-OwnerNativeMessagingRegistrations([string] $OwnerSid) {
  if (-not $OwnerSid) { return }
  Invoke-WithOwnerRegistryHive $OwnerSid {
    param($base, $hiveName)
    foreach ($spec in $nativeRegistrySpecs) {
      $keyPath = "$hiveName\$($spec.Key)"
      $key = $base.OpenSubKey($keyPath, $true)
      if (-not $key) { continue }
      $isEmpty = $false
      try {
        $current = [string]$key.GetValue('')
        if (-not $current) { continue }
        $expected = if ($spec.Key -like '*Mozilla*') { $firefoxManifestPath } else { $chromeManifestPath }
        if (-not (Test-OwnedNativeManifest $current $expected $OwnerSid)) { continue }
        $key.DeleteValue('', $false)
        $isEmpty = $key.SubKeyCount -eq 0 -and $key.ValueCount -eq 0
      } finally { $key.Dispose() }
      if ($isEmpty) {
        $separator = $keyPath.LastIndexOf('\')
        $parent = $base.OpenSubKey($keyPath.Substring(0, $separator), $true)
        if ($parent) {
          try { $parent.DeleteSubKey($keyPath.Substring($separator + 1), $false) }
          finally { $parent.Dispose() }
        }
      }
    }
  } | Out-Null

  foreach ($manifestPath in (Get-LegacyNativeManifestPaths $OwnerSid)) {
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) { continue }
    try {
      $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json -ErrorAction Stop
      if ($manifest.name -eq 'com.focuser.native' -and
          [IO.Path]::GetFileName([string]$manifest.path) -ieq 'focuser-native.exe') {
        Remove-Item -LiteralPath $manifestPath -Force -ErrorAction Stop
        $parent = Split-Path -Parent $manifestPath
        if (@(Get-ChildItem -LiteralPath $parent -Force -ErrorAction SilentlyContinue).Count -eq 0) {
          Remove-Item -LiteralPath $parent -Force -ErrorAction SilentlyContinue
        }
      }
    } catch {
      throw "Could not remove FocusGuard's legacy per-user Native Messaging manifest '$manifestPath': $_"
    }
  }
}

function Get-InstallState {
  if (-not (Test-Path -LiteralPath $installStatePath -PathType Leaf)) { return $null }
  try {
    $state = Get-Content -LiteralPath $installStatePath -Raw | ConvertFrom-Json -ErrorAction Stop
    if ($state.schema -ne 1 -or -not $state.owner_sid) { throw 'The install state is incomplete.' }
    $state.owner_sid = [Security.Principal.SecurityIdentifier]::new([string]$state.owner_sid).Value
    return $state
  } catch {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    Write-LifecycleTrace "Install state read failed as $($identity.Name) ($($identity.User.Value)); ACL: $(Get-PathAclSummary $installStatePath)"
    throw "The FocusGuard install state is unreadable; preserve '$installStatePath' and repair the installation before retrying: $_"
  }
}

function Write-InstallState([string] $OwnerSid) {
  $state = @{
    schema = 1
    owner_sid = $OwnerSid
    committed_utc = [DateTime]::UtcNow.ToString('o')
  } | ConvertTo-Json -Depth 3
  $temporaryPath = "$installStatePath.tmp"
  [IO.File]::WriteAllText($temporaryPath, $state, [Text.UTF8Encoding]::new($false))
  Move-Item -LiteralPath $temporaryPath -Destination $installStatePath -Force
}

function Assert-NoOtherOwnerUi([string] $OwnerSid) {
  $otherOwner = @(Get-UiProcesses | Where-Object OwnerSid -ne $OwnerSid)
  if ($otherOwner.Count -gt 0) {
    $processIds = ($otherOwner.ProcessId -join ', ')
    throw "FocusGuard is running in another Windows account (process $processIds). Close that session before continuing with this single-owner installation."
  }
}

function Get-PreviousOwnerInstallDirs([string] $OwnerSid) {
  $dirs = [Collections.Generic.List[string]]::new()
  if (-not $OwnerSid) { return @() }
  Invoke-WithOwnerRegistryHive $OwnerSid {
    param($base, $hiveName)
    $key = $base.OpenSubKey("$hiveName\Software\PublikHQ\FocusGuard", $false)
    if ($key) {
      try {
        $path = [string]$key.GetValue('')
        if ($path) { $dirs.Add($path) }
      } finally { $key.Dispose() }
    }
  } | Out-Null
  return $dirs.ToArray()
}

function Test-OwnedUiExecutable([string] $Path, [string] $OwnerSid, [string[]] $PreviousInstallDirs = @()) {
  if (-not $Path -or -not $OwnerSid) { return $false }
  try { $exePath = [IO.Path]::GetFullPath($Path) } catch { return $false }
  if ([IO.Path]::GetFileName($exePath) -ine 'focusguard-ui.exe') { return $false }
  $profile = Get-OwnerProfile $OwnerSid
  $knownInstallDirs = @(
    $InstallDir,
    (Join-Path $profile 'AppData\Local\FocusGuard'),
    (Join-Path $profile 'AppData\Local\PublikHQ\FocusGuard'),
    (Join-Path $programFilesRoot 'FocusGuard'),
    (Join-Path $programFilesRoot 'PublikHQ\FocusGuard')
  ) + @($PreviousInstallDirs)
  foreach ($dir in $knownInstallDirs) {
    if (-not $dir) { continue }
    if ([string]::Equals([IO.Path]::GetDirectoryName($exePath), [IO.Path]::GetFullPath($dir), [StringComparison]::OrdinalIgnoreCase)) {
      return $true
    }
  }
  return $false
}

function Remove-LegacyAutostart([string] $OwnerSid, [bool] $MigrateToUnelevatedRunEntry = $false) {
  $preserveAutostart = $false
  Write-LifecycleTrace 'Inspecting legacy autostart entries'
  $previousInstallDirs = @(Get-PreviousOwnerInstallDirs $OwnerSid)
  $task = Get-ScheduledTask -TaskPath '\' -TaskName 'FocusGuard' -ErrorAction SilentlyContinue
  if ($task) {
    $actions = @($task.Actions | Where-Object {
      (Test-OwnedUiExecutable $_.Execute $OwnerSid $previousInstallDirs) -and
      $_.Arguments -match '(^|\s)--autostart($|\s)'
    })
    if ($actions.Count -eq 0) {
      throw 'A scheduled task named FocusGuard belongs to another executable; refusing to alter it.'
    }
    $preserveAutostart = $task.State.ToString() -ne 'Disabled'
    Unregister-ScheduledTask -TaskPath '\' -TaskName 'FocusGuard' -Confirm:$false -ErrorAction Stop
  }

  if ($OwnerSid) {
    $runSeen = [bool[]]::new(1)
    Invoke-WithOwnerRegistryHive $OwnerSid {
      param($base, $hiveName)
      $runKey = $base.OpenSubKey("$hiveName\Software\Microsoft\Windows\CurrentVersion\Run", $true)
      if (-not $runKey) { return }
      try {
        $value = [string]$runKey.GetValue('FocusGuard')
        if ($value -and $value -match '^\s*"([^"]+)"' -and (Test-OwnedUiExecutable $Matches[1] $OwnerSid $previousInstallDirs)) {
          $runSeen[0] = $true
          $runKey.DeleteValue('FocusGuard', $false)
        }
      } finally { $runKey.Dispose() }
    } | Out-Null
    if ($runSeen[0]) { $preserveAutostart = $true }
  }

  if ($preserveAutostart -and $OwnerSid -and $MigrateToUnelevatedRunEntry) {
    Write-LifecycleTrace 'Migrating legacy autostart to the unelevated per-user Run key'
    Invoke-WithOwnerRegistryHive $OwnerSid {
      param($base, $hiveName)
      $runKey = $base.CreateSubKey("$hiveName\Software\Microsoft\Windows\CurrentVersion\Run")
      if (-not $runKey) { throw "Could not create FocusGuard's unelevated logon entry." }
      try {
        $runKey.SetValue('FocusGuard', "`"$uiExe`" --autostart", [Microsoft.Win32.RegistryValueKind]::String)
      } finally { $runKey.Dispose() }
    } | Out-Null
  }
  Write-LifecycleTrace 'Completed legacy autostart migration'
}

function Get-OwnerProfile([string] $OwnerSid) {
  $profileKey = "HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList\$OwnerSid"
  $profile = (Get-ItemProperty -LiteralPath $profileKey -Name ProfileImagePath -ErrorAction Stop).ProfileImagePath
  if (-not $profile -or -not (Test-Path -LiteralPath $profile)) {
    throw "Could not find the Windows profile for policy owner $OwnerSid."
  }
  return [Environment]::ExpandEnvironmentVariables($profile)
}

function Wait-ServiceState([string] $Expected, [int] $TimeoutSeconds = 30) {
  $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
  do {
    $service = Get-Service -Name $serviceName -ErrorAction SilentlyContinue
    if ($null -eq $service) {
      if ($Expected -eq 'Absent') { return }
    } elseif ($service.Status.ToString() -eq $Expected) {
      return
    }
    Start-Sleep -Milliseconds 250
  } while ([DateTime]::UtcNow -lt $deadline)
  $actual = (Get-Service -Name $serviceName -ErrorAction SilentlyContinue).Status
  throw "FocusGuard service did not reach '$Expected' (current state: $actual)."
}

function Stop-ServiceForInstall([string] $OwnerSid) {
  Assert-OwnedServicePath
  Assert-NoOtherOwnerUi $OwnerSid
  $runningUi = @(Get-UiProcesses)
  if ($runningUi) {
    throw 'FocusGuard is still running. Close its window and retry so its database can be handed to the service safely.'
  }

  $registered = Get-RegisteredService
  if ($registered -and $registered.State -ne 'Stopped') {
    Stop-Service -Name $serviceName -Force
    Wait-ServiceState 'Stopped'
  }
}

function Set-ProgramDataAcl {
  $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
  $principal = [Security.Principal.WindowsPrincipal]::new($identity)
  $isAdministrator = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
  Write-LifecycleTrace "Setting ProgramData ACL as $($identity.Name) ($($identity.User.Value)); elevated administrator token=$isAdministrator"
  New-Item -ItemType Directory -Force -Path $dataDir | Out-Null
  Invoke-Checked "$env:SystemRoot\System32\icacls.exe" @(
    $dataDir, '/inheritance:r', '/grant:r', '*S-1-5-18:(OI)(CI)F',
    '*S-1-5-32-544:(OI)(CI)F', '/T', '/C'
  )
  $stateAcl = if (Test-Path -LiteralPath $installStatePath -PathType Leaf) {
    Get-PathAclSummary $installStatePath
  } else {
    'not created yet'
  }
  Write-LifecycleTrace "ProgramData ACL after update: directory=$(Get-PathAclSummary $dataDir); state=$stateAcl"
}

function Copy-InitialDatabase([string] $OwnerSid) {
  $state = Get-InstallState
  if (Test-Path -LiteralPath $database -PathType Leaf) {
    if (-not $state -and -not (Get-RegisteredService)) {
      throw "An uncommitted FocusGuard database already exists at '$database'; refusing to guess whether it is a complete policy copy. Preserve it and review the install diagnostics before retrying."
    }
    if ($state -and $state.owner_sid -ne $OwnerSid) {
      throw "The existing FocusGuard database belongs to policy owner $($state.owner_sid), not $OwnerSid."
    }
    return $false
  }
  if ($state) {
    throw "The committed FocusGuard install state exists but its policy database is missing at '$database'."
  }

  $profile = Get-OwnerProfile $OwnerSid
  $sources = @(
    (Join-Path $profile 'AppData\Roaming\publikhq\FocusGuard\data\focusguard.db'),
    (Join-Path $profile 'AppData\Roaming\focuser\FocusGuard\data\focusguard.db')
  )
  $source = $sources | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
  if (-not $source) { return $false }

  $stagedFiles = @()
  $publishedFiles = @()
  try {
    # The UI and native host have already been stopped. Copy the database and
    # SQLite recovery sidecars to temporary same-volume names, verify each
    # byte-for-byte, then publish them together before starting the service.
    # SQLite rebuilds -shm, so never copy its process-shared lock/index file.
    foreach ($suffix in @('', '-journal', '-wal')) {
      $sourceFile = "$source$suffix"
      if (-not (Test-Path -LiteralPath $sourceFile -PathType Leaf)) { continue }
      $destination = "$database$suffix"
      if (Test-Path -LiteralPath $destination) {
        throw "A partial FocusGuard policy database sidecar already exists at '$destination'; preserve it and review the installation before retrying."
      }
      $stage = "$database.installing$suffix"
      $stagedFiles += [pscustomobject]@{ Stage = $stage; Destination = $destination }
      Copy-Item -LiteralPath $sourceFile -Destination $stage -Force
      $sourceHash = (Get-FileHash -LiteralPath $sourceFile -Algorithm SHA256).Hash
      $stageHash = (Get-FileHash -LiteralPath $stage -Algorithm SHA256).Hash
      if ($sourceHash -ne $stageHash) { throw "The staged policy database file '$sourceFile' did not pass its SHA-256 copy check." }
    }
    if (-not ($stagedFiles | Where-Object { $_.Destination -eq $database })) {
      throw "Could not stage the source policy database '$source'."
    }
    foreach ($file in $stagedFiles) {
      Move-Item -LiteralPath $file.Stage -Destination $file.Destination
      $publishedFiles += $file.Destination
    }
  } catch {
    foreach ($file in $stagedFiles) { Remove-Item -LiteralPath $file.Stage -Force -ErrorAction SilentlyContinue }
    foreach ($file in $publishedFiles) { Remove-Item -LiteralPath $file -Force -ErrorAction SilentlyContinue }
    throw
  }
  Write-Verbose "Staged the existing FocusGuard policy database for $OwnerSid; the source remains in the user's profile."
  return $true
}

function Backup-ExistingDatabase {
  if (-not (Test-Path -LiteralPath $database)) { return $false }
  Remove-Item -LiteralPath $databaseRollbackDir -Recurse -Force -ErrorAction SilentlyContinue
  New-Item -ItemType Directory -Force -Path $databaseRollbackDir | Out-Null
  foreach ($suffix in @('', '-journal', '-wal', '-shm')) {
    $source = "$database$suffix"
    if (Test-Path -LiteralPath $source) {
      Copy-Item -LiteralPath $source -Destination (Join-Path $databaseRollbackDir (Split-Path -Leaf $source))
    }
  }
  return $true
}

function Restore-ExistingDatabase {
  foreach ($suffix in @('', '-journal', '-wal', '-shm')) {
    Remove-Item -LiteralPath "$database$suffix" -Force -ErrorAction SilentlyContinue
    $backup = Join-Path $databaseRollbackDir "focusguard.db$suffix"
    if (Test-Path -LiteralPath $backup) { Copy-Item -LiteralPath $backup -Destination "$database$suffix" -Force }
  }
}

function Configure-Service([string] $OwnerSid) {
  Write-LifecycleTrace 'Validating service payload, host file, and Native Messaging registrations'
  if (-not (Test-Path -LiteralPath $packagedServiceExe)) { throw "Missing packaged service executable: $packagedServiceExe" }
  if (-not (Test-Path -LiteralPath $packagedNativeHostExe)) { throw "Missing packaged Native Messaging host: $packagedNativeHostExe" }
  if (-not (Test-Path -LiteralPath $uiExe)) { throw "Missing packaged UI executable: $uiExe" }
  if (-not (Test-Path -LiteralPath $hostsFile)) { throw "Windows hosts file not found: $hostsFile" }

  Assert-OwnedServicePath
  Assert-NoNativeHostProcess
  Assert-NativeMessagingRegistryAvailable $OwnerSid
  $registered = Get-RegisteredService
  $dataDirExisted = Test-Path -LiteralPath $dataDir
  $databaseExisted = Test-Path -LiteralPath $database -PathType Leaf
  $stateFileExisted = Test-Path -LiteralPath $installStatePath -PathType Leaf

  $createdService = $false
  $serviceBackup = Join-Path $serviceInstallDir 'focusguard-service.exe.rollback'
  $nativeHostBackup = Join-Path $serviceInstallDir 'focuser-native.exe.rollback'
  $backedUpService = $false
  $backedUpNativeHost = $false
  $backedUpDatabase = $false
  $copiedInitialDatabase = $false
  try {
    Write-LifecycleTrace 'Stopping the existing service and preparing protected ProgramData'
    Stop-ServiceForInstall $OwnerSid
    Set-ProgramDataAcl
    Write-LifecycleTrace 'Staging the initial policy database'
    $copiedInitialDatabase = Copy-InitialDatabase $OwnerSid
    if ($registered) { $backedUpDatabase = Backup-ExistingDatabase }

    New-Item -ItemType Directory -Force -Path $serviceInstallDir | Out-Null
    Invoke-Checked "$env:SystemRoot\System32\icacls.exe" @(
      $serviceInstallDir, '/inheritance:r', '/grant:r', '*S-1-5-18:(OI)(CI)F',
      '*S-1-5-32-544:(OI)(CI)F', '*S-1-5-32-545:(OI)(CI)RX'
    )
    if ($registered -and (Test-Path -LiteralPath $serviceExe)) {
      Copy-Item -LiteralPath $serviceExe -Destination $serviceBackup -Force
      $backedUpService = $true
    }
    if ($registered -and (Test-Path -LiteralPath $nativeHostExe)) {
      Copy-Item -LiteralPath $nativeHostExe -Destination $nativeHostBackup -Force
      $backedUpNativeHost = $true
    }
    Copy-Item -LiteralPath $packagedServiceExe -Destination $serviceExe -Force
    Copy-Item -LiteralPath $packagedNativeHostExe -Destination $nativeHostExe -Force
    Write-LifecycleTrace 'Copied service and Native Messaging executables'

    $imagePath = '"{0}" --owner-sid {1} --database "{2}" --hosts-file "{3}" --diagnostics-file "{4}"' -f `
      $serviceExe, $OwnerSid, $database, $hostsFile, $diagnostics
    if ($registered) {
      # The service executable and arguments live in this stable directory and
      # ProgramData path across upgrades. Keep its ImagePath unchanged; sc.exe's
      # native parser is brittle when binPath contains nested quoted arguments.
      Invoke-Checked "$env:SystemRoot\System32\sc.exe" @('config', $serviceName, 'start=', 'auto', 'obj=', 'LocalSystem')
    } else {
      New-Service -Name $serviceName -BinaryPathName $imagePath -DisplayName 'FocusGuard' `
        -StartupType Automatic -ErrorAction Stop | Out-Null
      $createdService = $true
    }
    Invoke-Checked "$env:SystemRoot\System32\sc.exe" @('sidtype', $serviceName, 'unrestricted')
    Invoke-Checked "$env:SystemRoot\System32\sc.exe" @('failure', $serviceName, 'reset=', '0', 'actions=', 'restart/5000/restart/5000/restart/5000')
    $dacl = "D:(A;;CCLCSWRPWPDTLOCRRC;;;SY)(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)(A;;LC;;;$OwnerSid)"
    Invoke-Checked "$env:SystemRoot\System32\sc.exe" @('sdset', $serviceName, $dacl)

    $service = Get-Service -Name $serviceName
    Write-LifecycleTrace "Starting service; current state is $($service.Status)"
    if ($service.Status -ne 'Running') { Start-Service -Name $serviceName }
    Wait-ServiceState 'Running'
    Write-LifecycleTrace 'Service reached Running; registering Native Messaging hosts'
    Register-NativeMessagingHosts
    Remove-OwnerNativeMessagingRegistrations $OwnerSid
    Write-LifecycleTrace 'Native Messaging hosts are registered and recognized legacy entries removed'

    Set-ServiceMetadata -OwnerSid $OwnerSid -InstallDir $InstallDir
    Write-InstallState $OwnerSid
    Remove-Item -LiteralPath $serviceBackup -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $nativeHostBackup -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $databaseRollbackDir -Recurse -Force -ErrorAction SilentlyContinue
    Write-Output "FocusGuard service is running for policy owner $OwnerSid."
    Write-LifecycleTrace 'Install completed and committed'
  } catch {
    Write-LifecycleTrace "Install failed: $_"
    if ($registered) {
      try {
        if ($backedUpService -or $backedUpNativeHost -or $backedUpDatabase) {
          Stop-Service -Name $serviceName -Force -ErrorAction SilentlyContinue
          Wait-ServiceState 'Stopped'
        }
        if ($backedUpService) {
          Copy-Item -LiteralPath $serviceBackup -Destination $serviceExe -Force
        }
        if ($backedUpNativeHost) {
          Copy-Item -LiteralPath $nativeHostBackup -Destination $nativeHostExe -Force
        }
        if ($backedUpDatabase) { Restore-ExistingDatabase }
        Start-Service -Name $serviceName
        Wait-ServiceState 'Running'
        Register-NativeMessagingHosts
        Remove-Item -LiteralPath $serviceBackup -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $nativeHostBackup -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $databaseRollbackDir -Recurse -Force -ErrorAction SilentlyContinue
      } catch {
        throw "Update failed and the previous service could not be restored and restarted: $_"
      }
    } elseif ($createdService) {
      try {
        $created = Get-Service -Name $serviceName -ErrorAction SilentlyContinue
        if ($created -and $created.Status -ne 'Stopped') {
          Stop-Service -Name $serviceName -Force
          Wait-ServiceState 'Stopped'
        }
        $cleanup = Start-Process -FilePath $uiExe -ArgumentList '--remove-hosts-blocks' -PassThru -Wait -WindowStyle Hidden
        if ($cleanup.ExitCode -ne 0) {
          Register-NativeMessagingHosts
          Start-Service -Name $serviceName
          Wait-ServiceState 'Running'
          throw "Install failed and hosts cleanup failed (exit $($cleanup.ExitCode)); enforcement was restarted."
        }
        Remove-NativeMessagingRegistrations
        Remove-OwnerNativeMessagingRegistrations $OwnerSid
        Invoke-Checked "$env:SystemRoot\System32\sc.exe" @('delete', $serviceName)
        Wait-ServiceState 'Absent'
        if (-not $registered) { Remove-ServiceMetadata }
        Remove-Item -LiteralPath $serviceInstallDir -Recurse -Force -ErrorAction SilentlyContinue
        if (-not $stateFileExisted) { Remove-Item -LiteralPath $installStatePath -Force -ErrorAction SilentlyContinue }
        if ($copiedInitialDatabase -and -not $databaseExisted) {
          foreach ($suffix in @('', '-journal', '-wal', '-shm')) {
            Remove-Item -LiteralPath "$database$suffix" -Force -ErrorAction SilentlyContinue
          }
        }
        if (-not $dataDirExisted) { Remove-Item -LiteralPath $dataDir -Recurse -Force -ErrorAction SilentlyContinue }
      } catch {
        $rollbackError = $_
        $recoveryError = $null
        if (Get-RegisteredService) {
          try {
            Register-NativeMessagingHosts
            Start-Service -Name $serviceName
            Wait-ServiceState 'Running'
          } catch { $recoveryError = $_ }
        }
        if ($recoveryError) {
          throw "Install failed and first-install rollback failed ($rollbackError); service recovery also failed: $recoveryError"
        }
        throw "Install failed and first-install rollback failed ($rollbackError); enforcement was restarted if the service remained registered."
      }
    } elseif (-not $registered) {
      # Failures before SCM registration (for example ACL or database staging)
      # may have created only this attempt's temporary ProgramData files.
      if ($copiedInitialDatabase -and -not $databaseExisted) {
        foreach ($suffix in @('', '-journal', '-wal', '-shm')) {
          Remove-Item -LiteralPath "$database$suffix" -Force -ErrorAction SilentlyContinue
        }
      }
      if (-not $stateFileExisted) { Remove-Item -LiteralPath $installStatePath -Force -ErrorAction SilentlyContinue }
      if (-not $dataDirExisted) { Remove-Item -LiteralPath $dataDir -Recurse -Force -ErrorAction SilentlyContinue }
    }
    throw
  }
}

function Remove-FocusGuardService([bool] $RemoveData) {
  Assert-OwnedServicePath
  Assert-NoNativeHostProcess
  $ownerSid = Get-ExistingOwnerSid
  if (-not $ownerSid) { $ownerSid = Get-InstallOwnerSid }
  $registered = Get-RegisteredService
  if ($ownerSid) { Assert-NoOtherOwnerUi $ownerSid }
  elseif (@(Get-UiProcesses).Count -gt 0) {
    throw 'FocusGuard is running and the installer cannot identify its policy owner; close it before uninstalling.'
  }
  Remove-LegacyAutostart $ownerSid
  if ($registered -and $registered.State -ne 'Stopped') {
    Stop-Service -Name $serviceName -Force
    Wait-ServiceState 'Stopped'
  }

  try {
    Remove-NativeMessagingRegistrations
    Remove-OwnerNativeMessagingRegistrations $ownerSid
  } catch {
    $registrationError = $_
    if ($registered) {
      try {
        Register-NativeMessagingHosts
        Start-Service -Name $serviceName
        Wait-ServiceState 'Running'
      } catch {
        throw "Native host removal failed ($registrationError), and enforcement could not be restored: $_"
      }
    }
    throw "Could not remove FocusGuard's Native Messaging registration; enforcement was restarted when a service was registered: $registrationError"
  }

  # The service is stopped first so it cannot race the marked hosts cleanup.
  # If cleanup fails, restart enforcement and keep the service registration.
  try {
    if (-not (Test-Path -LiteralPath $uiExe)) { throw "Missing FocusGuard UI for hosts cleanup: $uiExe" }
    $removeHosts = Start-Process -FilePath $uiExe -ArgumentList '--remove-hosts-blocks' -PassThru -Wait -WindowStyle Hidden
    if ($removeHosts.ExitCode -ne 0) { throw "FocusGuard UI returned exit code $($removeHosts.ExitCode) during hosts cleanup." }
  } catch {
    $hostsCleanupError = $_
    $restartError = $null
    if ($registered) {
      try {
        Register-NativeMessagingHosts
        Start-Service -Name $serviceName
        Wait-ServiceState 'Running'
      } catch { $restartError = $_ }
    }
    if ($restartError) {
      throw "Could not remove FocusGuard's marked hosts entries ($hostsCleanupError); restarting the service also failed: $restartError"
    }
    throw "Could not remove FocusGuard's marked hosts entries ($hostsCleanupError); enforcement has been restarted when a service was registered."
  }

  if ($registered) {
    try {
      Invoke-Checked "$env:SystemRoot\System32\sc.exe" @('delete', $serviceName)
      Wait-ServiceState 'Absent'
    } catch {
      if (Get-RegisteredService) {
        try {
          Register-NativeMessagingHosts
          Start-Service -Name $serviceName
          Wait-ServiceState 'Running'
        } catch { throw "Uninstall could not delete the service and enforcement could not be restored: $_" }
      }
      throw
    }
  }

  Remove-ServiceMetadata
  $normalizedServiceDir = [IO.Path]::GetFullPath($serviceInstallDir).TrimEnd('\')
  $allowedServiceDir = [IO.Path]::GetFullPath((Join-Path $programFilesRoot 'PublikHQ\FocusGuard\Service')).TrimEnd('\')
  if ($normalizedServiceDir -ne $allowedServiceDir) { throw 'Refusing to remove service files outside the FocusGuard Program Files directory.' }
  Remove-Item -LiteralPath $normalizedServiceDir -Recurse -Force -ErrorAction Stop
  if ($RemoveData) {
    $normalizedDataDir = [IO.Path]::GetFullPath($dataDir).TrimEnd('\')
    $allowedDataDir = [IO.Path]::GetFullPath((Join-Path $env:ProgramData 'PublikHQ\FocusGuard')).TrimEnd('\')
    if ($normalizedDataDir -ne $allowedDataDir) { throw 'Refusing to remove data outside the FocusGuard ProgramData directory.' }
    Remove-Item -LiteralPath $normalizedDataDir -Recurse -Force -ErrorAction SilentlyContinue
    if ($ownerSid) {
      $profile = Get-OwnerProfile $ownerSid
      Remove-Item -LiteralPath (Join-Path $profile 'AppData\Roaming\publikhq\FocusGuard') -Recurse -Force -ErrorAction SilentlyContinue
      Remove-Item -LiteralPath (Join-Path $profile 'AppData\Local\publikhq\FocusGuard') -Recurse -Force -ErrorAction SilentlyContinue
    }
  }
  Write-Output 'FocusGuard service and marked hosts entries were removed.'
}

try {
  Write-LifecycleTrace 'Lifecycle helper started'
  $principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
  if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'The FocusGuard service lifecycle requires an elevated per-machine installer.'
  }
  $InstallDir = [IO.Path]::GetFullPath($InstallDir)
  $serviceInstallDir = [IO.Path]::GetFullPath($serviceInstallDir)
  $serviceExe = [IO.Path]::GetFullPath($serviceExe)
  $packagedServiceExe = [IO.Path]::GetFullPath($packagedServiceExe)
  $uiExe = [IO.Path]::GetFullPath($uiExe)

  switch ($Mode) {
    'PreflightInstall' {
      Write-LifecycleTrace 'Checking install owner'
      $ownerSid = Get-InstallOwnerSid
      Write-LifecycleTrace "Install owner is $ownerSid; checking existing service and UI processes"
      Assert-OwnedServicePath
      Assert-NoOtherOwnerUi $ownerSid
      Assert-NoNativeHostProcess
      Write-LifecycleTrace 'Checking existing install data and browser registrations'
      $registered = Get-RegisteredService
      $state = Get-InstallState
      if ($state -and $state.owner_sid -ne $ownerSid) {
        throw "FocusGuard's committed policy belongs to $($state.owner_sid), but this install session is $ownerSid. Sign in to the original policy owner to update or remove it."
      }
      if (-not $registered -and (Test-Path -LiteralPath $dataDir) -and -not $state) {
        throw "An uncommitted FocusGuard data folder already exists at '$dataDir'. Preserve its contents and review it before retrying installation."
      }
      if ($state -and -not (Test-Path -LiteralPath $database -PathType Leaf)) {
        throw "FocusGuard's committed policy database is missing at '$database'; preserve the remaining data and repair the installation before retrying."
      }
      Assert-NativeMessagingRegistryAvailable $ownerSid
      Write-Output "Install preflight passed for policy owner $ownerSid."
      Write-LifecycleTrace 'Install preflight completed'
    }
    'PrepareInstall' {
      Write-LifecycleTrace 'Preparing install and migrating legacy autostart'
      $ownerSid = Get-InstallOwnerSid
      Assert-OwnedServicePath
      Assert-NoOtherOwnerUi $ownerSid
      if (@(Get-UiProcesses).Count -gt 0) {
        throw 'FocusGuard is still running. Close its window and retry before installing.'
      }
      Assert-NoNativeHostProcess
      Remove-LegacyAutostart $ownerSid $true
      Write-Output "Install preparation passed for policy owner $ownerSid; existing service enforcement remains active until the service update."
      Write-LifecycleTrace 'Install preparation completed'
    }
    'Install' {
      Write-LifecycleTrace 'Configuring Windows service'
      $ownerSid = Get-InstallOwnerSid
      Configure-Service $ownerSid
    }
    'PreflightRemove' {
      Assert-OwnedServicePath
      $ownerSid = Get-ExistingOwnerSid
      if (-not $ownerSid) { $ownerSid = Get-InstallOwnerSid }
      Assert-NoOtherOwnerUi $ownerSid
      Assert-NoNativeHostProcess
      Write-Output 'Uninstall preflight passed.'
    }
    'Remove' {
      Write-LifecycleTrace 'Removing Windows service'
      $removeData = $DeleteData -eq '1'
      Remove-FocusGuardService $removeData
    }
  }
  Write-LifecycleTrace 'Lifecycle helper completed successfully'
  exit 0
} catch {
  Write-LifecycleTrace "Lifecycle helper failed: $_"
  [Console]::Error.WriteLine("FocusGuard service lifecycle failed: $_")
  exit 1
}
