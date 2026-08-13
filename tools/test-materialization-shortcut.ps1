[CmdletBinding()]
param(
  [switch]$FullE2E
)

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..') -ErrorAction Stop).Path
$installerPath = Join-Path $repoRoot 'tools\install-materialization-shortcut.ps1'
$materializePath = Join-Path $repoRoot 'tools\materialize-all-carriers.ps1'
$contractPath = Join-Path $repoRoot 'tools\materialization-shortcut.contract.json'
$stablePowerShell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'

function Assert-Condition {
  param(
    [Parameter(Mandatory)][bool]$Condition,
    [Parameter(Mandatory)][string]$Message
  )

  if (-not $Condition) {
    throw "assertion_failed: $Message"
  }
}

function Invoke-ChildPowerShell {
  param([Parameter(Mandatory)][string[]]$Arguments)

  $previousErrorActionPreference = $ErrorActionPreference
  try {
    $ErrorActionPreference = 'Continue'
    $output = & $stablePowerShell @Arguments 2>&1
    $exitCode = $LASTEXITCODE
  } finally {
    $ErrorActionPreference = $previousErrorActionPreference
  }
  return [pscustomobject]@{
    ExitCode = $exitCode
    Output = @($output)
    Text = ($output | Out-String).Trim()
  }
}

function Invoke-Installer {
  param([Parameter(Mandatory)][string[]]$Arguments)

  $childArguments = @(
    '-NoLogo',
    '-NoProfile',
    '-NonInteractive',
    '-ExecutionPolicy',
    'Bypass',
    '-File',
    $installerPath
  ) + $Arguments
  $result = Invoke-ChildPowerShell -Arguments $childArguments

  $json = $null
  if (-not [string]::IsNullOrWhiteSpace($result.Text)) {
    try {
      $json = $result.Text | ConvertFrom-Json
    } catch {
      throw "installer_output_not_json: $($result.Text)"
    }
  }
  return [pscustomobject]@{
    ExitCode = $result.ExitCode
    Json = $json
    Text = $result.Text
  }
}

function Save-Environment {
  param([Parameter(Mandatory)][string[]]$Names)

  $backup = @{}
  foreach ($name in $Names) {
    $backup[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
  }
  return $backup
}

function Restore-Environment {
  param([Parameter(Mandatory)][hashtable]$Backup)

  foreach ($name in $Backup.Keys) {
    $value = $Backup[$name]
    if ($null -eq $value) {
      Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
    } else {
      Set-Item -LiteralPath "Env:$name" -Value $value
    }
  }
}

function Invoke-Materialization {
  param(
    [Parameter(Mandatory)][string]$LogPath,
    [Parameter(Mandatory)][string]$Mode,
    [Parameter(Mandatory)][string]$IsolationRoot
  )

  $environmentNames = @('Path', 'PNPM_HOME', 'FNM_MULTISHELL_PATH', 'LOCALAPPDATA', 'APPDATA', 'USERPROFILE', 'ProgramFiles')
  $backup = Save-Environment -Names $environmentNames
  try {
    $env:Path = 'C:\Windows\System32;C:\Windows'
    $env:FNM_MULTISHELL_PATH = $null
    if ($Mode -in @('failure', 'version-mismatch')) {
      $env:PNPM_HOME = Join-Path $IsolationRoot 'pnpm-home'
      $env:LOCALAPPDATA = Join-Path $IsolationRoot 'local-appdata'
      $env:APPDATA = Join-Path $IsolationRoot 'appdata'
      $env:USERPROFILE = Join-Path $IsolationRoot 'user-profile'
      $env:ProgramFiles = Join-Path $IsolationRoot 'program-files'
      foreach ($directory in @($env:PNPM_HOME, $env:LOCALAPPDATA, $env:APPDATA, $env:USERPROFILE, $env:ProgramFiles)) {
      New-Item -ItemType Directory -Path $directory -Force | Out-Null
      }
    }
    if ($Mode -eq 'version-mismatch') {
      [System.IO.File]::WriteAllText(
        (Join-Path $env:PNPM_HOME 'pnpm.cmd'),
        "@echo off`r`nif `"%1`"==`"--version`" echo 10.33.0`r`nexit /b 0`r`n",
        (New-Object System.Text.UTF8Encoding($false))
      )
    }

    return Invoke-ChildPowerShell -Arguments @(
      '-NoLogo',
      '-NoProfile',
      '-NonInteractive',
      '-ExecutionPolicy',
      'Bypass',
      '-File',
      $materializePath,
      '-NoNotification',
      '-LogPath',
      $LogPath
    )
  } finally {
    Restore-Environment -Backup $backup
  }
}

function Invoke-Shortcut {
  param(
    [Parameter(Mandatory)][string]$ShortcutPath,
    [Parameter(Mandatory)][string]$TempRoot
  )

  $environmentNames = @('Path', 'TEMP', 'TMP')
  $backup = Save-Environment -Names $environmentNames
  try {
    $env:Path = 'C:\Windows\System32;C:\Windows'
    $env:TEMP = $TempRoot
    $env:TMP = $TempRoot
    $shell = New-Object -ComObject WScript.Shell
    $exitCode = $shell.Run(('"' + $ShortcutPath + '"'), 0, $true)
    $logPath = Join-Path $TempRoot 'narada-materialize-all.log'
    return [pscustomobject]@{
      ExitCode = $exitCode
      LogPath = $logPath
    }
  } finally {
    Restore-Environment -Backup $backup
  }
}

function Test-ShortcutContract {
  param([Parameter(Mandatory)][string]$TestRoot)

  $contract = Get-Content -LiteralPath $contractPath -Raw | ConvertFrom-Json
  Assert-Condition ($contract.schema -eq 'narada.materialization_shortcut_contract.v1') 'contract schema must be versioned'
  Assert-Condition (-not ([string]$contract.script_relative_path -match '^[A-Za-z]:')) 'contract script path must be portable'
  Assert-Condition (([string]$contract.arguments_template -match '-NoProfile') -and ([string]$contract.arguments_template -match '-File')) 'contract must enforce no-profile file execution'
  Assert-Condition ([string]$contract.arguments_template -match '-PauseOnError') 'shortcut must keep failures visible for operator diagnosis'
  Assert-Condition ([string]$contract.arguments_template -match '-File\s+"\{script_path\}"\s+-PauseOnError(?:\s|$)') 'script parameters must follow the -File script path so PowerShell passes them to the script'
  Assert-Condition (-not ([string]$contract.arguments_template -match '-WindowStyle\\s+Hidden')) 'desktop launcher must remain visibly informative'
  Assert-Condition ([int]$contract.window_style -eq 1) 'desktop launcher must use a normal visible window style'
  $materializeScriptText = Get-Content -LiteralPath $materializePath -Raw
  Assert-Condition ($materializeScriptText -match 'Restart Codex, Kimi Code, and OpenCode') 'success notification must name every materialized carrier'
  Assert-Condition ($materializeScriptText -match 'Write-Status') 'launcher must print visible status messages'
  Assert-Condition ($materializeScriptText -match 'Resolve-PnpmToolchain') 'launcher must resolve pnpm independently of inherited PATH'

  $first = Invoke-Installer -Arguments @('-RepoRoot', $repoRoot, '-DesktopPath', $TestRoot)
  Assert-Condition ($first.ExitCode -eq 0) "initial installer failed: $($first.Text)"
  Assert-Condition ($first.Json.status -eq 'repaired') 'initial installation must repair the missing shortcut'
  Assert-Condition (@($first.Json.drift) -contains 'shortcut_missing') 'initial installation must report missing shortcut drift'
  Assert-Condition (Test-Path -LiteralPath $first.Json.shortcut -PathType Leaf) 'installer must create the .lnk'
  Assert-Condition (Test-Path -LiteralPath $first.Json.sidecar -PathType Leaf) 'installer must create the state sidecar'

  $inSync = Invoke-Installer -Arguments @('-RepoRoot', $repoRoot, '-DesktopPath', $TestRoot, '-CheckOnly')
  Assert-Condition ($inSync.ExitCode -eq 0) "check-only healthy shortcut failed: $($inSync.Text)"
  Assert-Condition ($inSync.Json.status -eq 'in_sync') 'fresh shortcut must be in_sync'

  $shell = (New-Object -ComObject WScript.Shell).CreateShortcut($first.Json.shortcut)
  $shell.Arguments = '-corrupted-arguments'
  $shell.WorkingDirectory = Join-Path $TestRoot 'corrupted-working-directory'
  $shell.Save()

  $drifted = Invoke-Installer -Arguments @('-RepoRoot', $repoRoot, '-DesktopPath', $TestRoot, '-CheckOnly')
  Assert-Condition ($drifted.ExitCode -ne 0) 'check-only drift must fail'
  Assert-Condition ($drifted.Json.status -eq 'drifted') 'check-only drift status must be drifted'
  Assert-Condition (@($drifted.Json.drift) -contains 'arguments') 'check-only must identify argument drift'
  Assert-Condition (@($drifted.Json.drift) -contains 'working_directory') 'check-only must identify working-directory drift'

  $repaired = Invoke-Installer -Arguments @('-RepoRoot', $repoRoot, '-DesktopPath', $TestRoot)
  Assert-Condition ($repaired.ExitCode -eq 0) "drift repair failed: $($repaired.Text)"
  Assert-Condition ($repaired.Json.status -eq 'repaired') 'installer must report repaired drift'

  $final = Invoke-Installer -Arguments @('-RepoRoot', $repoRoot, '-DesktopPath', $TestRoot, '-CheckOnly')
  Assert-Condition ($final.ExitCode -eq 0) "post-repair check failed: $($final.Text)"
  Assert-Condition ($final.Json.status -eq 'in_sync') 'repaired shortcut must be in_sync'

  $state = Get-Content -LiteralPath $final.Json.sidecar -Raw | ConvertFrom-Json
  $state.script_sha256 = 'corrupted-script-hash'
  [System.IO.File]::WriteAllText(
    $final.Json.sidecar,
    ($state | ConvertTo-Json -Depth 8),
    (New-Object System.Text.UTF8Encoding($false))
  )
  $stateDrift = Invoke-Installer -Arguments @('-RepoRoot', $repoRoot, '-DesktopPath', $TestRoot, '-CheckOnly')
  Assert-Condition ($stateDrift.ExitCode -ne 0) 'script-hash drift must fail check-only'
  Assert-Condition (@($stateDrift.Json.drift) -contains 'script_hash') 'check-only must identify script hash drift'

  $stateRepair = Invoke-Installer -Arguments @('-RepoRoot', $repoRoot, '-DesktopPath', $TestRoot)
  Assert-Condition ($stateRepair.ExitCode -eq 0) "script-hash repair failed: $($stateRepair.Text)"
  Assert-Condition ($stateRepair.Json.status -eq 'repaired') 'installer must repair script hash drift'
}

function Test-FailureReporting {
  param([Parameter(Mandatory)][string]$TestRoot)

  $logPath = Join-Path $TestRoot 'materialization-failure.log'
  $result = Invoke-Materialization -LogPath $logPath -Mode 'failure' -IsolationRoot (Join-Path $TestRoot 'isolated-runtime')
  Assert-Condition ($result.ExitCode -ne 0) 'isolated missing-runtime run must fail'
  Assert-Condition (Test-Path -LiteralPath $logPath -PathType Leaf) 'failure run must produce a log'
  $log = Get-Content -LiteralPath $logPath -Raw
  Assert-Condition ($log -match 'ERROR:') 'failure log must include an ERROR record'
  Assert-Condition ($log -match 'pnpm toolchain unavailable') 'failure log must identify missing pnpm'

  [pscustomobject]@{
    mode = 'failure'
    exit_code = $result.ExitCode
    log_path = $logPath
    error_marker = $true
  }
}

function Test-PnpmVersionEnforcement {
  param([Parameter(Mandatory)][string]$TestRoot)

  $logPath = Join-Path $TestRoot 'materialization-version-mismatch.log'
  $result = Invoke-Materialization -LogPath $logPath -Mode 'version-mismatch' -IsolationRoot (Join-Path $TestRoot 'isolated-version-runtime')
  Assert-Condition ($result.ExitCode -ne 0) 'isolated mismatched-pnpm run must fail'
  Assert-Condition (Test-Path -LiteralPath $logPath -PathType Leaf) 'version-mismatch run must produce a log'
  $log = Get-Content -LiteralPath $logPath -Raw
  Assert-Condition ($log -match 'requires pnpm@10\.34\.0') 'version-mismatch log must identify the pinned pnpm version'
  Assert-Condition ($log -match '10\.33\.0') 'version-mismatch log must identify the discovered pnpm version'

  [pscustomobject]@{
    mode = 'version_mismatch'
    exit_code = $result.ExitCode
    log_path = $logPath
    error_marker = $true
  }
}

function Test-FullE2E {
  param([Parameter(Mandatory)][string]$TestRoot)

  $manifestPath = Join-Path $repoRoot '.ai\runtime\workspace-artifact-manifest.json'
  $generationPaths = @(
    [pscustomobject]@{ Carrier = 'codex-andrey'; Path = (Join-Path $env:USERPROFILE '.codex\config.toml.narada-generation.json') },
    [pscustomobject]@{ Carrier = 'kimi-andrey'; Path = (Join-Path $env:USERPROFILE '.kimi-code\mcp.json.narada-generation.json') },
    [pscustomobject]@{ Carrier = 'opencode-andrey'; Path = (Join-Path $env:USERPROFILE '.config\opencode\opencode.jsonc.narada-generation.json') }
  )
  $e2eDesktop = Join-Path $TestRoot 'e2e-desktop'
  $installation = Invoke-Installer -Arguments @('-RepoRoot', $repoRoot, '-DesktopPath', $e2eDesktop)
  Assert-Condition ($installation.ExitCode -eq 0) "E2E shortcut installation failed: $($installation.Text)"
  $startedAt = [DateTimeOffset]::UtcNow
  $result = Invoke-Shortcut -ShortcutPath $installation.Json.shortcut -TempRoot $TestRoot
  $logPath = $result.LogPath
  Assert-Condition ($result.ExitCode -eq 0) "full shortcut E2E failed with exit code $($result.ExitCode)"
  Assert-Condition (Test-Path -LiteralPath $logPath -PathType Leaf) 'shortcut E2E must produce a log'
  $log = Get-Content -LiteralPath $logPath -Raw
  Assert-Condition ($log -match 'Materialization completed successfully\.') 'success log must report completed materialization'

  Assert-Condition (Test-Path -LiteralPath $manifestPath -PathType Leaf) 'workspace artifact manifest must exist'
  $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
  Assert-Condition (-not [string]::IsNullOrWhiteSpace([string]$manifest.manifest_fingerprint)) 'manifest fingerprint must be present'
  Assert-Condition (@($manifest.artifacts).Count -gt 0) 'manifest must contain artifacts'
  $manifestMtime = (Get-Item -LiteralPath $manifestPath).LastWriteTimeUtc
  Assert-Condition ($manifestMtime -ge $startedAt.UtcDateTime.AddSeconds(-5)) 'manifest must be refreshed by this E2E run'

  $generations = @()
  foreach ($carrierGeneration in $generationPaths) {
    Assert-Condition (Test-Path -LiteralPath $carrierGeneration.Path -PathType Leaf) "$($carrierGeneration.Carrier) materialization generation sidecar must exist"
    $generation = Get-Content -LiteralPath $carrierGeneration.Path -Raw | ConvertFrom-Json
    Assert-Condition ([string]$generation.carrier_id -eq [string]$carrierGeneration.Carrier) "$($carrierGeneration.Carrier) sidecar must identify its carrier"
    Assert-Condition ([string]$generation.artifact_manifest_fingerprint -eq [string]$manifest.manifest_fingerprint) "$($carrierGeneration.Carrier) generation must reference the current manifest fingerprint"
    Assert-Condition (Test-Path -LiteralPath ([string]$generation.runtime_materialization_plan_path) -PathType Leaf) "$($carrierGeneration.Carrier) runtime materialization plan must exist"
    $generatedAt = [DateTimeOffset]::Parse([string]$generation.generated_at)
    Assert-Condition ($generatedAt -ge $startedAt.AddSeconds(-5)) "$($carrierGeneration.Carrier) generation timestamp must be fresh for this E2E run"
    $generations += [pscustomobject]@{
      carrier = $generation.carrier_id
      generation_fingerprint = $generation.generation_fingerprint
      generated_at = $generation.generated_at
      artifact_manifest_fingerprint = $generation.artifact_manifest_fingerprint
      proxy_implementation = $generation.proxy_implementation
    }
  }

  [pscustomobject]@{
    mode = 'full_e2e'
    exit_code = $result.ExitCode
    log_path = $logPath
    manifest_fingerprint = $manifest.manifest_fingerprint
    artifact_count = @($manifest.artifacts).Count
    carrier_generations = $generations
  }
}

$testRoot = Join-Path ([IO.Path]::GetTempPath()) ('narada-materialization-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $testRoot -Force | Out-Null

try {
  Test-ShortcutContract -TestRoot (Join-Path $testRoot 'desktop')
  $failure = Test-FailureReporting -TestRoot $testRoot
  $versionMismatch = Test-PnpmVersionEnforcement -TestRoot $testRoot
  $results = @($failure, $versionMismatch)

  if ($FullE2E) {
    $results += Test-FullE2E -TestRoot $testRoot
  }

  [pscustomobject]@{
    schema = 'narada.materialization_shortcut_test.v1'
    status = 'passed'
    full_e2e = [bool]$FullE2E
    results = $results
  } | ConvertTo-Json -Depth 10
  exit 0
} catch {
  [pscustomobject]@{
    schema = 'narada.materialization_shortcut_test.v1'
    status = 'failed'
    full_e2e = [bool]$FullE2E
    error = ($_ | Out-String).Trim()
  } | ConvertTo-Json -Depth 10
  exit 1
} finally {
  Remove-Item -LiteralPath $testRoot -Recurse -Force -ErrorAction SilentlyContinue
}
