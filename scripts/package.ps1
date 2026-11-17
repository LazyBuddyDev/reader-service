param([Parameter(Mandatory=$true)][string]$Destination)
$ErrorActionPreference='Stop'
$serviceRoot=Split-Path -Parent $PSScriptRoot
Push-Location $serviceRoot
try{
    & cargo build --release --locked
    if($LASTEXITCODE){throw 'Reader build failed.'}
    Push-Location (Join-Path $serviceRoot 'helpers/fanqie')
    try{
        & cargo build --release --locked
        if($LASTEXITCODE){throw 'Native chapter helper build failed.'}
        $helperMetadata=(& cargo metadata --locked --format-version 1 --filter-platform x86_64-pc-windows-msvc | ConvertFrom-Json)
        if($LASTEXITCODE){throw 'Helper dependency metadata failed.'}
    }finally{Pop-Location}
    $manifest=Get-Content module.json -Raw | ConvertFrom-Json
    $final=[IO.Path]::GetFullPath((Join-Path $Destination "$($manifest.id)/$($manifest.version)"))
    $package="$final.pending"
    if((Test-Path -LiteralPath $final) -or (Test-Path -LiteralPath $package)){throw 'Package destination already exists; use a fresh destination.'}
    # Match the Windows package target; other platforms' dependencies are not shipped.
    $metadata=(& cargo metadata --locked --format-version 1 --filter-platform x86_64-pc-windows-msvc | ConvertFrom-Json)
    if($LASTEXITCODE){throw 'Dependency metadata failed.'}
    New-Item -ItemType Directory -Path (Join-Path $package 'bin') -Force | Out-Null
    Copy-Item -LiteralPath target/release/reader-service.exe -Destination (Join-Path $package 'bin')
    Copy-Item -LiteralPath helpers/fanqie/target/release/fanqie-helper.exe -Destination (Join-Path $package 'bin')
    Copy-Item -LiteralPath module.json,LICENSE -Destination $package
    $notices=@('# Third-party dependencies','', 'Exact sources are pinned by Cargo.lock. License texts accompany this package.','')
    $sourceDependencies=@()
    $notices+='- fanqie-helper 0.1.0: GPL-3.0-only; corresponding source: sources/fanqie-helper-0.1.0.zip; upstream commit 9ba6f8151a24815be1c5a6917750704031defa96'
    $helperLicenses=Join-Path $package 'licenses/fanqie-helper-0.1.0'
    New-Item -ItemType Directory -Path $helperLicenses -Force | Out-Null
    Copy-Item -LiteralPath helpers/fanqie/vendor/LICENSE,helpers/fanqie/vendor/THIRD_PARTY_NOTICES.txt -Destination $helperLicenses
    foreach($dependency in (@($metadata.packages)+@($helperMetadata.packages)) | Where-Object {$_.source} | Sort-Object name,version -Unique){
        $notices+="- $($dependency.name) $($dependency.version): $($dependency.license) — $($dependency.repository)"
        $texts=@(Get-ChildItem -LiteralPath (Split-Path -Parent $dependency.manifest_path) -File | Where-Object {$_.Name -match '^(LICENSE|LICENCE|COPYING|COPYRIGHT|UNLICENSE)'} )
        if(!$texts.Count){
            $verifiedLicense=Join-Path $serviceRoot "helpers/fanqie/licenses/$($dependency.name)-$($dependency.version)"
            if(!(Test-Path -LiteralPath $verifiedLicense)){$verifiedLicense=Join-Path $serviceRoot "licenses/$($dependency.name)-$($dependency.version)"}
            if(Test-Path -LiteralPath $verifiedLicense){$texts=@(Get-ChildItem -LiteralPath $verifiedLicense -File)}
        }
        if(!$texts.Count){throw "Missing dependency license text: $($dependency.name)"}
        if($dependency.license -match 'MPL-2.0'){
            $sourceDependencies+=@{dependency=$dependency;licenses=$texts}
            $notices+="  Source: sources/$($dependency.name)-$($dependency.version).zip"
        }
        $licenses=Join-Path $package "licenses/$($dependency.name)-$($dependency.version)"
        New-Item -ItemType Directory -Path $licenses -Force | Out-Null
        foreach($text in $texts){Copy-Item -LiteralPath $text.FullName -Destination $licenses}
        if($dependency.name -eq 'luajit-src'){
            Copy-Item -LiteralPath (Join-Path (Split-Path -Parent $dependency.manifest_path) 'luajit2/COPYRIGHT') -Destination (Join-Path $licenses 'LuaJIT-COPYRIGHT')
        }
    }
    $notices | Set-Content -LiteralPath (Join-Path $package 'THIRD-PARTY.md') -Encoding utf8
    $archive=Join-Path $serviceRoot "target/packages/reader-$($manifest.version).zip"
    New-Item -ItemType Directory -Path (Split-Path -Parent $archive) -Force | Out-Null
    # Registry license files may carry pre-1980 timestamps, outside ZIP's supported range.
    Get-ChildItem -LiteralPath $package -Recurse | ForEach-Object {$_.LastWriteTime=[datetime]'2020-01-01T00:00:00'}
    # Compress-Archive on Windows PowerShell emits backslashes rejected by safe ZIP readers.
    Add-Type -AssemblyName System.IO.Compression,System.IO.Compression.FileSystem
    $sourceDirectory=Join-Path $package 'sources'
    New-Item -ItemType Directory -Path $sourceDirectory -Force | Out-Null
    foreach($source in $sourceDependencies){
        $dependency=$source.dependency
        $crateRoot=Split-Path -Parent $dependency.manifest_path
        $crateZip=[IO.Compression.ZipFile]::Open((Join-Path $sourceDirectory "$($dependency.name)-$($dependency.version).zip"),[IO.Compression.ZipArchiveMode]::Create)
        try{
            foreach($file in Get-ChildItem -LiteralPath $crateRoot -Recurse -File){
                $relative=$file.FullName.Substring($crateRoot.Length+1).Replace('\','/')
                if($relative -in @('.cargo-ok','.cargo-checksum.json')){continue}
                $entry=$crateZip.CreateEntry($relative,[IO.Compression.CompressionLevel]::Optimal)
                $entry.LastWriteTime=[datetime]'2020-01-01T00:00:00'
                $inputStream=[IO.File]::OpenRead($file.FullName)
                $outputStream=$entry.Open()
                try{$inputStream.CopyTo($outputStream)}finally{$inputStream.Dispose();$outputStream.Dispose()}
            }
            foreach($text in $source.licenses){
                if(!$text.FullName.StartsWith($crateRoot+[IO.Path]::DirectorySeparatorChar)){
                    [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($crateZip,$text.FullName,$text.Name,[IO.Compression.CompressionLevel]::Optimal) | Out-Null
                }
            }
        }finally{$crateZip.Dispose()}
    }
    $helperRoot=Join-Path $serviceRoot 'helpers/fanqie'
    $sourceFiles=@(Get-Item -LiteralPath (Join-Path $helperRoot 'Cargo.toml'),(Join-Path $helperRoot 'Cargo.lock'),(Join-Path $helperRoot 'README.md'))+@(Get-ChildItem -LiteralPath (Join-Path $helperRoot 'src'),(Join-Path $helperRoot 'vendor'),(Join-Path $helperRoot 'licenses') -Recurse -File)
    $sourceZip=[IO.Compression.ZipFile]::Open((Join-Path $sourceDirectory 'fanqie-helper-0.1.0.zip'),[IO.Compression.ZipArchiveMode]::Create)
    try{
        foreach($sourceFile in $sourceFiles){
            $relative=$sourceFile.FullName.Substring($helperRoot.Length+1).Replace('\','/')
            [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($sourceZip,$sourceFile.FullName,$relative,[IO.Compression.CompressionLevel]::Optimal) | Out-Null
        }
    }finally{$sourceZip.Dispose()}
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
