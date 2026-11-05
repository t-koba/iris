; Inputs are supplied by package-windows.ps1 from Cargo metadata and the built app.
#ifndef AppVersion
  #error AppVersion is required
#endif
#ifndef FileVersion
  #error FileVersion is required
#endif
#ifndef SourceDir
  #error SourceDir is required
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
OutputBaseFilename=Iris-windows-setup
UninstallDisplayIcon={app}\Iris.exe
WizardStyle=modern

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; Flags: unchecked

[Files]
Source: "{#SourceDir}\Iris.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\Iris"; Filename: "{app}\Iris.exe"
Name: "{userdesktop}\Iris"; Filename: "{app}\Iris.exe"; Tasks: desktopicon
