param([Parameter(Mandatory=$true)][string]$Destination)
$ErrorActionPreference='Stop'
$serviceRoot=Split-Path -Parent $PSScriptRoot
Push-Location $serviceRoot
try{
    & cargo build --release --locked
    if($LASTEXITCODE){throw 'Reader build failed.'}
    $manifest=Get-Content module.json -Raw | ConvertFrom-Json
    $final=[IO.Path]::GetFullPath((Join-Path $Destination "$($manifest.id)/$($manifest.version)"))
    $package="$final.pending"
    if((Test-Path -LiteralPath $final) -or (Test-Path -LiteralPath $package)){throw 'Package destination already exists; use a fresh destination.'}
    # Match the Windows package target; other platforms' dependencies are not shipped.
    $metadata=(& cargo metadata --locked --format-version 1 --filter-platform x86_64-pc-windows-msvc | ConvertFrom-Json)
    if($LASTEXITCODE){throw 'Dependency metadata failed.'}
    New-Item -ItemType Directory -Path (Join-Path $package 'bin') -Force | Out-Null
    Copy-Item -LiteralPath target/release/reader-service.exe -Destination (Join-Path $package 'bin')
    Copy-Item -LiteralPath module.json,LICENSE -Destination $package
    $notices=@('# Third-party dependencies','', 'Exact sources are pinned by Cargo.lock. License texts accompany this package.','')
    foreach($dependency in $metadata.packages | Where-Object {$_.source}){
        $notices+="- $($dependency.name) $($dependency.version): $($dependency.license) — $($dependency.repository)"
        $texts=@(Get-ChildItem -LiteralPath (Split-Path -Parent $dependency.manifest_path) -File | Where-Object {$_.Name -match '^(LICENSE|LICENCE|COPYING|COPYRIGHT|UNLICENSE)'} )
        if(!$texts.Count){throw "Missing dependency license text: $($dependency.name)"}
        $licenses=Join-Path $package "licenses/$($dependency.name)-$($dependency.version)"
        New-Item -ItemType Directory -Path $licenses -Force | Out-Null
        foreach($text in $texts){Copy-Item -LiteralPath $text.FullName -Destination $licenses}
    }
    $notices | Set-Content -LiteralPath (Join-Path $package 'THIRD-PARTY.md') -Encoding utf8
    $archive=Join-Path $serviceRoot "target/packages/reader-$($manifest.version).zip"
    New-Item -ItemType Directory -Path (Split-Path -Parent $archive) -Force | Out-Null
    # Registry license files may carry pre-1980 timestamps, outside ZIP's supported range.
    Get-ChildItem -LiteralPath $package -Recurse | ForEach-Object {$_.LastWriteTime=[datetime]'2020-01-01T00:00:00'}
    # Compress-Archive on Windows PowerShell emits backslashes rejected by safe ZIP readers.
    Add-Type -AssemblyName System.IO.Compression,System.IO.Compression.FileSystem
    $zip=[IO.Compression.ZipFile]::Open($archive,[IO.Compression.ZipArchiveMode]::Create)
    try{
        foreach($file in Get-ChildItem -LiteralPath $package -Recurse -File){
            $relative=$file.FullName.Substring($package.Length+1).Replace('\','/')
            [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip,$file.FullName,$relative,[IO.Compression.CompressionLevel]::Optimal) | Out-Null
        }
    }finally{$zip.Dispose()}
    $check=[IO.Compression.ZipFile]::OpenRead($archive)
    try{
        foreach($entry in $check.Entries){if($entry.FullName.Contains('\')){throw 'Package ZIP must use forward-slash paths.'}}
    }finally{$check.Dispose()}
    Move-Item -LiteralPath $package -Destination $final
    Get-FileHash -Algorithm SHA256 -LiteralPath $archive
    Write-Host "Prepared complete reader module: $final"
}finally{Pop-Location}
