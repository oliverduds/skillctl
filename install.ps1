$ErrorActionPreference = "Stop"

$Repo = "oliverduds/skillctl"
$BinName = "skillctl.exe"
$Target = "x86_64-pc-windows-msvc"
$Version = if ($env:SKILLCTL_VERSION) { $env:SKILLCTL_VERSION } else { "latest" }
$InstallDir = if ($env:SKILLCTL_INSTALL_DIR) {
    $env:SKILLCTL_INSTALL_DIR
} else {
    Join-Path $env:LOCALAPPDATA "Programs\skillctl"
}

$BaseUrl = if ($Version -eq "latest") {
    "https://github.com/$Repo/releases/latest/download"
} else {
    "https://github.com/$Repo/releases/download/$Version"
}

$Archive = "skillctl-$Target.zip"
$Url = "$BaseUrl/$Archive"
$SumsUrl = "$BaseUrl/SHA256SUMS.txt"

$TmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ("skillctl-install-" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force -Path $TmpDir | Out-Null

try {
    $ZipPath = Join-Path $TmpDir $Archive
    Write-Host "Baixando skillctl ($Target)..." -ForegroundColor Cyan
    Invoke-WebRequest -Uri $Url -OutFile $ZipPath -UseBasicParsing

    # Verificacao SHA-256 opcional quando SHA256SUMS.txt estiver disponivel
    try {
        $SumsPath = Join-Path $TmpDir "SHA256SUMS.txt"
        Invoke-WebRequest -Uri $SumsUrl -OutFile $SumsPath -UseBasicParsing
        $ExpectedLine = Select-String -Path $SumsPath -Pattern ([regex]::Escape($Archive) + "$") | Select-Object -First 1
        if ($ExpectedLine) {
            $Expected = ($ExpectedLine.Line -split "\s+")[0].ToLower()
            $Actual = (Get-FileHash -Path $ZipPath -Algorithm SHA256).Hash.ToLower()
            if ($Actual -ne $Expected) {
                throw "Falha na verificacao SHA-256 (esperado $Expected, obtido $Actual)"
            }
        }
    } catch {
        if ($_.Exception.Message -like "*SHA-256*") { throw }
    }

    Expand-Archive -Path $ZipPath -DestinationPath $TmpDir -Force
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

    $SrcExe = Join-Path $TmpDir $BinName
    if (-not (Test-Path $SrcExe)) {
        throw "Executavel $BinName nao encontrado dentro de $Archive"
    }

    $DestExe = Join-Path $InstallDir $BinName
    Copy-Item -Path $SrcExe -Destination $DestExe -Force

    # Adiciona ao PATH do usuario sem duplicar
    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $Parts = if ($UserPath) { $UserPath -split ";" | Where-Object { $_ -ne "" } } else { @() }
    if ($Parts -notcontains $InstallDir) {
        $NewUserPath = ($Parts + $InstallDir) -join ";"
        [Environment]::SetEnvironmentVariable("Path", $NewUserPath, "User")
        Write-Host "Adicionado '$InstallDir' ao PATH do usuario." -ForegroundColor Green
    }
    if (($env:Path -split ";") -notcontains $InstallDir) {
        $env:Path = "$InstallDir;$env:Path"
    }

    Write-Host ""
    Write-Host "skillctl instalado com sucesso em: $DestExe" -ForegroundColor Green
    Write-Host "Execute 'skillctl' para abrir o menu interativo (reabra o terminal se necessario)." -ForegroundColor Cyan
}
finally {
    Remove-Item -Path $TmpDir -Recurse -Force -ErrorAction SilentlyContinue
}
