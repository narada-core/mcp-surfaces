[CmdletBinding()]
param(
  [switch]$NoNotification,
  [string]$LogPath
)

$ErrorActionPreference = 'Stop'
$repoRoot = $null
$scriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$logPath = if ([string]::IsNullOrWhiteSpace($LogPath)) {
  Join-Path ([System.IO.Path]::GetTempPath()) 'narada-materialize-all.log'
} else {
  [System.IO.Path]::GetFullPath($LogPath)
}
$pushedLocation = $false

function Write-Log {
  param([Parameter(Mandatory)][string]$Message)

  $line = ("[{0:o}] {1}{2}" -f (Get-Date), $Message, [Environment]::NewLine)
  [System.IO.File]::AppendAllText(
    $logPath,
    $line,
    (New-Object System.Text.UTF8Encoding($false))
  )
}

function Invoke-PnpmStep {
  param(
    [Parameter(Mandatory)][string]$Label,
    [Parameter(Mandatory)][string[]]$Arguments
  )

  $invocationArguments = @($pnpmCommand.PrefixArguments) + @($Arguments)
  Write-Log "Starting $($Label): $($pnpmCommand.Display) $($Arguments -join ' ')"
  $previousErrorActionPreference = $ErrorActionPreference
  try {
    $ErrorActionPreference = 'Continue'
    & $pnpmCommand.Command @invocationArguments 2>&1 |
      Out-File -LiteralPath $logPath -Append -Encoding utf8
    $exitCode = $LASTEXITCODE
  } finally {
    $ErrorActionPreference = $previousErrorActionPreference
  }
  if ($exitCode -ne 0) {
    throw "${Label} failed with exit code $exitCode."
  }
  Write-Log "Completed ${Label}."
}

function Resolve-PnpmCommand {
  $candidates = @()

  foreach ($candidate in @('pnpm.cmd', 'pnpm.exe', 'pnpm')) {
    try {
      $resolved = Get-Command $candidate -CommandType Application -ErrorAction Stop
      if ($resolved -and $resolved.Source) {
        $candidates += $resolved.Source
      }
    } catch {
      # Try the next supported pnpm launcher.
    }
  }

  if ($env:PNPM_HOME) {
    $candidates += Join-Path $env:PNPM_HOME 'pnpm.cmd'
  }
  if ($env:APPDATA) {
    $candidates += Join-Path $env:APPDATA 'npm\pnpm.cmd'
  }
  if ($env:LOCALAPPDATA) {
    $candidates += Join-Path $env:LOCALAPPDATA 'pnpm\pnpm.cmd'
  }
  if ($env:ProgramFiles) {
    $candidates += Join-Path $env:ProgramFiles 'nodejs\pnpm.cmd'
  }

  $fnmMultishellRoot = if ($env:LOCALAPPDATA) {
    Join-Path $env:LOCALAPPDATA 'fnm_multishells'
  }
  if ($fnmMultishellRoot -and (Test-Path -LiteralPath $fnmMultishellRoot -PathType Container)) {
    $candidates += @(
      Get-ChildItem -LiteralPath $fnmMultishellRoot -Directory -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending |
        ForEach-Object { Join-Path $_.FullName 'pnpm.CMD' }
    )
  }

  foreach ($candidate in $candidates) {
    if (-not [string]::IsNullOrWhiteSpace($candidate) -and (Test-Path -LiteralPath $candidate -PathType Leaf)) {
      return [pscustomobject]@{
        Command = $candidate
        PrefixArguments = @()
        Display = $candidate
      }
    }
  }

  $corepackCandidates = @()
  if ($env:ProgramFiles) {
    $corepackCandidates += Join-Path $env:ProgramFiles 'nodejs\corepack.cmd'
  }
  if ($fnmMultishellRoot -and (Test-Path -LiteralPath $fnmMultishellRoot -PathType Container)) {
    $corepackCandidates += @(
      Get-ChildItem -LiteralPath $fnmMultishellRoot -Directory -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending |
        ForEach-Object { Join-Path $_.FullName 'corepack.cmd' }
    )
  }

  foreach ($candidate in $corepackCandidates) {
    if (-not [string]::IsNullOrWhiteSpace($candidate) -and (Test-Path -LiteralPath $candidate -PathType Leaf)) {
      return [pscustomobject]@{
        Command = $candidate
        PrefixArguments = @('pnpm')
        Display = "$candidate pnpm"
      }
    }
  }

  throw 'pnpm was not found. Install pnpm or Corepack, or expose the installation through PNPM_HOME.'
}

function Add-PathEntry {
  param([Parameter(Mandatory)][string]$Directory)

  if (-not (Test-Path -LiteralPath $Directory -PathType Container)) {
    return
  }

  $entries = @($env:Path -split [IO.Path]::PathSeparator | Where-Object { $_ })
  if ($entries -notcontains $Directory) {
    $env:Path = (@($Directory) + $entries) -join [IO.Path]::PathSeparator
  }
}

function Show-SuccessNotification {
  try {
    Add-Type -AssemblyName System.Drawing
    Add-Type -AssemblyName System.Windows.Forms

    $notifyIcon = New-Object System.Windows.Forms.NotifyIcon
    try {
      $notifyIcon.Icon = [System.Drawing.SystemIcons]::Information
      $notifyIcon.BalloonTipTitle = 'Narada MCP'
      $notifyIcon.BalloonTipText = 'All carriers materialized. Restart Codex to load the refreshed MCP configuration.'
      $notifyIcon.BalloonTipIcon = [System.Windows.Forms.ToolTipIcon]::Info
      $notifyIcon.Visible = $true
      $notifyIcon.ShowBalloonTip(10000)
      Start-Sleep -Seconds 10
    } finally {
      $notifyIcon.Dispose()
    }
  } catch {
    Write-Log "WARNING: success notification unavailable: $($_ | Out-String)"
  }
}

try {
  $repoRoot = (Resolve-Path (Join-Path $scriptRoot '..') -ErrorAction Stop).Path
  $logDirectory = Split-Path -Parent $logPath
  if ($logDirectory -and -not (Test-Path -LiteralPath $logDirectory)) {
    New-Item -ItemType Directory -Path $logDirectory -Force | Out-Null
  }
  [System.IO.File]::WriteAllText(
    $logPath,
    "[$(Get-Date -Format o)] Starting workspace build and all-carrier materialization.$([Environment]::NewLine)",
    (New-Object System.Text.UTF8Encoding($false))
  )
  $pnpmCommand = Resolve-PnpmCommand
  Add-PathEntry -Directory (Split-Path -Parent $pnpmCommand.Command)
  if ($env:BUN_INSTALL) {
    Add-PathEntry -Directory (Join-Path $env:BUN_INSTALL 'bin')
  }
  if ($env:USERPROFILE) {
    Add-PathEntry -Directory (Join-Path $env:USERPROFILE '.bun\bin')
    Add-PathEntry -Directory (Join-Path $env:USERPROFILE '.cargo\bin')
  }
  Push-Location $repoRoot
  $pushedLocation = $true

  Invoke-PnpmStep -Label 'workspace build' -Arguments @('run', 'build')
  Invoke-PnpmStep -Label 'all-carrier materialization' -Arguments @('run', 'materialize:carrier', '--', '--materialize-all')
  Write-Log 'Materialization completed successfully.'
  if (-not $NoNotification) {
    Show-SuccessNotification
  }
  exit 0
} catch {
  $details = ($_ | Out-String).Trim()
  try {
    Write-Log "ERROR: $details"
  } catch {
    # Preserve the original failure when the log cannot be written.
  }
  try {
    [Console]::Error.WriteLine("Narada materialization failed. See $logPath")
  } catch {
    # The process exit code remains the failure signal.
  }
  exit 1
} finally {
  if ($pushedLocation) {
    Pop-Location
  }
}
