; AppVersion/FileVersion come from Cargo metadata; InstallerFiles is generated
; from the exact-SHA product inventory by windows-installer-files.py.
#ifndef AppVersion
  #error AppVersion is required
#endif
#ifndef FileVersion
  #error FileVersion is required
#endif
#ifndef SourceDir
  #error SourceDir is required
#endif
#ifndef InstallerFiles
  #error InstallerFiles is required
#endif

[Setup]
; Keep this identity stable across releases so upgrades share the same installation.
AppId={{91ED532A-29D6-4EB6-9692-9EF3994B1DA3}
AppName=Iris
AppVersion={#AppVersion}
VersionInfoVersion={#FileVersion}
DefaultDirName={localappdata}\Programs\Iris
DefaultGroupName=Iris
PrivilegesRequired=lowest
; Iris ships x64 binaries only. The VC runtime prerequisite below must inspect
; the native (64-bit) system directory, never the SysWOW64 view.
ArchitecturesAllowed=x64
ArchitecturesInstallIn64BitMode=x64
OutputBaseFilename=Iris-windows-setup
UninstallDisplayIcon={app}\Iris.exe
WizardStyle=modern

; Tag compilation only: /DSignedUninstaller=yes activates the SignTool defined
; by the compiler's /s switch. Inno signs Setup, its temporary self-copies and
; the generated uninstaller. Rehearsals define nothing and stay unsigned.
#ifdef SignedUninstaller
SignTool=IrisCompilerSign
SignedUninstaller=yes
#endif

[CustomMessages]
; External prerequisite: no vc_redist binary is bundled with or downloaded by
; this installer. Microsoft documents that the matching 2015-2022 x64
; redistributable must already be installed on the target machine.
VcRedistMissing=Iris requires the Visual C++ Redistributable for Visual Studio 2015-2022 (x64), which is not installed on this computer. Install the matching x64 redistributable from Microsoft and run this installer again.

[Code]
// The checked-in VC import allowlist records that a runner's system-directory
// copy is not proof of what a target system provides. Iris x64 PE imports are
// satisfied by this externally installed runtime; no app-local copies exist.
function IsVcRedistInstalled: Boolean;
var
  SystemDir: string;
begin
  Result := False;
  if not Is64BitInstallMode then
    Exit;
  SystemDir := ExpandConstant('{sys}');
  if SystemDir = '' then
    Exit;
  Result := FileExists(SystemDir + '\vcruntime140.dll') and
            FileExists(SystemDir + '\vcruntime140_1.dll') and
            FileExists(SystemDir + '\msvcp140.dll');
end;

function InitializeSetup: Boolean;
begin
  Result := True;
  if not IsVcRedistInstalled then
  begin
    MsgBox(ExpandConstant('{cm:VcRedistMissing}'), mbError, MB_OK);
    Result := False;
  end;
end;

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; Flags: unchecked

[Files]
#include InstallerFiles

[Icons]
Name: "{userprograms}\Iris"; Filename: "{app}\Iris.exe"; Check: IsVcRedistInstalled
Name: "{userdesktop}\Iris"; Filename: "{app}\Iris.exe"; Tasks: desktopicon; Check: IsVcRedistInstalled
