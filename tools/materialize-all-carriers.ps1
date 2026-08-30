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

function Invoke-CarrierMaterialization {
  param([Parameter(Mandatory)][string[]]$Arguments)
  $stdoutPath = Join-Path ([System.IO.Path]::GetTempPath()) "narada-carrier-build-$PID.stdout.log"
  $stderrPath = Join-Path ([System.IO.Path]::GetTempPath()) "narada-carrier-build-$PID.stderr.log"
  try {
    $cargo = Get-Command cargo.exe -ErrorAction Stop | Select-Object -First 1
    $process = Start-Process -FilePath $cargo.Source -ArgumentList (@('native-materialize', '--') + $Arguments) -WorkingDirectory $repoRoot -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath -NoNewWindow -PassThru -Wait
    foreach ($path in @($stdoutPath, $stderrPath)) {
      if (Test-Path -LiteralPath $path -PathType Leaf) {
        Get-Content -LiteralPath $path -TotalCount 1000 | Add-Content -LiteralPath $logPath -Encoding utf8
      }
    }
    if ($process.ExitCode -ne 0) {
      throw "Cargo-native carrier materialization failed with exit code $($process.ExitCode)."
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
      $notification.BalloonTipText = 'All carriers materialized. Restart Codex, Kimi Code, OpenCode, and Pi to load the refreshed configuration.'
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
  Write-Status 'Preparing, synchronizing, sealing, and materializing the native carrier graph.' ([ConsoleColor]::Cyan)
  Write-Status "Detailed output: $logPath"
  Write-LogLine 'Authority: cargo native-materialize'
  Invoke-CarrierMaterialization -Arguments @(
    '--contract', $contract,
    '--home', $carrierRoot,
    '--matrix', $matrix,
    '--installed-index', $installedIndex
  )
  Write-LogLine 'Materialization completed successfully.'
  Write-Status 'All carriers materialized by the native authority. Restart Codex, Kimi Code, OpenCode, and Pi.' ([ConsoleColor]::Green)
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
