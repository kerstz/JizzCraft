; Inno Setup script: per-user installer, no admin rights, no registry noise
; beyond what Windows needs for an uninstall entry. Built by build.ps1 after the
; portable tree exists in dist\JizzCraft (the CI runner has ISCC preinstalled).
;
; Uninstalling removes the program only: your library, settings and thumbnails
; (under %APPDATA% and %LOCALAPPDATA%) are never touched.

#define MyVersion GetEnv("JIZZCRAFT_VERSION")
#if MyVersion == ""
  #define MyVersion "0.2.0"
#endif

[Setup]
AppId={{B6B49E3D-3F6F-4B7C-9A55-5C0E2D3A7C11}
AppName=JizzCraft
AppVersion={#MyVersion}
AppPublisher=JizzCraft contributors
DefaultDirName={localappdata}\Programs\JizzCraft
DefaultGroupName=JizzCraft
PrivilegesRequired=lowest
OutputDir=..\..\dist
OutputBaseFilename=JizzCraft-{#MyVersion}-windows-x86_64-setup
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
DisableProgramGroupPage=yes
SetupIconFile=..\..\assets\icons\jizzcraft.ico
UninstallDisplayIcon={app}\jizzcraft.exe

[Files]
Source: "..\..\dist\JizzCraft\*"; DestDir: "{app}"; Flags: recursesubdirs ignoreversion

[Icons]
Name: "{autoprograms}\JizzCraft"; Filename: "{app}\jizzcraft.exe"
; A neutral entry that starts the same program disguised (name, icon, ID).
Name: "{autoprograms}\Viewer"; Filename: "{app}\jizzcraft.exe"; Parameters: "--disguise"; IconFilename: "{app}\viewer.ico"; Tasks: viewer

[Tasks]
Name: "viewer"; Description: "Add a neutral ""Viewer"" shortcut"; Flags: unchecked

[Run]
Filename: "{app}\jizzcraft.exe"; Description: "Start JizzCraft"; Flags: nowait postinstall skipifsilent
