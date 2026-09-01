[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string[]]$ExecutablePath
)

$ErrorActionPreference = "Stop"
$ExpectedDependentLoadFlags = 0x0800 # LOAD_LIBRARY_SEARCH_SYSTEM32

function Read-UInt16At {
    param([IO.BinaryReader]$Reader, [long]$Offset)
    $Reader.BaseStream.Position = $Offset
    return $Reader.ReadUInt16()
}

function Read-UInt32At {
    param([IO.BinaryReader]$Reader, [long]$Offset)
    $Reader.BaseStream.Position = $Offset
    return $Reader.ReadUInt32()
}

function Convert-RvaToFileOffset {
    param(
        [IO.BinaryReader]$Reader,
        [uint32]$Rva,
        [long]$SectionTableOffset,
        [uint16]$SectionCount
    )
    for ($index = 0; $index -lt $SectionCount; $index++) {
        $section = $SectionTableOffset + (40 * $index)
        $virtualSize = Read-UInt32At -Reader $Reader -Offset ($section + 8)
        $virtualAddress = Read-UInt32At -Reader $Reader -Offset ($section + 12)
        $rawSize = Read-UInt32At -Reader $Reader -Offset ($section + 16)
        $rawPointer = Read-UInt32At -Reader $Reader -Offset ($section + 20)
        $span = [Math]::Max([uint64]$virtualSize, [uint64]$rawSize)
        if ([uint64]$Rva -ge [uint64]$virtualAddress -and
            [uint64]$Rva -lt ([uint64]$virtualAddress + $span)) {
            return [long]$rawPointer + ([long]$Rva - [long]$virtualAddress)
        }
    }
    throw "PE load-config RVA does not map to a section"
}

foreach ($candidate in $ExecutablePath) {
    $path = [IO.Path]::GetFullPath($candidate)
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "PE executable does not exist: $path"
    }
    $stream = [IO.File]::Open($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        $reader = [IO.BinaryReader]::new($stream)
        if ((Read-UInt16At -Reader $reader -Offset 0) -ne 0x5A4D) {
            throw "Not a DOS/PE executable: $path"
        }
        $peOffset = Read-UInt32At -Reader $reader -Offset 0x3C
        if ((Read-UInt32At -Reader $reader -Offset $peOffset) -ne 0x00004550) {
            throw "Invalid PE signature: $path"
        }
        $sectionCount = Read-UInt16At -Reader $reader -Offset ($peOffset + 6)
        $optionalHeaderSize = Read-UInt16At -Reader $reader -Offset ($peOffset + 20)
        $optionalHeader = [long]$peOffset + 24
        if ((Read-UInt16At -Reader $reader -Offset $optionalHeader) -ne 0x020B) {
            throw "Expected a PE32+ x64 executable: $path"
        }
        $directoryCount = Read-UInt32At -Reader $reader -Offset ($optionalHeader + 108)
        if ($directoryCount -le 10) {
            throw "PE executable has no load-config directory: $path"
        }
        $loadConfigDirectory = $optionalHeader + 112 + (10 * 8)
        $loadConfigRva = Read-UInt32At -Reader $reader -Offset $loadConfigDirectory
        $loadConfigDirectorySize = Read-UInt32At -Reader $reader -Offset ($loadConfigDirectory + 4)
        if ($loadConfigRva -eq 0 -or $loadConfigDirectorySize -lt 80) {
            throw "PE load-config directory is missing or too small: $path"
        }
        $sectionTable = $optionalHeader + $optionalHeaderSize
        $loadConfigOffset = Convert-RvaToFileOffset `
            -Reader $reader `
            -Rva $loadConfigRva `
            -SectionTableOffset $sectionTable `
            -SectionCount $sectionCount
        $structureSize = Read-UInt32At -Reader $reader -Offset $loadConfigOffset
        if ($structureSize -lt 80) {
            throw "PE load-config structure cannot contain DependentLoadFlags: $path"
        }
        $actual = Read-UInt16At -Reader $reader -Offset ($loadConfigOffset + 78)
        if ($actual -ne $ExpectedDependentLoadFlags) {
            throw ("Unsafe PE DependentLoadFlags in {0}: expected 0x{1:X4}, actual 0x{2:X4}" -f `
                $path, $ExpectedDependentLoadFlags, $actual)
        }
        [pscustomobject]@{
            executable = $path
            dependentLoadFlags = ("0x{0:X4}" -f $actual)
            staticDependencySearch = "System32Only"
        }
    }
    finally {
        if ($null -ne $reader) {
            $reader.Dispose()
        }
        else {
            $stream.Dispose()
        }
    }
}
