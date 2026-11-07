param(
    [Parameter(Mandatory = $true)]
    [string]$BinaryPath
)

$ErrorActionPreference = "Stop"

function Get-RequiredEnvironmentValue {
    param([Parameter(Mandatory = $true)][string]$Name)

    $value = [Environment]::GetEnvironmentVariable($Name)
    if ([string]::IsNullOrWhiteSpace($value)) {
        throw "Required environment variable '$Name' is missing"
    }
    return $value
}

$runnerTemp = [IO.Path]::GetFullPath((Get-RequiredEnvironmentValue "RUNNER_TEMP"))
$certificatePath = [IO.Path]::GetFullPath((Get-RequiredEnvironmentValue "WINDOWS_CERTIFICATE_PATH"))
$relativeCertificatePath = [IO.Path]::GetRelativePath($runnerTemp, $certificatePath)
if ($relativeCertificatePath.StartsWith("..", [StringComparison]::Ordinal) -or
    [IO.Path]::IsPathRooted($relativeCertificatePath)) {
    throw "The Windows signing certificate must be stored below RUNNER_TEMP"
}
if (-not (Test-Path -LiteralPath $certificatePath -PathType Leaf)) {
    throw "The Windows signing certificate file is missing"
}

$certificatePassword = Get-RequiredEnvironmentValue "WINDOWS_CERTIFICATE_PASSWORD"
$timestampUrl = Get-RequiredEnvironmentValue "WINDOWS_TIMESTAMP_URL"
$resolvedBinaryPath = (Resolve-Path -LiteralPath $BinaryPath).Path

$signToolCommand = Get-Command "signtool.exe" -ErrorAction SilentlyContinue
if ($null -ne $signToolCommand) {
    $signToolPath = $signToolCommand.Source
} else {
    $sdkRoot = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
    $signToolPath = Get-ChildItem -Path $sdkRoot -Filter "signtool.exe" -File -Recurse |
        Where-Object { $_.FullName -match "\\x64\\signtool\.exe$" } |
        Sort-Object FullName -Descending |
        Select-Object -First 1 -ExpandProperty FullName
}
if ([string]::IsNullOrWhiteSpace($signToolPath)) {
    throw "signtool.exe was not found"
}

$signArguments = @(
    "sign",
    "/fd", "SHA256",
    "/f", $certificatePath,
    "/p", $certificatePassword,
    "/tr", $timestampUrl,
    "/td", "SHA256",
    $resolvedBinaryPath
)
& $signToolPath @signArguments
if ($LASTEXITCODE -ne 0) {
    throw "signtool.exe failed to sign a release binary"
}

& $signToolPath "verify" "/pa" "/all" $resolvedBinaryPath
if ($LASTEXITCODE -ne 0) {
    throw "signtool.exe could not verify a signed release binary"
}
