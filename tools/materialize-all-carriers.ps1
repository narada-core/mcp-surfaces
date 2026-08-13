[CmdletBinding()]
param(
  [switch]$NoNotification,
  [switch]$PauseOnError,
  [string]$LogPath,
  [string]$ContractPath,
  [string]$MatrixPath,
  [string]$CarrierHome,
  [string]$InstalledIndexPath
)

$ErrorActionPreference = 'Stop'
$scriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path (Join-Path $scriptRoot '..') -ErrorAction Stop).Path
$logPath = if ([string]::IsNullOrWhiteSpace($LogPath)) {
  Join-Path ([System.IO.Path]::GetTempPath()) 'narada-materialize-all.log'
} else {
  [System.IO.Path]::GetFullPath($LogPath)
}

function Write-Status {
  param([Parameter(Mandatory)][string]$Message, [ConsoleColor]$Color = [ConsoleColor]::Gray)
  try { Write-Host ("[{0}] {1}" -f (Get-Date -Format 'HH:mm:ss'), $Message) -ForegroundColor $Color } catch {}
}

function Write-LogLine {
  param([Parameter(Mandatory)][string]$Message)
  [System.IO.File]::AppendAllText(
    $logPath,
    ("[{0:o}] {1}{2}" -f (Get-Date), $Message, [Environment]::NewLine),
    (New-Object System.Text.UTF8Encoding($false))
  )
}

function Resolve-Materializer {
  $artifactRoot = Join-Path $repoRoot 'packages\shared\mcp-materializer-native\dist\native'
  $pointerPath = Join-Path $artifactRoot 'current.json'
  if (-not (Test-Path -LiteralPath $pointerPath -PathType Leaf)) {
    throw "Native materializer pointer is missing: $pointerPath. Build and publish @narada-core/mcp-materializer-native first."
  }
  $pointer = Get-Content -LiteralPath $pointerPath -Raw | ConvertFrom-Json
  if ($pointer.schema -ne 'narada.mcp_materializer.native_artifact_pointer.v1') {
    throw "Native materializer pointer has an unsupported schema: $pointerPath"
  }
  $relative = [string]$pointer.artifacts.'narada-mcp-materializer.exe'
  if ($relative -notmatch '^versions/[0-9a-f]{64}/narada-mcp-materializer\.exe$') {
    throw "Native materializer pointer target is unsafe: $relative"
  }
  $candidate = [System.IO.Path]::GetFullPath((Join-Path $artifactRoot ($relative -replace '/', '\')))
  $versionsRoot = [System.IO.Path]::GetFullPath((Join-Path $artifactRoot 'versions')) + [System.IO.Path]::DirectorySeparatorChar
  if (-not $candidate.StartsWith($versionsRoot, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Native materializer pointer escapes the immutable artifact root: $candidate"
  }
  if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) {
    throw "Native materializer artifact is missing: $candidate"
  }
  return $candidate
}

function Invoke-NativeMaterializer {
  param([Parameter(Mandatory)][string]$Executable, [Parameter(Mandatory)][string[]]$Arguments)
  $stdoutPath = Join-Path ([System.IO.Path]::GetTempPath()) "narada-materialize-$PID.stdout.log"
  $stderrPath = Join-Path ([System.IO.Path]::GetTempPath()) "narada-materialize-$PID.stderr.log"
  try {
    $process = Start-Process -FilePath $Executable -ArgumentList $Arguments -WorkingDirectory $repoRoot `
      -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath -NoNewWindow -PassThru -Wait
    foreach ($path in @($stdoutPath, $stderrPath)) {
      if (Test-Path -LiteralPath $path -PathType Leaf) {
        Get-Content -LiteralPath $path -TotalCount 400 | Add-Content -LiteralPath $logPath -Encoding utf8
      }
    }
    if ($process.ExitCode -ne 0) {
      throw "Native materializer failed with exit code $($process.ExitCode)."
    }
  } finally {
    Remove-Item -LiteralPath $stdoutPath, $stderrPath -Force -ErrorAction SilentlyContinue
  }
}

function Resolve-PnpmToolchain {
  $pnpmCandidates = @()
  if (-not [string]::IsNullOrWhiteSpace($env:PNPM_HOME)) {
    $pnpmCandidates += Join-Path $env:PNPM_HOME 'pnpm.cmd'
  }
  if (-not [string]::IsNullOrWhiteSpace($env:FNM_MULTISHELL_PATH)) {
    $pnpmCandidates += Join-Path $env:FNM_MULTISHELL_PATH 'pnpm.cmd'
  }
  $pnpmCommand = Get-Command pnpm.cmd -ErrorAction SilentlyContinue | Select-Object -First 1
  if ($pnpmCommand) { $pnpmCandidates += $pnpmCommand.Source }
  foreach ($candidate in $pnpmCandidates | Select-Object -Unique) {
    if (Test-Path -LiteralPath $candidate -PathType Leaf) {
      return [pscustomobject]@{ Executable = [System.IO.Path]::GetFullPath($candidate); ArgumentPrefix = @() }
    }
  }

  $corepackCandidates = @()
  if (-not [string]::IsNullOrWhiteSpace($env:FNM_MULTISHELL_PATH)) {
    $corepackCandidates += Join-Path $env:FNM_MULTISHELL_PATH 'corepack.cmd'
  }
  $corepackCommand = Get-Command corepack.cmd -ErrorAction SilentlyContinue | Select-Object -First 1
  if ($corepackCommand) { $corepackCandidates += $corepackCommand.Source }
  if (-not [string]::IsNullOrWhiteSpace($env:APPDATA)) {
    $fnmVersions = Join-Path $env:APPDATA 'fnm\node-versions'
    if (Test-Path -LiteralPath $fnmVersions -PathType Container) {
      $corepackCandidates += Get-ChildItem -LiteralPath $fnmVersions -Directory |
        Sort-Object Name -Descending |
        ForEach-Object { Join-Path $_.FullName 'installation\corepack.cmd' }
    }
  }
  foreach ($candidate in $corepackCandidates | Select-Object -Unique) {
    if (Test-Path -LiteralPath $candidate -PathType Leaf) {
      return [pscustomobject]@{ Executable = [System.IO.Path]::GetFullPath($candidate); ArgumentPrefix = @('pnpm') }
    }
  }
  throw 'pnpm toolchain unavailable: install pnpm/Corepack or set PNPM_HOME/FNM_MULTISHELL_PATH.'
}

function Invoke-CarrierArtifactBuild {
  $stdoutPath = Join-Path ([System.IO.Path]::GetTempPath()) "narada-carrier-build-$PID.stdout.log"
  $stderrPath = Join-Path ([System.IO.Path]::GetTempPath()) "narada-carrier-build-$PID.stderr.log"
  try {
    $tool = Resolve-PnpmToolchain
    $packageManager = [string](Get-Content -LiteralPath (Join-Path $repoRoot 'package.json') -Raw | ConvertFrom-Json).packageManager
    if ($packageManager -notmatch '^pnpm@(?<version>\d+\.\d+\.\d+)$') {
      throw "package.json must declare an exact pnpm packageManager version; found '$packageManager'."
    }
    $requiredVersion = $Matches.version
    $versionArguments = @($tool.ArgumentPrefix) + @('--version')
    $actualVersion = [string](& $tool.Executable @versionArguments | Select-Object -First 1)
    if ($actualVersion.Trim() -ne $requiredVersion) {
      throw "Carrier artifact preparation requires pnpm@$requiredVersion; resolved '$($tool.Executable)' reported '$($actualVersion.Trim())'."
    }
    $arguments = @($tool.ArgumentPrefix) + @('run', 'build:carrier-artifacts')
    $process = Start-Process -FilePath $tool.Executable -ArgumentList $arguments -WorkingDirectory $repoRoot -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath -NoNewWindow -PassThru -Wait
    foreach ($path in @($stdoutPath, $stderrPath)) {
      if (Test-Path -LiteralPath $path -PathType Leaf) {
        Get-Content -LiteralPath $path -TotalCount 1000 | Add-Content -LiteralPath $logPath -Encoding utf8
      }
    }
    if ($process.ExitCode -ne 0) {
      throw "Carrier artifact preparation failed with exit code $($process.ExitCode)."
    }
  } finally {
    Remove-Item -LiteralPath $stdoutPath, $stderrPath -Force -ErrorAction SilentlyContinue
  }
}

function Show-SuccessNotification {
  try {
    Add-Type -AssemblyName System.Drawing
    Add-Type -AssemblyName System.Windows.Forms
    $notification = New-Object System.Windows.Forms.NotifyIcon
    try {
      $notification.Icon = [System.Drawing.SystemIcons]::Information
      $notification.BalloonTipTitle = 'Narada MCP'
      $notification.BalloonTipText = 'All carriers materialized. Restart Codex, Kimi Code, and OpenCode to load the refreshed configuration.'
      $notification.Visible = $true
      $notification.ShowBalloonTip(5000)
      Start-Sleep -Seconds 5
    } finally { $notification.Dispose() }
  } catch { Write-LogLine "WARNING: success notification unavailable: $($_.Exception.Message)" }
}

try {
  if ([string]::IsNullOrWhiteSpace($env:USERPROFILE)) { throw 'USERPROFILE is required.' }
  $carrierRoot = if ($CarrierHome) { [System.IO.Path]::GetFullPath($CarrierHome) } else { $env:USERPROFILE }
  $contract = if ($ContractPath) { [System.IO.Path]::GetFullPath($ContractPath) } else {
    Join-Path $env:USERPROFILE 'Narada\.narada\capabilities\carrier-materialization.json'
  }
  $matrix = if ($MatrixPath) { [System.IO.Path]::GetFullPath($MatrixPath) } else {
    Join-Path (Split-Path -Parent $repoRoot) 'narada\packages\operator-surface-runtime-contract\contracts\runtime-implementation-matrix.json'
  }
  $installedIndex = if ($InstalledIndexPath) { [System.IO.Path]::GetFullPath($InstalledIndexPath) } else {
    Join-Path $carrierRoot '.narada\carriers\installed-carriers.json'
  }
  $logDirectory = Split-Path -Parent $logPath
  if ($logDirectory -and -not (Test-Path -LiteralPath $logDirectory)) {
    New-Item -ItemType Directory -Path $logDirectory -Force | Out-Null
  }
  [System.IO.File]::WriteAllText($logPath, '', (New-Object System.Text.UTF8Encoding($false)))
  $operationLockPath = Join-Path $repoRoot '.ai\runtime\carrier-promotion.lock'
  $operationLockDirectory = Split-Path -Parent $operationLockPath
  if (-not (Test-Path -LiteralPath $operationLockDirectory)) {
    New-Item -ItemType Directory -Path $operationLockDirectory -Force | Out-Null
  }
  try {
    $script:operationLock = [System.IO.File]::Open(
      $operationLockPath,
      [System.IO.FileMode]::OpenOrCreate,
      [System.IO.FileAccess]::ReadWrite,
      [System.IO.FileShare]::None
    )
  } catch {
    throw "Another carrier promotion operation holds the workspace lock: $operationLockPath"
  }
  Write-Status 'Preparing and sealing the native carrier artifact graph.' ([ConsoleColor]::Cyan)
  Invoke-CarrierArtifactBuild
  $materializer = Resolve-Materializer
  Write-Status 'Narada native bundle promotion starting.' ([ConsoleColor]::Cyan)
  Write-Status "Authority: $materializer"
  Write-Status "Detailed output: $logPath"
  Write-LogLine "Authority: $materializer"
  Invoke-NativeMaterializer -Executable $materializer -Arguments @(
    'promote-site',
    '--contract', $contract,
    '--workspace-root', $repoRoot,
    '--home', $carrierRoot,
    '--matrix', $matrix,
    '--installed-index', $installedIndex
  )
  Write-Status 'All carriers materialized by the native authority. Restart Codex, Kimi Code, and OpenCode.' ([ConsoleColor]::Green)
  if (-not $NoNotification) { Show-SuccessNotification }
  exit 0
} catch {
  try { Write-LogLine "ERROR: $($_.Exception.Message)" } catch {}
  try {
    [Console]::Error.WriteLine("Narada materialization failed. See $logPath")
    Write-Status "Materialization failed. See $logPath" ([ConsoleColor]::Red)
  } catch {}
  if ($PauseOnError) {
    try { Write-Status 'Press Enter to close this window.' ([ConsoleColor]::Yellow); Read-Host | Out-Null } catch {}
  }
  exit 1
}
