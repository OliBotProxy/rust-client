#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Installs tunnel-client as a Windows service using WinSW.

.DESCRIPTION
    Copies tunnel-client.exe and tunnel-client-svc.exe (WinSW) to Program Files,
    creates an env.conf at C:\ProgramData\OliBot\tunnel-client\, and registers
    the service to start automatically.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File install.ps1
#>

$ErrorActionPreference = 'Stop'

$InstallDir  = "$env:ProgramFiles\OliBot\tunnel-client"
$ConfigDir   = "$env:ProgramData\OliBot\tunnel-client"
$ConfigFile  = "$ConfigDir\env.conf"
$ServiceName = 'tunnel-client'
$ScriptDir   = Split-Path -Parent $MyInvocation.MyCommand.Definition

Write-Host "=== Oli.bot Tunnel Client Installer ===" -ForegroundColor Cyan

# --- Copy binaries and WinSW wrapper ---
Write-Host "Installing to $InstallDir..."
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Copy-Item "$ScriptDir\tunnel-client.exe"     "$InstallDir\tunnel-client.exe"     -Force
Copy-Item "$ScriptDir\tunnel-client-svc.exe" "$InstallDir\tunnel-client-svc.exe" -Force
Copy-Item "$ScriptDir\tunnel-client-svc.xml" "$InstallDir\tunnel-client-svc.xml" -Force

# --- Create config file (only if it doesn't already exist) ---
New-Item -ItemType Directory -Force -Path $ConfigDir | Out-Null
if (-not (Test-Path $ConfigFile)) {
    @'
# Oli.bot Tunnel Client configuration
# Edit this file, then run: Restart-Service tunnel-client

# API endpoint — choose your region:
#   https://api-us.oli.bot/api   (United States)
#   https://api-eu.oli.bot/api   (Europe)
#   https://api-asia.oli.bot/api (Asia)
TUNNEL_API_URL=https://api-us.oli.bot/api

# Your tunnel ID from the oli.bot dashboard
TUNNEL_ID=YOUR_TUNNEL_ID

# Your API key (<subscriptionId>_<salt> format)
TUNNEL_API_KEY=YOUR_API_KEY
'@ | Set-Content $ConfigFile -Encoding UTF8
    Write-Host "Created config at $ConfigFile" -ForegroundColor Yellow
} else {
    Write-Host "Config already exists at $ConfigFile — not overwritten." -ForegroundColor Green
}

# --- Load env.conf values into user environment (WinSW reads them at start) ---
function Get-EnvValue($file, $key) {
    $line = Get-Content $file | Where-Object { $_ -match "^$key=" } | Select-Object -First 1
    if ($line) { return $line.Substring($key.Length + 1).Trim() }
    return ''
}

$apiUrl   = Get-EnvValue $ConfigFile 'TUNNEL_API_URL'
$tunnelId = Get-EnvValue $ConfigFile 'TUNNEL_ID'
$apiKey   = Get-EnvValue $ConfigFile 'TUNNEL_API_KEY'

# Set as machine-level env vars so WinSW XML can expand %TUNNEL_*%
[System.Environment]::SetEnvironmentVariable('TUNNEL_API_URL', $apiUrl,   'Machine')
[System.Environment]::SetEnvironmentVariable('TUNNEL_ID',      $tunnelId, 'Machine')
[System.Environment]::SetEnvironmentVariable('TUNNEL_API_KEY', $apiKey,   'Machine')

# --- Install / reinstall service via WinSW ---
$svc = "$InstallDir\tunnel-client-svc.exe"

if (Get-Service $ServiceName -ErrorAction SilentlyContinue) {
    Write-Host "Updating existing service..."
    & $svc stop    | Out-Null
    & $svc uninstall
}

Write-Host "Registering service..."
& $svc install

if ($tunnelId -eq 'YOUR_TUNNEL_ID' -or $apiKey -eq 'YOUR_API_KEY') {
    Write-Host ""
    Write-Host "=== Action required ===" -ForegroundColor Yellow
    Write-Host "Edit $ConfigFile with your credentials, then run:" -ForegroundColor Yellow
    Write-Host "  .\install.ps1         (re-run to apply changes)" -ForegroundColor White
    Write-Host "  Start-Service $ServiceName" -ForegroundColor White
} else {
    Write-Host "Starting service..."
    & $svc start
    Write-Host ""
    Write-Host "Done!" -ForegroundColor Green
    Get-Service $ServiceName | Select-Object Name, Status, StartType
    Write-Host "Logs: $InstallDir\tunnel-client.log" -ForegroundColor Cyan
}
