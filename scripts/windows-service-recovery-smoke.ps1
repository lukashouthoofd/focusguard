$ErrorActionPreference = 'Stop'

if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_OS -ne 'Windows' -or
    $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    throw 'This SCM runtime test may run only on a GitHub-hosted Windows runner.'
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'The GitHub-hosted runner token is not elevated; refusing to install a service.'
}

$serviceName = 'FocusGuardService'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$serviceExe = Join-Path $repoRoot 'target\debug\focusguard-service.exe'
$probeExe = Join-Path $repoRoot 'target\debug\examples\scm_policy_probe.exe'
$runnerTemp = (Resolve-Path $env:RUNNER_TEMP).Path.TrimEnd('\')
$testRoot = Join-Path $runnerTemp ("focusguard-scm-recovery-" + [guid]::NewGuid().ToString('N'))
$databasePath = Join-Path $testRoot 'policy.db'
$hostsPath = Join-Path $testRoot 'hosts.fixture'
$diagnosticsPath = Join-Path $testRoot 'service-startup.log'
$realHostsPath = Join-Path $env:SystemRoot 'System32\drivers\etc\hosts'
$testDomain = 'focuser-scm-recovery.invalid'
$createdService = $false

function Invoke-Checked([string] $Executable, [string[]] $Arguments) {
    & $Executable @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Executable failed with exit code $LASTEXITCODE"
    }
}

function Get-ServiceInfo {
    Get-CimInstance Win32_Service -Filter "Name='$serviceName'" -ErrorAction SilentlyContinue
}

function Show-RecentServiceEvents {
    foreach ($logName in @('System', 'Application')) {
        try {
            $filter = @{
                LogName = $logName
                StartTime = [DateTime]::UtcNow.AddMinutes(-5)
            }
            if ($logName -eq 'System') {
                $filter.ProviderName = 'Service Control Manager'
            }
            else {
                $filter.Id = @(1000, 1001, 1026)
            }
            Get-WinEvent -FilterHashtable $filter -MaxEvents 20 -ErrorAction Stop |
                Select-Object TimeCreated, Id, ProviderName, Message |
                Format-List | Out-Host
        }
        catch { Write-Host "Could not read recent $logName service events: $_" }
    }
}

function Wait-ServiceRunning([uint32] $PreviousPid = 0) {
    $deadline = [DateTime]::UtcNow.AddSeconds(45)
    $lastCheckpoint = $null
    $reportedRecoveryWait = $false
    do {
        $service = Get-ServiceInfo
        if ($null -ne $service -and $service.State -eq 'Start Pending' -and
            $service.CheckPoint -ne $lastCheckpoint) {
            $lastCheckpoint = $service.CheckPoint
            Write-Host "SCM startup checkpoint $lastCheckpoint (wait hint: $($service.WaitHint) ms)."
        }
        if ($null -ne $service -and $service.State -eq 'Stopped') {
            if ($PreviousPid -eq 0) {
                Write-Host "Service stopped during startup (Win32 exit $($service.ExitCode), service exit $($service.ServiceSpecificExitCode))."
                if (Test-Path $diagnosticsPath) {
                    Get-Content -LiteralPath $diagnosticsPath | Out-Host
                }
                & sc.exe queryex $serviceName | Out-Host
                Show-RecentServiceEvents
                throw "$serviceName stopped before reaching Running."
            }

            if (-not $reportedRecoveryWait) {
                Write-Host "Service is stopped after the forced crash; waiting for SCM's configured recovery action."
                $reportedRecoveryWait = $true
            }
        }
        if ($null -ne $service -and $service.State -eq 'Running' -and
            $service.ProcessId -gt 0 -and
            ($PreviousPid -eq 0 -or $service.ProcessId -ne $PreviousPid)) {
            return $service
        }
        Start-Sleep -Milliseconds 500
    } while ([DateTime]::UtcNow -lt $deadline)

    $service = Get-ServiceInfo
    if ($null -ne $service) {
        Write-Host "Last SCM status: state=$($service.State), checkpoint=$($service.CheckPoint), waitHint=$($service.WaitHint), PID=$($service.ProcessId)."
        & sc.exe queryex $serviceName | Out-Host
    }
    if (Test-Path $databasePath) {
        Get-Item -LiteralPath $databasePath | Select-Object FullName, Length, LastWriteTimeUtc | Format-List | Out-Host
    }
    if (Test-Path $hostsPath) {
        Write-Host 'Temporary hosts fixture at startup timeout:'
        Get-Content -LiteralPath $hostsPath | Out-Host
    }
    Show-RecentServiceEvents
    throw "SCM did not start/restart $serviceName within 45 seconds."
}

function Wait-ServiceStopped {
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    do {
        $service = Get-ServiceInfo
        if ($null -eq $service -or $service.State -eq 'Stopped') { return }
        Start-Sleep -Milliseconds 500
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "$serviceName did not stop within 20 seconds."
}

function Wait-HostsRule {
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    do {
        if ((Test-Path $hostsPath) -and
            (Get-Content -Raw $hostsPath).Contains("127.0.0.1 $testDomain")) {
            return
        }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "The temporary hosts fixture did not contain the expected rule for $testDomain."
}

function Invoke-Probe([string] $Stage) {
    $env:FOCUSGUARD_SCM_RUNTIME_TEST = '1'
    try {
        Invoke-Checked $probeExe @($Stage)
    }
    finally {
        Remove-Item Env:\FOCUSGUARD_SCM_RUNTIME_TEST -ErrorAction SilentlyContinue
    }
}

try {
    if (-not (Test-Path $serviceExe) -or -not (Test-Path $probeExe)) {
        throw 'Build focusguard-service.exe and scm_policy_probe.exe before running this script.'
    }
    if ($null -ne (Get-ServiceInfo)) {
        throw "A service named $serviceName already exists; refusing to modify it."
    }

    $fullTestRoot = [IO.Path]::GetFullPath($testRoot)
    $fullRunnerTemp = [IO.Path]::GetFullPath($runnerTemp).TrimEnd('\') + '\'
    if (-not $fullTestRoot.StartsWith($fullRunnerTemp, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Temporary test directory is outside RUNNER_TEMP; refusing to continue.'
    }
    if ([IO.Path]::GetFullPath($hostsPath) -ieq [IO.Path]::GetFullPath($realHostsPath)) {
        throw 'Refusing to use the real Windows hosts file.'
    }

    New-Item -ItemType Directory -Path $testRoot | Out-Null
    Invoke-Checked 'icacls.exe' @($testRoot, '/grant', '*S-1-5-18:(OI)(CI)F', '/T', '/C')
    Set-Content -Path $hostsPath -Value "# FocusGuard CI hosts fixture`r`n" -NoNewline -Encoding ascii

    $ownerSid = $identity.User.Value
    $imagePath = '"{0}" --owner-sid {1} --database "{2}" --hosts-file "{3}" --diagnostics-file "{4}"' -f `
        $serviceExe, $ownerSid, $databasePath, $hostsPath, $diagnosticsPath
    Invoke-Checked 'sc.exe' @('create', $serviceName, 'binPath=', $imagePath, 'start=', 'demand', 'obj=', 'LocalSystem')
    $createdService = $true
    Invoke-Checked 'sc.exe' @('sidtype', $serviceName, 'unrestricted')
    Invoke-Checked 'sc.exe' @('failure', $serviceName, 'reset=', '0', 'actions=', 'restart/5000/restart/5000/restart/5000')
    Invoke-Checked 'sc.exe' @('start', $serviceName)

    $initial = Wait-ServiceRunning
    if ($initial.StartName -ne 'LocalSystem') {
        throw "Service account is '$($initial.StartName)', expected LocalSystem."
    }
    Write-Host "SCM started LocalSystem service PID $($initial.ProcessId) with isolated paths."

    Invoke-Probe 'seed'
    Wait-HostsRule
    $beforeCrash = Get-ServiceInfo
    if ($null -eq $beforeCrash -or $beforeCrash.State -ne 'Running') {
        throw 'Service stopped before the forced-crash step.'
    }

    Stop-Process -Id $beforeCrash.ProcessId -Force
    Wait-ServiceStopped
    Set-Content -Path $hostsPath -Value "# Reset before SCM recovery verification`r`n" -NoNewline -Encoding ascii
    if ((Get-Content -Raw $hostsPath).Contains($testDomain)) {
        throw 'Could not clear the temporary fixture before SCM recovery.'
    }
    $recovered = Wait-ServiceRunning -PreviousPid $beforeCrash.ProcessId
    Write-Host "SCM restarted the service as PID $($recovered.ProcessId) after forced termination."

    Wait-HostsRule
    Invoke-Probe 'verify'
    Write-Host 'Recovery test passed: authenticated IPC sees the durable rule and SCM restart restored it to the temporary hosts file.'
}
finally {
    $cleanupFailures = @()
    $deleteRequested = $false
    if ($createdService) {
        try {
            $service = Get-ServiceInfo
            if ($null -ne $service -and $service.State -ne 'Stopped') {
                & sc.exe stop $serviceName | Out-Host
                try {
                    Wait-ServiceStopped
                }
                catch {
                    # Mark it for deletion before the fallback termination so
                    # crash recovery cannot respawn the test while files are removed.
                    & sc.exe delete $serviceName | Out-Host
                    $deleteRequested = $LASTEXITCODE -eq 0
                    $service = Get-ServiceInfo
                    if ($null -ne $service -and $service.ProcessId -gt 0) {
                        Stop-Process -Id $service.ProcessId -Force -ErrorAction SilentlyContinue
                    }
                    try { Wait-ServiceStopped } catch { $cleanupFailures += $_ }
                }
            }
            if ($null -ne (Get-ServiceInfo) -and -not $deleteRequested) {
                & sc.exe delete $serviceName | Out-Host
                if ($LASTEXITCODE -ne 0) { throw "Could not delete temporary service $serviceName." }
                $deleteRequested = $true
            }

            if ($deleteRequested) {
                $deleteDeadline = [DateTime]::UtcNow.AddSeconds(15)
                while ($null -ne (Get-ServiceInfo) -and [DateTime]::UtcNow -lt $deleteDeadline) {
                    Start-Sleep -Milliseconds 250
                }
                if ($null -ne (Get-ServiceInfo)) { throw "Temporary service $serviceName remains registered." }
            }
        }
        catch {
            $cleanupFailures += "Service cleanup failed: $_"
        }
    }

    try {
        if (Test-Path $testRoot) {
            $fullTestRoot = [IO.Path]::GetFullPath($testRoot)
            $fullRunnerTemp = [IO.Path]::GetFullPath($runnerTemp).TrimEnd('\') + '\'
            if (-not $fullTestRoot.StartsWith($fullRunnerTemp, [StringComparison]::OrdinalIgnoreCase)) {
                throw 'Refusing to recursively remove a path outside RUNNER_TEMP.'
            }
            Remove-Item -LiteralPath $fullTestRoot -Recurse -Force
        }
    }
    catch { $cleanupFailures += "Temporary files cleanup failed: $_" }

    if ($cleanupFailures.Count -gt 0) { throw ($cleanupFailures -join [Environment]::NewLine) }
}
