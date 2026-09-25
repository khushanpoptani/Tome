param(
  [Parameter(Mandatory = $true)]
  [string]$Installer
)

$ErrorActionPreference = 'Stop'
$installerFile = Get-Item $Installer
if ($installerFile.Length -lt 1MB) {
  throw "Installer is unexpectedly small: $($installerFile.Length) bytes"
}

$builtHost = Resolve-Path 'target\x86_64-pc-windows-msvc\release\tome-server-dashboard.exe'
$bytes = [System.IO.File]::ReadAllBytes($builtHost)
$peOffset = [BitConverter]::ToInt32($bytes, 0x3c)
$optionalHeader = $peOffset + 24
$subsystem = [BitConverter]::ToUInt16($bytes, $optionalHeader + 68)
if ($subsystem -ne 2) {
  throw "Expected a Windows GUI executable (subsystem 2), found subsystem $subsystem"
}

$installRoot = Join-Path $env:RUNNER_TEMP 'tome-server-installer-smoke'
$appData = Join-Path $env:LOCALAPPDATA 'com.khushanpoptani.tome-server'
$dataDirectory = Join-Path $appData 'ci-data'
New-Item -ItemType Directory -Path $dataDirectory -Force | Out-Null
@{
  port = 17331
  lan_enabled = $false
  tailscale_enabled = $false
  firewall_enabled = $false
  launch_at_login = $false
  data_directory = $dataDirectory
} | ConvertTo-Json | Set-Content (Join-Path $appData 'settings.json') -Encoding UTF8

$install = Start-Process -FilePath $installerFile.FullName -ArgumentList '/S', "/D=$installRoot" -Wait -PassThru
if ($install.ExitCode -ne 0) {
  throw "Silent installer exited with $($install.ExitCode)"
}

$installedHost = Get-ChildItem $installRoot -Filter 'tome-server-dashboard.exe' -Recurse | Select-Object -First 1
if (-not $installedHost) {
  throw 'Installed dashboard executable was not found.'
}

Start-Sleep -Seconds 2
$installedProcessName = [System.IO.Path]::GetFileNameWithoutExtension($installedHost.Name)
$autoStarted = @(
  Get-Process -Name $installedProcessName -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -eq $installedHost.FullName }
)
if ($autoStarted.Count -gt 0) {
  $autoStarted | Stop-Process -Force
  throw 'Silent install auto-started Tome Server.'
}
$unexpectedListener = Get-NetTCPConnection -LocalPort 17331 -State Listen -ErrorAction SilentlyContinue
if ($unexpectedListener) {
  throw 'Tome Server test port was listening before the installed app was explicitly started.'
}

$lanRule = Get-NetFirewallRule -DisplayName 'Tome Server (Private LAN)' -ErrorAction Stop
$tailscaleRule = Get-NetFirewallRule -DisplayName 'Tome Server (Tailscale)' -ErrorAction Stop
if ($lanRule.Profile -notmatch 'Private') {
  throw "LAN firewall rule is not limited to the Private profile: $($lanRule.Profile)"
}
$lanFilter = $lanRule | Get-NetFirewallAddressFilter
$tailscaleFilter = $tailscaleRule | Get-NetFirewallAddressFilter
if ($lanFilter.RemoteAddress -notcontains 'LocalSubnet') {
  throw 'LAN firewall rule is not scoped to LocalSubnet.'
}
$tailscaleAddresses = @($tailscaleFilter.RemoteAddress)
if ($tailscaleAddresses -notcontains '100.64.0.0-100.127.255.255') {
  throw "Tailscale firewall rule is not scoped to the CGNAT range. Found: $($tailscaleAddresses -join ', ')"
}

$hostProcess = Start-Process -FilePath $installedHost.FullName -PassThru
try {
  $healthy = $false
  for ($attempt = 0; $attempt -lt 30; $attempt++) {
    try {
      $response = Invoke-RestMethod 'http://127.0.0.1:17331/api/v1/health' -TimeoutSec 1
      if ($response.status -eq 'ok' -and $response.protocol_version -eq 1) {
        $healthy = $true
        break
      }
    } catch {
      Start-Sleep -Milliseconds 500
    }
  }
  if (-not $healthy) {
    throw 'Installed Tome Server did not report healthy status.'
  }
} finally {
  if (-not $hostProcess.HasExited) {
    Stop-Process -Id $hostProcess.Id -Force
    $hostProcess.WaitForExit()
  }
}

$uninstaller = Get-ChildItem $installRoot -Filter 'uninstall.exe' -Recurse | Select-Object -First 1
if (-not $uninstaller) {
  throw 'NSIS uninstaller was not installed.'
}
$uninstall = Start-Process -FilePath $uninstaller.FullName -ArgumentList '/S' -Wait -PassThru
if ($uninstall.ExitCode -ne 0) {
  throw "Silent uninstaller exited with $($uninstall.ExitCode)"
}
if (Get-NetFirewallRule -DisplayName 'Tome Server (Private LAN)' -ErrorAction SilentlyContinue) {
  throw 'LAN firewall rule remained after uninstall.'
}
if (Get-NetFirewallRule -DisplayName 'Tome Server (Tailscale)' -ErrorAction SilentlyContinue) {
  throw 'Tailscale firewall rule remained after uninstall.'
}

Write-Host "Validated installer $($installerFile.Name), silent non-launch, GUI subsystem, health endpoint, firewall scope, and uninstall cleanup."
