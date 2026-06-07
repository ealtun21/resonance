# Build resonance_apo.dll: the Rust engine (staticlib, unwind profile, static CRT)
# linked into the C++ COM/aggregation shell via MSVC. Run on Windows with the
# VS2022 Build Tools + Windows SDK installed.
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path "$PSScriptRoot\..\..").Path
Set-Location $repo

# 1) Rust engine staticlib (static CRT so it links cleanly into the C++ DLL).
$env:RUSTFLAGS = '-C target-feature=+crt-static'
$env:CARGO_TERM_COLOR = 'never'   # keep ANSI escapes out of captured output
cargo build --profile apo -p resonance-apo
$lib = "$repo\target\apo\resonance_apo.lib"
if (-not (Test-Path $lib)) { throw "missing $lib" }

# 2) Native libs the Rust std staticlib needs (kernel32, ntdll, bcrypt, ...).
#    Captured via cmd redirection so cargo's stderr progress isn't treated fatal.
$nslFile = "$env:TEMP\resonance_apo_nsl.txt"
cmd /c "cargo rustc -p resonance-apo --profile apo -- --print native-static-libs > `"$nslFile`" 2>&1"
$nslLine = Get-Content $nslFile | Select-String 'native-static-libs:' | Select-Object -First 1
$nsl = if ($nslLine) { ($nslLine.ToString() -replace '.*native-static-libs:\s*', '').Trim() } else { '' }
$nsl = $nsl -replace "$([char]27)\[[0-9;]*m", ''   # strip any residual ANSI escapes
if (-not $nsl) {
    $nsl = 'kernel32.lib advapi32.lib ntdll.lib userenv.lib ws2_32.lib dbghelp.lib bcrypt.lib synchronization.lib'
}
Write-Output "native-static-libs: $nsl"

# 3) Compile + link the C++ shell against the SDK APO libs.
$vcvars = Get-ChildItem 'C:\Program Files*\Microsoft Visual Studio\*\*\VC\Auxiliary\Build\vcvars64.bat' -EA SilentlyContinue |
    Select-Object -First 1 -ExpandProperty FullName
if (-not $vcvars) { throw 'vcvars64.bat not found (install VS Build Tools + C++)' }

$cpp = "$repo\crates\resonance-apo\cpp\resonance_apo.cpp"
$def = "$repo\crates\resonance-apo\cpp\resonance_apo.def"
$obj = "$env:TEMP\resonance_apo.obj"
$out = "$repo\target\apo\resonance_apo.dll"
Remove-Item $out -ErrorAction SilentlyContinue  # don't be fooled by a stale DLL

$bat = "$env:TEMP\build_apo.bat"
@"
@call "$vcvars" >nul
cl /nologo /c /EHsc /O2 /MT /std:c++17 "$cpp" /Fo"$obj"
if errorlevel 1 exit /b 1
link /nologo /DLL /DEF:"$def" /OUT:"$out" /IMPLIB:"$env:TEMP\resonance_apo_dll.lib" /NODEFAULTLIB:atls.lib "$obj" "$lib" audiobaseprocessingobject.lib audioeng.lib audiomediatypecrt.lib wmcodecdspuuid.lib legacy_stdio_definitions.lib advapi32.lib ole32.lib oleaut32.lib $nsl
"@ | Set-Content -Encoding ASCII $bat

cmd /c "`"$bat`""
if (Test-Path $out) { Write-Output "OK: $out" } else { throw 'link failed' }
