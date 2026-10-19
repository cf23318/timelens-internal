$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw '请在 Windows 构建' }
Set-Location (Join-Path $PSScriptRoot '..')
Get-Command cargo -ErrorAction Stop | Out-Null
Get-Command npm -ErrorAction Stop | Out-Null
node -e 'if (Number(process.versions.node.split(".")[0]) < 22) process.exit(1)'
if ($LASTEXITCODE -ne 0) { throw '需要 Node.js 22 或更新版本' }
if (!(Test-Path node_modules)) { throw '请先通过代理运行 npm ci --ignore-scripts 安装依赖' }
if (!$env:CARGO_NET_OFFLINE) { $env:CARGO_NET_OFFLINE = 'true' }
# Require WebView2 to be installed on the test machine; do not download a bootstrapper.
$internalConfigPath = [System.IO.Path]::GetTempFileName()
try {
    [System.IO.File]::WriteAllText($internalConfigPath, '{"bundle":{"windows":{"webviewInstallMode":{"type":"skip"}}}}', (New-Object System.Text.UTF8Encoding($false)))
    npm run tauri -- build --bundles nsis --config $internalConfigPath
    if ($LASTEXITCODE -ne 0) { throw 'Windows 构建失败，请检查 MSVC、NSIS 与依赖缓存' }
    Write-Host '本地安装包构建完成；分发前仍需组织代码签名及安全软件实机验证。'
} finally {
    Remove-Item $internalConfigPath -ErrorAction SilentlyContinue
}
