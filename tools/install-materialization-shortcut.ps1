[CmdletBinding()]
param(
  [string]$RepoRoot,
  [string]$DesktopPath,
  [switch]$CheckOnly
)

$ErrorActionPreference = 'Stop'

function Normalize-PathValue {
  param([AllowNull()][string]$Value)

  if ([string]::IsNullOrWhiteSpace($Value)) {
    return ''
  }

  return $Value.Trim().TrimEnd('\', '/').ToLowerInvariant()
}

function Expand-ContractTemplate {
  param(
    [Parameter(Mandatory)][string]$Template,
    [Parameter(Mandatory)][string]$ResolvedRepoRoot,
    [Parameter(Mandatory)][string]$ResolvedScriptPath
  )

  return $Template.Replace('{repo_root}', $ResolvedRepoRoot).
    Replace('{script_path}', $ResolvedScriptPath).
    Replace('{system_root}', $env:SystemRoot)
}

function Read-ShortcutContract {
  param([Parameter(Mandatory)][string]$ContractPath)

  if (-not (Test-Path -LiteralPath $ContractPath -PathType Leaf)) {
    throw "Shortcut contract was not found: $ContractPath"
  }

  $contract = Get-Content -LiteralPath $ContractPath -Raw | ConvertFrom-Json
  if ($contract.schema -ne 'narada.materialization_shortcut_contract.v1') {
    throw "Unsupported shortcut contract schema: $($contract.schema)"
  }
  if ([string]::IsNullOrWhiteSpace([string]$contract.shortcut_file_name)) {
    throw 'Shortcut contract is missing shortcut_file_name.'
  }
  if ([string]::IsNullOrWhiteSpace([string]$contract.script_relative_path)) {
    throw 'Shortcut contract is missing script_relative_path.'
  }
  if ([string]::IsNullOrWhiteSpace([string]$contract.arguments_template)) {
    throw 'Shortcut contract is missing arguments_template.'
  }
  if ([string]::IsNullOrWhiteSpace([string]$contract.description)) {
    throw 'Shortcut contract is missing description.'
  }
  return $contract
}

function Resolve-PowerShellTarget {
  param([Parameter(Mandatory)]$Contract)

  $preferred = $Contract.target.preferred_relative_path.Replace('{system_root}', $env:SystemRoot)
  if (Test-Path -LiteralPath $preferred -PathType Leaf) {
    return (Resolve-Path -LiteralPath $preferred).Path
  }

  $fallbackCommand = [string]$Contract.target.fallback_command
  if (-not [string]::IsNullOrWhiteSpace($fallbackCommand)) {
    try {
      $fallback = Get-Command $fallbackCommand -CommandType Application -ErrorAction Stop
      if ($fallback -and $fallback.Source) {
        return $fallback.Source
      }
    } catch {
      # The preferred Windows PowerShell target remains authoritative when available.
    }
  }

  throw "No PowerShell target is available for materialization shortcut. Preferred target: $preferred"
}

function Get-ExpectedShortcutState {
  param(
    [Parameter(Mandatory)]$Contract,
    [Parameter(Mandatory)][string]$ResolvedRepoRoot,
    [Parameter(Mandatory)][string]$ResolvedScriptPath,
    [Parameter(Mandatory)][string]$ResolvedTargetPath
  )

  return [ordered]@{
    target = $ResolvedTargetPath
    arguments = Expand-ContractTemplate -Template $Contract.arguments_template -ResolvedRepoRoot $ResolvedRepoRoot -ResolvedScriptPath $ResolvedScriptPath
    working_directory = Expand-ContractTemplate -Template ([string]$Contract.working_directory) -ResolvedRepoRoot $ResolvedRepoRoot -ResolvedScriptPath $ResolvedScriptPath
    window_style = [int]$Contract.window_style
    description = [string]$Contract.description
    icon_location = Expand-ContractTemplate -Template ([string]$Contract.icon_template) -ResolvedRepoRoot $ResolvedRepoRoot -ResolvedScriptPath $ResolvedScriptPath
  }
}

function Read-ShortcutState {
  param([Parameter(Mandatory)][string]$ShortcutPath)

  if (-not (Test-Path -LiteralPath $ShortcutPath -PathType Leaf)) {
    return $null
  }

  $shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut($ShortcutPath)
  return [ordered]@{
    target = [string]$shortcut.TargetPath
    arguments = [string]$shortcut.Arguments
    working_directory = [string]$shortcut.WorkingDirectory
    window_style = [int]$shortcut.WindowStyle
    description = [string]$shortcut.Description
    icon_location = [string]$shortcut.IconLocation
  }
}

function Read-StateSidecar {
  param([Parameter(Mandatory)][string]$SidecarPath)

  if (-not (Test-Path -LiteralPath $SidecarPath -PathType Leaf)) {
    return $null
  }

  return Get-Content -LiteralPath $SidecarPath -Raw | ConvertFrom-Json
}

function Get-DriftReasons {
  param(
    [AllowNull()]$Actual,
    [AllowNull()]$Sidecar,
    [Parameter(Mandatory)]$Expected,
    [Parameter(Mandatory)][string]$ContractHash,
    [Parameter(Mandatory)][string]$ScriptHash
  )

  $reasons = @()
  if ($null -eq $Actual) {
    $reasons += 'shortcut_missing'
  } else {
    if ((Normalize-PathValue $Actual.target) -ne (Normalize-PathValue $Expected.target)) { $reasons += 'target' }
    if ([string]$Actual.arguments -ne [string]$Expected.arguments) { $reasons += 'arguments' }
    if ((Normalize-PathValue $Actual.working_directory) -ne (Normalize-PathValue $Expected.working_directory)) { $reasons += 'working_directory' }
    if ([int]$Actual.window_style -ne [int]$Expected.window_style) { $reasons += 'window_style' }
    if ([string]$Actual.description -ne [string]$Expected.description) { $reasons += 'description' }
    if ([string]$Actual.icon_location -ne [string]$Expected.icon_location) { $reasons += 'icon_location' }
  }

  if ($null -eq $Sidecar) {
    $reasons += 'state_sidecar_missing'
  } else {
    if ([string]$Sidecar.schema -ne 'narada.materialization_shortcut_state.v1') { $reasons += 'state_schema' }
    if ([string]$Sidecar.contract_sha256 -ne $ContractHash) { $reasons += 'contract_hash' }
    if ([string]$Sidecar.script_sha256 -ne $ScriptHash) { $reasons += 'script_hash' }
    if ((Normalize-PathValue $Sidecar.repo_root) -ne (Normalize-PathValue $Expected.working_directory)) { $reasons += 'state_repo_root' }
  }

  return @($reasons)
}

function Write-ShortcutState {
  param(
    [Parameter(Mandatory)][string]$ShortcutPath,
    [Parameter(Mandatory)]$Expected
  )

  $parent = Split-Path -Parent $ShortcutPath
  if (-not (Test-Path -LiteralPath $parent -PathType Container)) {
    New-Item -ItemType Directory -Path $parent -Force | Out-Null
  }

  $shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut($ShortcutPath)
  $shortcut.TargetPath = $Expected.target
  $shortcut.Arguments = $Expected.arguments
  $shortcut.WorkingDirectory = $Expected.working_directory
  $shortcut.WindowStyle = $Expected.window_style
  $shortcut.Description = $Expected.description
  $shortcut.IconLocation = $Expected.icon_location
  $shortcut.Save()
}

function Write-StateSidecar {
  param(
    [Parameter(Mandatory)][string]$SidecarPath,
    [Parameter(Mandatory)][string]$ShortcutPath,
    [Parameter(Mandatory)]$Expected,
    [Parameter(Mandatory)][string]$ContractHash,
    [Parameter(Mandatory)][string]$ScriptHash
  )

  $state = [ordered]@{
    schema = 'narada.materialization_shortcut_state.v1'
    generated_at = [DateTimeOffset]::UtcNow.ToString('o')
    contract_sha256 = $ContractHash
    script_sha256 = $ScriptHash
    shortcut_path = $ShortcutPath
    repo_root = $Expected.working_directory
    target = $Expected.target
    arguments = $Expected.arguments
    working_directory = $Expected.working_directory
  }
  [System.IO.File]::WriteAllText(
    $SidecarPath,
    ($state | ConvertTo-Json -Depth 8),
    (New-Object System.Text.UTF8Encoding($false))
  )
}

function Emit-Result {
  param(
    [Parameter(Mandatory)][string]$Status,
    [Parameter(Mandatory)][string]$ShortcutPath,
    [Parameter(Mandatory)][string]$SidecarPath,
    [Parameter(Mandatory)][AllowEmptyCollection()][string[]]$Drift,
    [AllowNull()]$Expected,
    [AllowNull()]$Actual,
    [AllowNull()][string]$ErrorMessage
  )

  $result = [ordered]@{
    schema = 'narada.materialization_shortcut_result.v1'
    status = $Status
    shortcut = $ShortcutPath
    sidecar = $SidecarPath
    drift = @($Drift)
    expected = $Expected
    actual = $Actual
  }
  if (-not [string]::IsNullOrWhiteSpace($ErrorMessage)) {
    $result.error = $ErrorMessage
  }
  Write-Output ($result | ConvertTo-Json -Depth 10)
}

try {
  if ([string]::IsNullOrWhiteSpace($RepoRoot)) {
    $RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..') -ErrorAction Stop).Path
  } else {
    $RepoRoot = (Resolve-Path -LiteralPath $RepoRoot -ErrorAction Stop).Path
  }

  $contractPath = Join-Path $RepoRoot 'tools\materialization-shortcut.contract.json'
  $contract = Read-ShortcutContract -ContractPath $contractPath
  $scriptPath = (Resolve-Path (Join-Path $RepoRoot $contract.script_relative_path) -ErrorAction Stop).Path
  $targetPath = Resolve-PowerShellTarget -Contract $contract

  if ([string]::IsNullOrWhiteSpace($DesktopPath)) {
    $DesktopPath = [Environment]::GetFolderPath('Desktop')
  } else {
    $DesktopPath = [IO.Path]::GetFullPath($DesktopPath)
  }
  if ([string]::IsNullOrWhiteSpace($DesktopPath)) {
    throw 'The current Windows user has no resolvable Desktop folder.'
  }
  if (-not $CheckOnly -and -not (Test-Path -LiteralPath $DesktopPath -PathType Container)) {
    New-Item -ItemType Directory -Path $DesktopPath -Force | Out-Null
  }

  $shortcutPath = Join-Path $DesktopPath $contract.shortcut_file_name
  $sidecarPath = "$shortcutPath.narada.json"
  $expected = Get-ExpectedShortcutState -Contract $contract -ResolvedRepoRoot $RepoRoot -ResolvedScriptPath $scriptPath -ResolvedTargetPath $targetPath
  $contractHash = (Get-FileHash -LiteralPath $contractPath -Algorithm SHA256).Hash.ToLowerInvariant()
  $scriptHash = (Get-FileHash -LiteralPath $scriptPath -Algorithm SHA256).Hash.ToLowerInvariant()
  $actual = Read-ShortcutState -ShortcutPath $shortcutPath
  $sidecar = Read-StateSidecar -SidecarPath $sidecarPath
  $drift = @(Get-DriftReasons -Actual $actual -Sidecar $sidecar -Expected $expected -ContractHash $contractHash -ScriptHash $scriptHash)

  if ($CheckOnly) {
    if ($drift.Count -eq 0) {
      Emit-Result -Status 'in_sync' -ShortcutPath $shortcutPath -SidecarPath $sidecarPath -Drift $drift -Expected $expected -Actual $actual -ErrorMessage $null
      exit 0
    }
    Emit-Result -Status 'drifted' -ShortcutPath $shortcutPath -SidecarPath $sidecarPath -Drift $drift -Expected $expected -Actual $actual -ErrorMessage $null
    exit 1
  }

  if ($drift.Count -gt 0) {
    Write-ShortcutState -ShortcutPath $shortcutPath -Expected $expected
    Write-StateSidecar -SidecarPath $sidecarPath -ShortcutPath $shortcutPath -Expected $expected -ContractHash $contractHash -ScriptHash $scriptHash
  }

  $finalActual = Read-ShortcutState -ShortcutPath $shortcutPath
  $finalSidecar = Read-StateSidecar -SidecarPath $sidecarPath
  $finalDrift = @(Get-DriftReasons -Actual $finalActual -Sidecar $finalSidecar -Expected $expected -ContractHash $contractHash -ScriptHash $scriptHash)
  if ($finalDrift.Count -gt 0) {
    throw "Shortcut remained out of sync after repair: $($finalDrift -join ', ')"
  }

  Emit-Result -Status $(if ($drift.Count -gt 0) { 'repaired' } else { 'in_sync' }) -ShortcutPath $shortcutPath -SidecarPath $sidecarPath -Drift $drift -Expected $expected -Actual $finalActual -ErrorMessage $null
  exit 0
} catch {
  $message = ($_ | Out-String).Trim()
  $shortcutPathForError = if ($shortcutPath) { $shortcutPath } else { '' }
  $sidecarPathForError = if ($sidecarPath) { $sidecarPath } else { '' }
  Emit-Result -Status 'failed' -ShortcutPath $shortcutPathForError -SidecarPath $sidecarPathForError -Drift @() -Expected $null -Actual $null -ErrorMessage $message
  exit 1
}