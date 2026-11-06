; Prototype-only Inno Setup script for the isolated Windows preview prototype.
; Test-only; never a release payload. Installs the staged prototype app dir to a
; fresh directory for smoke execution, then the smoke script uninstalls it.
#ifndef AppVersion
  #error AppVersion is required
#endif
#ifndef SourceDir
  #error SourceDir is required
#endif
[Setup]
AppName=Iris Windows Preview Prototype
AppVersion={#AppVersion}
DefaultDirName={autopf}\IrisPrototype
DisableProgramGroupPage=yes
DisableDirPage=no
OutputBaseFilename=Iris-prototype-setup
Compression=lzma2/ultra64
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayName=Iris Windows Preview Prototype (test-only)
CreateUninstallRegKey=yes

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: recursesubdirs ignoreversion

[Icons]
Name: "{autopf}\Iris Windows Preview Prototype (test-only)"; Filename: "{app}\Iris.exe"
