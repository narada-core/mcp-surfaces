[CmdletBinding()]
param(
  [switch]$NoNotification,
  [switch]$PauseOnError,
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

function Write-ConsoleStatus {
  param(
    [Parameter(Mandatory)][string]$Message,
    [ConsoleColor]$Color = [ConsoleColor]::Gray
  )

  try {
    Write-Host ("[{0}] {1}" -f (Get-Date -Format 'HH:mm:ss'), $Message) -ForegroundColor $Color
  } catch {
    # Console output is diagnostic only; logging remains authoritative.
  }
}

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
  $stepToken = [guid]::NewGuid().ToString('N')
  $stepOutputPath = Join-Path ([System.IO.Path]::GetTempPath()) "narada-materialize-$PID-$stepToken.stdout.log"
  $stepErrorPath = Join-Path ([System.IO.Path]::GetTempPath()) "narada-materialize-$PID-$stepToken.stderr.log"
  $stepExitPath = Join-Path ([System.IO.Path]::GetTempPath()) "narada-materialize-$PID-$stepToken.exit.txt"
  $spinner = @('|', '/', '-', '\')
  $spinnerIndex = 0

  Write-ConsoleStatus "${Label}: starting (output is captured in $logPath)" ([ConsoleColor]::Cyan)
  Write-Log "Starting $($Label): $($pnpmCommand.Display) (pnpm $($pnpmCommand.Version)) $($Arguments -join ' ')"
  try {
    $startProcessParameters = @{
      FilePath = $pnpmCommand.Command
      ArgumentList = $invocationArguments
      WorkingDirectory = $repoRoot
      RedirectStandardOutput = $stepOutputPath
      RedirectStandardError = $stepErrorPath
      NoNewWindow = $true
      PassThru = $true
    }
    if ([System.IO.Path]::GetExtension($pnpmCommand.Command) -in @('.cmd', '.bat')) {
      $quotedArguments = @($invocationArguments | ForEach-Object {
        '"' + ([string]$_).Replace('"', '""') + '"'
      })
      $commandLine = '""' + $pnpmCommand.Command.Replace('"', '""') + '" ' +
        ($quotedArguments -join ' ') +
        ' & set "_narada_exit=!errorlevel!" & >"' + $stepExitPath.Replace('"', '""') +
        '" echo !_narada_exit! & exit /b !_narada_exit!"'
      $startProcessParameters.FilePath = $env:ComSpec
      $startProcessParameters.ArgumentList = @('/d', '/v:on', '/s', '/c', $commandLine)
    }
    $process = Start-Process @startProcessParameters

    while (-not $process.HasExited) {
      $frame = $spinner[$spinnerIndex % $spinner.Count]
      Write-Host ("`r[{0}] {1} {2} running..." -f (Get-Date -Format 'HH:mm:ss'), $frame, $Label) -NoNewline
      $spinnerIndex++
      Start-Sleep -Milliseconds 250
    }
    $process.WaitForExit()
    $process.Refresh()
    Write-Host ("`r[{0}]   {1} complete.           " -f (Get-Date -Format 'HH:mm:ss'), $Label)
    $exitCode = $process.ExitCode
    if (Test-Path -LiteralPath $stepExitPath -PathType Leaf) {
      $recordedExitCode = (Get-Content -LiteralPath $stepExitPath -Raw).Trim()
      if ($recordedExitCode -match '^-?\d+$') {
        $exitCode = [int]$recordedExitCode
      }
    }

    foreach ($outputPath in @($stepOutputPath, $stepErrorPath)) {
      if (Test-Path -LiteralPath $outputPath -PathType Leaf) {
        Get-Content -LiteralPath $outputPath | Add-Content -LiteralPath $logPath -Encoding utf8
      }
    }
  } finally {
    Remove-Item -LiteralPath $stepOutputPath, $stepErrorPath, $stepExitPath -Force -ErrorAction SilentlyContinue
  }
  if ($null -eq $exitCode) {
    throw "${Label} did not expose an exit code (materialization_step_exit_code_unavailable)."
  }
  if ($exitCode -ne 0) {
    throw "${Label} failed with exit code $exitCode."
  }
  Write-Log "Completed ${Label}."
}

function Read-RequiredPnpmVersion {
  param([Parameter(Mandatory)][string]$PackagePath)

  $package = Get-Content -LiteralPath $PackagePath -Raw | ConvertFrom-Json
  $packageManager = [string]$package.packageManager
  if ($packageManager -notmatch '^pnpm@(?<version>\d+\.\d+\.\d+)(?:$|\+)') {
    throw "package.json must declare an exact pnpm packageManager pin; received '$packageManager'."
  }
  return $Matches.version
}

function Get-PnpmVersion {
  param(
    [Parameter(Mandatory)][string]$Command,
    [string[]]$PrefixArguments = @()
  )

  try {
    $output = @(& $Command @PrefixArguments '--version' 2>&1)
    $exitCode = $LASTEXITCODE
  } catch {
    return $null
  }
  if ($exitCode -ne 0) {
    return $null
  }
  return @($output |
      ForEach-Object { ([string]$_).Trim() } |
      Where-Object { $_ -match '^\d+\.\d+\.\d+(?:[-+].*)?$' } |
      Select-Object -Last 1)[0]
}

function Resolve-PnpmCommand {
  param([Parameter(Mandatory)][string]$RequiredVersion)

  $candidates = @()
  $mismatches = @()

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
      $prefixArguments = @()
      $version = Get-PnpmVersion -Command $candidate -PrefixArguments $prefixArguments
      if ($version -eq $RequiredVersion) {
        return [pscustomobject]@{
          Command = $candidate
          PrefixArguments = $prefixArguments
          Display = $candidate
          Version = $version
        }
      }
      if ($version) { $mismatches += "$candidate ($version)" }
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
      $prefixArguments = @('pnpm')
      $version = Get-PnpmVersion -Command $candidate -PrefixArguments $prefixArguments
      if ($version -eq $RequiredVersion) {
        return [pscustomobject]@{
          Command = $candidate
          PrefixArguments = $prefixArguments
          Display = "$candidate pnpm"
          Version = $version
        }
      }
      if ($version) { $mismatches += "$candidate pnpm ($version)" }
    }
  }

  if ($mismatches.Count -gt 0) {
    $sample = ($mismatches | Select-Object -Unique | Select-Object -First 8) -join '; '
    throw "Repository requires pnpm@$RequiredVersion, but no matching pnpm executable was found. Available versions: $sample"
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
      $notifyIcon.BalloonTipText = 'All carriers materialized. Restart Codex, Kimi Code, and OpenCode to load the refreshed MCP configuration.'
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
  Write-ConsoleStatus 'Narada all-carrier materialization starting.' ([ConsoleColor]::Cyan)
  Write-ConsoleStatus "Workspace: $repoRoot"
  $requiredPnpmVersion = Read-RequiredPnpmVersion -PackagePath (Join-Path $repoRoot 'package.json')
  $logDirectory = Split-Path -Parent $logPath
  if ($logDirectory -and -not (Test-Path -LiteralPath $logDirectory)) {
    New-Item -ItemType Directory -Path $logDirectory -Force | Out-Null
  }
  [System.IO.File]::WriteAllText(
    $logPath,
    "[$(Get-Date -Format o)] Starting workspace build and all-carrier materialization.$([Environment]::NewLine)",
    (New-Object System.Text.UTF8Encoding($false))
  )
  $pnpmCommand = Resolve-PnpmCommand -RequiredVersion $requiredPnpmVersion
  Write-ConsoleStatus "Using $($pnpmCommand.Display) (pnpm $($pnpmCommand.Version))."
  Write-ConsoleStatus "Detailed output: $logPath"
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
  Write-ConsoleStatus 'All carriers materialized successfully. Restart Codex, Kimi Code, and OpenCode.' ([ConsoleColor]::Green)
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
    Write-ConsoleStatus "Materialization failed. See $logPath" ([ConsoleColor]::Red)
  } catch {
    # The process exit code remains the failure signal.
  }
  if ($PauseOnError) {
    try {
      Write-ConsoleStatus 'Press Enter to close this window.' ([ConsoleColor]::Yellow)
      Read-Host | Out-Null
    } catch {
      # The process exit code remains the failure signal.
    }
  }
  exit 1
} finally {
  if ($pushedLocation) {
    Pop-Location
  }
}
