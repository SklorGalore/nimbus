# Runs the uninstaller that Nimbus's Inno Setup installer registered.
$ErrorActionPreference = 'Stop'

[array]$keys = Get-UninstallRegistryKey -SoftwareName 'Nimbus*'

if ($keys.Count -eq 1) {
  $packageArgs = @{
    packageName    = $env:ChocolateyPackageName
    fileType       = 'exe'
    silentArgs     = '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART'
    validExitCodes = @(0)
    file           = ($keys[0].UninstallString -replace '"', '')
  }
  Uninstall-ChocolateyPackage @packageArgs
} elseif ($keys.Count -eq 0) {
  Write-Warning "Nimbus is not in Programs and Features; nothing to uninstall."
} else {
  Write-Warning "$($keys.Count) programs match 'Nimbus*'; uninstall them from Settings > Apps:"
  $keys | ForEach-Object { Write-Warning "- $($_.DisplayName)" }
}
