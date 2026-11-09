param(
  [Parameter(Mandatory = $true)]
  [string] $InstallerPath
)

$ErrorActionPreference = 'Stop'

if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_OS -ne 'Windows' -or
    $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
  throw 'The installer lifecycle test may run only on a GitHub-hosted Windows runner.'
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
  throw 'The GitHub-hosted runner token is not elevated; refusing to install a service.'
}

$serviceName = 'FocusGuardService'
$installer = (Resolve-Path -LiteralPath $InstallerPath).Path
$probeExe = Join-Path $PSScriptRoot '..\target\release\examples\scm_policy_probe.exe'
$hostsPath = Join-Path $env:SystemRoot 'System32\drivers\etc\hosts'
$dataDir = Join-Path $env:ProgramData 'PublikHQ\FocusGuard'
$programFilesRoot = if ($env:ProgramW6432) { $env:ProgramW6432 } else { $env:ProgramFiles }
$serviceDir = Join-Path $programFilesRoot 'PublikHQ\FocusGuard\Service'
$nativeHostExe = Join-Path $serviceDir 'focuser-native.exe'
$nativeManifestDir = Join-Path $serviceDir 'native-messaging'
$chromeExtensionId = (Get-Content -LiteralPath (Join-Path $PSScriptRoot '..\extension\identity.json') -Raw | ConvertFrom-Json).chrome_extension_id
$serviceMetadataPath = 'SOFTWARE\PublikHQ\FocusGuard'
$ownerSid = $identity.User.Value
$legacyManifestDir = Join-Path $env:APPDATA 'focuser\FocusGuard\data\native-messaging'
$legacyChromeManifest = Join-Path $legacyManifestDir 'com.focuser.native.json'
$legacyFirefoxManifest = Join-Path $legacyManifestDir 'com.focuser.native.firefox.json'
$legacyNativeExecutable = Join-Path $env:RUNNER_TEMP 'legacy-focuser-native.exe'
$legacyUiExecutable = Join-Path $env:LOCALAPPDATA 'FocusGuard\focusguard-ui.exe'
$nativeRegistrationSpecs = @(
  [pscustomobject]@{ Key = 'Software\Google\Chrome\NativeMessagingHosts\com.focuser.native'; Manifest = $legacyChromeManifest },
  [pscustomobject]@{ Key = 'Software\Microsoft\Edge\NativeMessagingHosts\com.focuser.native'; Manifest = $legacyChromeManifest },
  [pscustomobject]@{ Key = 'Software\Mozilla\NativeMessagingHosts\com.focuser.native'; Manifest = $legacyFirefoxManifest }
)
$testDomain = 'focuser-scm-recovery.invalid'
$hostsSnapshot = [IO.File]::ReadAllBytes($hostsPath)
$hostsAcl = Get-Acl -LiteralPath $hostsPath
$hostsAttributes = [IO.File]::GetAttributes($hostsPath)
$hostsWriteTimeUtc = [IO.File]::GetLastWriteTimeUtc($hostsPath)
$hostsHash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($hostsSnapshot))
$lifecycleTracePath = Join-Path $env:RUNNER_TEMP 'focusguard-lifecycle.log'
$nsisLogPath = Join-Path $env:RUNNER_TEMP 'focusguard-installer.log'
$env:FOCUSGUARD_LIFECYCLE_LOG = $lifecycleTracePath
$uninstallerPath = $null
$testMayOwnSystemState = $false
$hostsMayHaveChanged = $false
$safeToRestoreHosts = $false
$legacyRegistrationSeeded = $false
$legacyAutostartSeeded = $false

function Get-ProductService {
  Get-CimInstance Win32_Service -Filter "Name='$serviceName'" -ErrorAction Stop
}

function Get-ServiceMetadataViews {
  if ([Environment]::Is64BitOperatingSystem) {
    return @([Microsoft.Win32.RegistryView]::Registry64, [Microsoft.Win32.RegistryView]::Registry32)
  }
  return @([Microsoft.Win32.RegistryView]::Default)
}

function Test-ProductMetadata {
  foreach ($view in (Get-ServiceMetadataViews)) {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, $view)
    try {
      $key = $base.OpenSubKey($serviceMetadataPath, $false)
      if ($key) { $key.Dispose(); return $true }
    } finally { $base.Dispose() }
  }
  return $false
}

function Get-ProductMetadata {
  $view = if ([Environment]::Is64BitOperatingSystem) {
    [Microsoft.Win32.RegistryView]::Registry64
  } else {
    [Microsoft.Win32.RegistryView]::Default
  }
  $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, $view)
  try {
    $key = $base.OpenSubKey($serviceMetadataPath, $false)
    if (-not $key) { throw "FocusGuard service metadata is missing from the expected registry view $view." }
    try {
      return [pscustomobject]@{
        OwnerSid = $key.GetValue('OwnerSid')
        InstallDir = $key.GetValue('InstallDir')
      }
    } finally { $key.Dispose() }
  } finally { $base.Dispose() }
}

function Remove-TestServiceMetadata {
  foreach ($view in (Get-ServiceMetadataViews)) {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, $view)
    try { $base.DeleteSubKeyTree($serviceMetadataPath, $false) }
    finally { $base.Dispose() }
  }
}

function Wait-ProductService([string] $State, [uint32] $PreviousPid = 0, [int] $TimeoutSeconds = 60) {
  $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
  do {
    $service = Get-ProductService
    if ($State -eq 'Absent' -and -not $service) { return $null }
    if ($service -and $service.State -eq $State -and
        ($PreviousPid -eq 0 -or $service.ProcessId -ne $PreviousPid)) {
      return $service
    }
    Start-Sleep -Milliseconds 500
  } while ([DateTime]::UtcNow -lt $deadline)
  $service = Get-ProductService
  $actual = if ($service) { "$($service.State), PID $($service.ProcessId)" } else { 'absent' }
  throw "FocusGuard service did not reach '$State' (current state: $actual)."
}

function Invoke-Probe([string] $Stage) {
  $env:FOCUSGUARD_SCM_RUNTIME_TEST = '1'
  try {
    & $probeExe $Stage
    if ($LASTEXITCODE -ne 0) { throw "The authenticated service probe '$Stage' exited $LASTEXITCODE." }
  } finally {
    Remove-Item Env:\FOCUSGUARD_SCM_RUNTIME_TEST -ErrorAction SilentlyContinue
  }
}

function Invoke-NativeHeartbeat {
  if ($PSVersionTable.PSVersion -lt [version]'7.4') {
    throw 'The installed Native Messaging smoke test requires PowerShell 7.4 or newer.'
  }
  $requestPath = Join-Path $env:RUNNER_TEMP 'focusguard-native-request.bin'
  $responsePath = Join-Path $env:RUNNER_TEMP 'focusguard-native-response.bin'
  $errorPath = Join-Path $env:RUNNER_TEMP 'focusguard-native-stderr.log'
  $message = @{
    msg_type = 'Event'
    payload = @{
      kind = 'heartbeat'
      browser = 'Chrome'
      profile_id = '00000000-0000-4000-8000-000000000001'
      incognito_allowed = $true
    }
    request_id = 775
  } | ConvertTo-Json -Compress -Depth 5
  $jsonBytes = [Text.UTF8Encoding]::new($false).GetBytes($message)
  $lengthBytes = [BitConverter]::GetBytes([uint32] $jsonBytes.Length)
  $frame = New-Object byte[] ($lengthBytes.Length + $jsonBytes.Length)
  [Array]::Copy($lengthBytes, 0, $frame, 0, $lengthBytes.Length)
  [Array]::Copy($jsonBytes, 0, $frame, $lengthBytes.Length, $jsonBytes.Length)
  [IO.File]::WriteAllBytes($requestPath, $frame)

  $process = $null
  try {
    $nativeArgs = @("chrome-extension://$chromeExtensionId/", '--parent-window=0')
    $process = Start-Process -FilePath $nativeHostExe -ArgumentList $nativeArgs -PassThru -WindowStyle Hidden `
      -RedirectStandardInput $requestPath -RedirectStandardOutput $responsePath -RedirectStandardError $errorPath
    if (-not $process.WaitForExit(15000)) { throw 'Installed Native Messaging host exceeded its 15-second timeout.' }
  } finally {
    if ($null -ne $process -and -not $process.HasExited) {
      Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
      if (-not $process.WaitForExit(5000)) { throw 'Installed Native Messaging host did not exit after forced cleanup.' }
    }
  }
  if ($process.ExitCode -ne 0) {
    $stderr = if (Test-Path $errorPath) { Get-Content -Raw $errorPath } else { '' }
    throw "Installed Native Messaging host exited $($process.ExitCode): $stderr"
  }
  $responseFrame = [IO.File]::ReadAllBytes($responsePath)
  if ($responseFrame.Length -lt 4) { throw 'Installed Native Messaging host returned a truncated frame.' }
  $responseLength = [BitConverter]::ToUInt32($responseFrame, 0)
  if ($responseLength -eq 0 -or $responseLength -ne ($responseFrame.Length - 4)) {
    throw 'Installed Native Messaging host returned an invalid frame length.'
  }
  $responseJson = [Text.UTF8Encoding]::new($false).GetString($responseFrame, 4, [int] $responseLength)
  $response = $responseJson | ConvertFrom-Json
  if ($response.msg_type -ne 'Event' -or $response.request_id -ne 775 -or $response.payload.ok -ne $true) {
    throw "Installed Native Messaging host did not acknowledge its authenticated service event: $responseJson"
  }
}

function Wait-HostsRule {
  $deadline = [DateTime]::UtcNow.AddSeconds(20)
  do {
    $content = [IO.File]::ReadAllText($hostsPath)
    if ($content -match "(?m)^127\.0\.0\.1\s+$testDomain\s*$" -and
        $content -match "(?m)^127\.0\.0\.1\s+www\.$testDomain\s*$") { return }
    Start-Sleep -Milliseconds 250
  } while ([DateTime]::UtcNow -lt $deadline)
  throw "The real Windows hosts file did not contain the expected exact-domain rule for $testDomain."
}

function Start-HiddenAndWait([string] $Path, [string[]] $Arguments, [int] $TimeoutSeconds = 180) {
  $process = Start-Process -FilePath $Path -ArgumentList $Arguments -PassThru -WindowStyle Hidden
  if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
    $diagnostics = [Collections.Generic.List[string]]::new()
    if (Test-Path -LiteralPath $lifecycleTracePath) {
      $diagnostics.Add("Lifecycle trace:`n$((Get-Content -LiteralPath $lifecycleTracePath -Tail 80) -join [Environment]::NewLine)")
    }
    if ([IO.Path]::GetFullPath($Path) -eq [IO.Path]::GetFullPath($installer)) {
      $focusProcesses = @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | Where-Object {
        $_.Name -in @('powershell.exe', 'pwsh.exe', 'focusguard-ui.exe', 'focusguard-service.exe', 'focuser-native.exe')
      } | Select-Object ProcessId, ParentProcessId, Name, CommandLine)
      if ($focusProcesses.Count -gt 0) {
        $diagnostics.Add("Related process snapshot:`n$(($focusProcesses | Format-Table -AutoSize | Out-String).Trim())")
      }
      if (Test-Path -LiteralPath $nsisLogPath) {
        $diagnostics.Add("NSIS log:`n$((Get-Content -LiteralPath $nsisLogPath -Tail 80) -join [Environment]::NewLine)")
      }
    }
    Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
    $detail = if ($diagnostics.Count -gt 0) { "`n$($diagnostics -join "`n")" } else { '' }
    throw "$([IO.Path]::GetFileName($Path)) exceeded its $TimeoutSeconds-second timeout.$detail"
  }
  if ($process.ExitCode -ne 0) {
    $detail = ''
    if ([IO.Path]::GetFullPath($Path) -eq [IO.Path]::GetFullPath($installer)) {
      $diagnostics = [Collections.Generic.List[string]]::new()
      if (Test-Path -LiteralPath $lifecycleTracePath) {
        $diagnostics.Add("Lifecycle trace:`n$((Get-Content -LiteralPath $lifecycleTracePath -Tail 100) -join [Environment]::NewLine)")
      }
      if (Test-Path -LiteralPath $nsisLogPath) {
        $diagnostics.Add("NSIS log:`n$((Get-Content -LiteralPath $nsisLogPath -Tail 100) -join [Environment]::NewLine)")
      }
      if ($diagnostics.Count -gt 0) { $detail = "`n$($diagnostics -join "`n")" }
    }
    throw "$([IO.Path]::GetFileName($Path)) exited $($process.ExitCode).$detail"
  }
}

function Get-TestRegistryViews {
  if ([Environment]::Is64BitOperatingSystem) {
    return @([Microsoft.Win32.RegistryView]::Registry64, [Microsoft.Win32.RegistryView]::Registry32)
  }
  return @([Microsoft.Win32.RegistryView]::Default)
}

function Assert-NoLegacyNativeRegistrations {
  foreach ($view in (Get-TestRegistryViews)) {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::CurrentUser, $view)
    try {
      foreach ($spec in $nativeRegistrationSpecs) {
        $key = $base.OpenSubKey($spec.Key, $false)
        if ($key) {
          try {
            if ($key.GetValue('')) { throw "A legacy native host registration already exists at $($spec.Key) in $view." }
          } finally { $key.Dispose() }
        }
      }
    } finally { $base.Dispose() }
  }
  if (Test-Path -LiteralPath $legacyManifestDir) {
    throw "The legacy per-user native-host directory already exists at '$legacyManifestDir'; refusing to overwrite it."
  }
}

function Seed-LegacyNativeRegistrations {
  New-Item -ItemType Directory -Force -Path $legacyManifestDir | Out-Null
  $chrome = @{
    name = 'com.focuser.native'
    description = 'Legacy FocusGuard Native Messaging Host'
    path = $legacyNativeExecutable
    type = 'stdio'
    allowed_origins = @("chrome-extension://$chromeExtensionId/")
  } | ConvertTo-Json -Depth 4
  $firefox = @{
    name = 'com.focuser.native'
    description = 'Legacy FocusGuard Native Messaging Host'
    path = $legacyNativeExecutable
    type = 'stdio'
    allowed_extensions = @('focusguard@publikhq.com')
  } | ConvertTo-Json -Depth 4
  [IO.File]::WriteAllText($legacyChromeManifest, $chrome, [Text.UTF8Encoding]::new($false))
  [IO.File]::WriteAllText($legacyFirefoxManifest, $firefox, [Text.UTF8Encoding]::new($false))
  foreach ($view in (Get-TestRegistryViews)) {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::CurrentUser, $view)
    try {
      foreach ($spec in $nativeRegistrationSpecs) {
        $key = $base.CreateSubKey($spec.Key)
        try { $key.SetValue('', $spec.Manifest, [Microsoft.Win32.RegistryValueKind]::String) }
        finally { $key.Dispose() }
      }
    } finally { $base.Dispose() }
  }
  $script:legacyRegistrationSeeded = $true
}

function Seed-LegacyAutostart {
  $action = New-ScheduledTaskAction -Execute $legacyUiExecutable -Argument '--autostart'
  $trigger = New-ScheduledTaskTrigger -AtLogOn -User $identity.Name
  $principal = New-ScheduledTaskPrincipal -UserId $identity.Name -LogonType Interactive -RunLevel Highest
  Register-ScheduledTask -TaskName 'FocusGuard' -TaskPath '\' -Action $action -Trigger $trigger -Principal $principal -Force | Out-Null

  $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::CurrentUser, [Microsoft.Win32.RegistryView]::Default)
  try {
    $key = $base.CreateSubKey('Software\Microsoft\Windows\CurrentVersion\Run')
    try { $key.SetValue('FocusGuard', "`"$legacyUiExecutable`" --autostart", [Microsoft.Win32.RegistryValueKind]::String) }
    finally { $key.Dispose() }
  } finally { $base.Dispose() }
  $script:legacyAutostartSeeded = $true
}

function Assert-LegacyNativeRegistrationsRemoved {
  foreach ($view in (Get-TestRegistryViews)) {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::CurrentUser, $view)
    try {
      foreach ($spec in $nativeRegistrationSpecs) {
        $key = $base.OpenSubKey($spec.Key, $false)
        if ($key) {
          try {
            $current = [string]$key.GetValue('')
            if ($current -and [string]::Equals($current, $spec.Manifest, [StringComparison]::OrdinalIgnoreCase)) {
              throw "The legacy per-user registration $($spec.Key) still points at '$current' in $view."
            }
          } finally { $key.Dispose() }
        }
      }
    } finally { $base.Dispose() }
  }
  if ((Test-Path -LiteralPath $legacyChromeManifest) -or (Test-Path -LiteralPath $legacyFirefoxManifest)) {
    throw 'The old per-user Native Messaging manifest files remain after migration or uninstall.'
  }
}

function Assert-LegacyAutostartMigrated {
  $task = Get-ScheduledTask -TaskPath '\' -TaskName 'FocusGuard' -ErrorAction SilentlyContinue
  if ($task) { throw 'The old highest-privilege FocusGuard logon task was not removed.' }
  $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::CurrentUser, [Microsoft.Win32.RegistryView]::Default)
  try {
    $key = $base.OpenSubKey('Software\Microsoft\Windows\CurrentVersion\Run', $false)
    if (-not $key) { throw 'The old FocusGuard logon preference was not migrated to the unelevated Run key.' }
    try {
      $value = [string]$key.GetValue('FocusGuard')
      if (-not $value -or $value -notmatch '^\s*"([^"]+)"\s+--autostart\s*$' -or
          -not [string]::Equals([IO.Path]::GetFullPath($Matches[1]), [IO.Path]::GetFullPath((Join-Path $installDir 'focusguard-ui.exe')), [StringComparison]::OrdinalIgnoreCase)) {
        throw "The legacy autostart preference was not rewritten to the installed unelevated UI path: '$value'."
      }
    } finally { $key.Dispose() }
  } finally { $base.Dispose() }
}

function Assert-LegacyAutostartRemoved {
  if (Get-ScheduledTask -TaskPath '\' -TaskName 'FocusGuard' -ErrorAction SilentlyContinue) {
    throw 'Uninstall left the FocusGuard logon task behind.'
  }
  $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::CurrentUser, [Microsoft.Win32.RegistryView]::Default)
  try {
    $key = $base.OpenSubKey('Software\Microsoft\Windows\CurrentVersion\Run', $false)
    if ($key) {
      try {
        if ($key.GetValue('FocusGuard')) { throw 'Uninstall left the FocusGuard per-user logon value behind.' }
      } finally { $key.Dispose() }
    }
  } finally { $base.Dispose() }
}

function Remove-TestLegacyNativeRegistrations {
  if (-not $legacyRegistrationSeeded) { return }
  foreach ($view in (Get-TestRegistryViews)) {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::CurrentUser, $view)
    try {
      foreach ($spec in $nativeRegistrationSpecs) {
        $key = $base.OpenSubKey($spec.Key, $true)
        if (-not $key) { continue }
        try {
          $current = [string]$key.GetValue('')
          if ($current -and [string]::Equals($current, $spec.Manifest, [StringComparison]::OrdinalIgnoreCase)) {
            $key.DeleteValue('', $false)
          }
        } finally { $key.Dispose() }
      }
    } finally { $base.Dispose() }
  }
  Remove-Item -LiteralPath $legacyManifestDir -Recurse -Force -ErrorAction SilentlyContinue
}

function Remove-TestLegacyAutostart {
  if (-not $legacyAutostartSeeded) { return }
  $task = Get-ScheduledTask -TaskPath '\' -TaskName 'FocusGuard' -ErrorAction SilentlyContinue
  if ($task -and @($task.Actions | Where-Object { [string]::Equals([IO.Path]::GetFullPath($_.Execute), [IO.Path]::GetFullPath($legacyUiExecutable), [StringComparison]::OrdinalIgnoreCase) }).Count -gt 0) {
    Unregister-ScheduledTask -TaskPath '\' -TaskName 'FocusGuard' -Confirm:$false -ErrorAction SilentlyContinue
  }
  $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::CurrentUser, [Microsoft.Win32.RegistryView]::Default)
  try {
    $key = $base.OpenSubKey('Software\Microsoft\Windows\CurrentVersion\Run', $true)
    if ($key) {
      try {
        $value = [string]$key.GetValue('FocusGuard')
        if ($value -and ($value.Contains($legacyUiExecutable) -or $value.Contains('PublikHQ\FocusGuard\focusguard-ui.exe'))) {
          $key.DeleteValue('FocusGuard', $false)
        }
      } finally { $key.Dispose() }
    }
  } finally { $base.Dispose() }
}

try {
  if (-not (Test-Path -LiteralPath $probeExe)) { throw "Missing installer policy probe: $probeExe" }
  if (Get-ProductService) { throw "A service named $serviceName already exists; refusing to modify it." }
  if (Get-ScheduledTask -TaskPath '\' -TaskName 'FocusGuard' -ErrorAction SilentlyContinue) { throw 'A scheduled task named FocusGuard already exists; refusing to modify it.' }
  if (Test-ProductMetadata) { throw 'FocusGuard service metadata already exists in a Windows registry view; refusing to modify it.' }
  if (Test-Path -LiteralPath $dataDir) { throw 'FocusGuard ProgramData already exists; refusing to modify it.' }
  if (Test-Path -LiteralPath $serviceDir) { throw 'FocusGuard service files already exist; refusing to modify them.' }
  foreach ($keyPath in @(
      'HKLM:\SOFTWARE\Google\Chrome\NativeMessagingHosts\com.focuser.native',
      'HKLM:\SOFTWARE\Microsoft\Edge\NativeMessagingHosts\com.focuser.native',
      'HKLM:\SOFTWARE\Mozilla\NativeMessagingHosts\com.focuser.native')) {
    if (Test-Path -LiteralPath $keyPath) { throw "A machine-wide native messaging registration already exists at $keyPath." }
  }
  Assert-NoLegacyNativeRegistrations
  $initialHosts = [Text.Encoding]::UTF8.GetString($hostsSnapshot)
  if ($initialHosts -match '(?im)^# .*FOCUSGUARD BLOCK' -or $initialHosts.Contains($testDomain)) {
    throw 'The Windows hosts file already contains FocusGuard or test entries; refusing to overwrite them.'
  }

  $testMayOwnSystemState = $true
  $hostsMayHaveChanged = $true
  Seed-LegacyNativeRegistrations
  Seed-LegacyAutostart
  Write-Host 'Installing FocusGuard silently on the disposable GitHub-hosted runner.'
  Start-HiddenAndWait $installer @('/S', "/LOG=$nsisLogPath")
  $service = Wait-ProductService 'Running'
  $metadata = Get-ProductMetadata
  $installDir = $metadata.InstallDir
  if (-not $installDir) { throw 'The installer did not persist its installation directory.' }
  $uninstallerPath = Join-Path $installDir 'uninstall.exe'
  if (-not (Test-Path -LiteralPath $uninstallerPath)) { throw "Missing uninstaller at $uninstallerPath" }
  if ($service.StartMode -ne 'Auto' -or $service.StartName -ne 'LocalSystem') {
    throw "Unexpected service configuration: start=$($service.StartMode), account=$($service.StartName)."
  }
  if ($service.PathName -notlike "*$serviceDir*") { throw "Service executable is outside its protected Program Files location: $($service.PathName)" }
  $savedOwnerSid = $metadata.OwnerSid
  if ($savedOwnerSid -ne $identity.User.Value) { throw 'The installer persisted an unexpected policy owner SID.' }
  $installState = Get-Content -LiteralPath (Join-Path $dataDir 'install-state.json') -Raw | ConvertFrom-Json
  if ($installState.schema -ne 1 -or $installState.owner_sid -ne $identity.User.Value) {
    throw 'The installer did not commit a matching ProgramData policy state marker.'
  }
  Assert-LegacyNativeRegistrationsRemoved
  Assert-LegacyAutostartMigrated

  $chromeManifest = Get-Content -LiteralPath (Join-Path $nativeManifestDir 'com.focuser.native.json') -Raw | ConvertFrom-Json
  $firefoxManifest = Get-Content -LiteralPath (Join-Path $nativeManifestDir 'com.focuser.native.firefox.json') -Raw | ConvertFrom-Json
  if ($chromeManifest.allowed_origins.Count -ne 1 -or
      $chromeManifest.allowed_origins[0] -ne "chrome-extension://$chromeExtensionId/" -or
      [IO.Path]::GetFullPath($chromeManifest.path) -ine [IO.Path]::GetFullPath($nativeHostExe)) {
    throw 'The installed Chromium Native Messaging manifest does not match the pinned extension identity and host path.'
  }
  if ($firefoxManifest.allowed_extensions.Count -ne 1 -or
      $firefoxManifest.allowed_extensions[0] -ne 'focusguard@publikhq.com' -or
      [IO.Path]::GetFullPath($firefoxManifest.path) -ine [IO.Path]::GetFullPath($nativeHostExe)) {
    throw 'The installed Firefox Native Messaging manifest is invalid.'
  }
  foreach ($keyPath in @(
      'HKLM:\SOFTWARE\Google\Chrome\NativeMessagingHosts\com.focuser.native',
      'HKLM:\SOFTWARE\Microsoft\Edge\NativeMessagingHosts\com.focuser.native',
      'HKLM:\SOFTWARE\Mozilla\NativeMessagingHosts\com.focuser.native')) {
    if (-not (Test-Path -LiteralPath $keyPath)) { throw "Missing per-machine Native Messaging registration: $keyPath" }
  }

  Invoke-Probe 'seed'
  Wait-HostsRule
  Write-Host 'The installed service applied apex and www rules to the real runner hosts file.'
  Invoke-NativeHeartbeat
  Invoke-Probe 'verify'
  Write-Host 'The installed Native Messaging host launched with the pinned Chrome origin and reached the service.'

  # Exercise NSIS maintenance/update mode against the just-installed package.
  # The old service remains active while the UI files are copied; POSTINSTALL
  # then backs up policy, replaces stable service binaries, and restarts it.
  $preUpdatePid = [uint32] $service.ProcessId
  Start-HiddenAndWait $installer @('/S', '/UPDATE', "/LOG=$nsisLogPath")
  $service = Wait-ProductService 'Running' $preUpdatePid
  Wait-HostsRule
  Invoke-Probe 'verify'
  Write-Host "Update-mode install kept policy active and restarted the service as PID $($service.ProcessId)."

  # Simulate a privileged edit that removes FocusGuard's owned section. The
  # service reconciler must restore policy from its database without UI input.
  [IO.File]::WriteAllBytes($hostsPath, $hostsSnapshot)
  Wait-HostsRule
  Write-Host 'The service restored hosts policy after a direct hosts-file edit.'

  # Verify that the packaged SCM failure action restarts the installed service
  # and the restarted instance reconstructs policy from ProgramData.
  $previousPid = [uint32] $service.ProcessId
  Stop-Process -Id $previousPid -Force
  $service = Wait-ProductService 'Running' $previousPid
  Wait-HostsRule
  Write-Host "SCM restarted the installed service as PID $($service.ProcessId) and restored the policy."

  Start-HiddenAndWait $uninstallerPath @('/S')
  $null = Wait-ProductService 'Absent'
  if ([IO.Directory]::Exists($serviceDir)) { throw 'Uninstall left the FocusGuard service executable directory behind.' }
  foreach ($keyPath in @(
      'HKLM:\SOFTWARE\Google\Chrome\NativeMessagingHosts\com.focuser.native',
      'HKLM:\SOFTWARE\Microsoft\Edge\NativeMessagingHosts\com.focuser.native',
      'HKLM:\SOFTWARE\Mozilla\NativeMessagingHosts\com.focuser.native')) {
    if (Test-Path -LiteralPath $keyPath) { throw "Uninstall left a Native Messaging registration: $keyPath" }
  }
  Assert-LegacyNativeRegistrationsRemoved
  Assert-LegacyAutostartRemoved
  if (-not (Test-Path -LiteralPath (Join-Path $dataDir 'install-state.json'))) {
    throw 'Uninstall unexpectedly deleted policy data without the explicit delete-data option.'
  }
  $remaining = [IO.File]::ReadAllText($hostsPath)
  if ($remaining -match '(?im)^# .*FOCUSGUARD BLOCK' -or $remaining.Contains($testDomain)) {
    throw 'Uninstall left FocusGuard or test entries in the Windows hosts file.'
  }
  $currentHash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([IO.File]::ReadAllBytes($hostsPath)))
  if ($currentHash -ne $hostsHash) { throw 'Uninstall did not restore the original Windows hosts-file bytes.' }
  Write-Host 'Silent uninstall removed the service and restored the hosts file byte for byte.'
} catch {
  Write-Host "Windows installer smoke failed: $($_.Exception.Message)"
  if (Test-Path -LiteralPath $lifecycleTracePath) {
    Write-Host "Lifecycle trace:`n$((Get-Content -LiteralPath $lifecycleTracePath -Tail 100) -join [Environment]::NewLine)"
  }
  if (Test-Path -LiteralPath $nsisLogPath) {
    Write-Host "NSIS log:`n$((Get-Content -LiteralPath $nsisLogPath -Tail 100) -join [Environment]::NewLine)"
  }
  throw
} finally {
  # The runner is disposable, but restore the real system file before the job
  # exits even when an assertion or installer step fails.
  if ($hostsMayHaveChanged) {
    try {
      if ($uninstallerPath -and (Test-Path -LiteralPath $uninstallerPath)) {
        try { Start-HiddenAndWait $uninstallerPath @('/S') }
        catch { Write-Warning "The silent cleanup uninstaller failed; removing the guarded test service directly: $_" }
      }
      $service = Get-ProductService
      if ($service) {
        if ($service.PathName -notlike "*$serviceDir*") {
          throw "The registered service no longer points to the guarded test directory; refusing to stop it: $($service.PathName)"
        }
        if ($service.State -ne 'Stopped') { Stop-Service -Name $serviceName -Force -ErrorAction Stop }
        $null = Wait-ProductService 'Stopped' -TimeoutSeconds 30
        $safeToRestoreHosts = $true
        & sc.exe delete $serviceName | Out-Null
        $null = Wait-ProductService 'Absent' -TimeoutSeconds 30
      } else {
        $safeToRestoreHosts = $true
      }
    } catch {
      Write-Error "Could not stop and remove the test service before restoring hosts: $_"
      throw
    } finally {
      if (-not $safeToRestoreHosts) {
        throw 'Refusing to rewrite the hosts file while the test service may still be active.'
      }
      [IO.File]::WriteAllBytes($hostsPath, $hostsSnapshot)
      Set-Acl -LiteralPath $hostsPath -AclObject $hostsAcl
      [IO.File]::SetAttributes($hostsPath, $hostsAttributes)
      [IO.File]::SetLastWriteTimeUtc($hostsPath, $hostsWriteTimeUtc)
      $restoredHash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([IO.File]::ReadAllBytes($hostsPath)))
      if ($restoredHash -ne $hostsHash) { throw 'The cleanup step could not restore the original hosts-file bytes.' }

      if ($testMayOwnSystemState -and -not (Get-ProductService)) {
        Remove-TestServiceMetadata
        $expectedDataDir = [IO.Path]::GetFullPath((Join-Path $env:ProgramData 'PublikHQ\FocusGuard')).TrimEnd('\')
        if ([IO.Path]::GetFullPath($dataDir).TrimEnd('\') -eq $expectedDataDir) {
          Remove-Item -LiteralPath $dataDir -Recurse -Force -ErrorAction SilentlyContinue
        }
        $expectedServiceDir = [IO.Path]::GetFullPath((Join-Path $programFilesRoot 'PublikHQ\FocusGuard\Service')).TrimEnd('\')
        if ([IO.Path]::GetFullPath($serviceDir).TrimEnd('\') -eq $expectedServiceDir) {
          Remove-Item -LiteralPath $serviceDir -Recurse -Force -ErrorAction SilentlyContinue
        }
      }
      Remove-TestLegacyNativeRegistrations
      Remove-TestLegacyAutostart
      Remove-Item Env:\FOCUSGUARD_LIFECYCLE_LOG -ErrorAction SilentlyContinue
    }
  }
}
