; Inno Setup script for the MeshFixer Windows installer.
;
; Built by .github/workflows/release.yml:
;   iscc /DAppVersion=0.1.2 /DSourceDir=<packaged folder> /DOutputDir=dist packaging\windows\meshfixer.iss
; <packaged folder> is the same folder the portable zip is made from:
; MeshFixer.exe, cli\meshfixer.exe, README.md, the licences and notices.
;
; The installer is per-user by default (no admin prompt). It adds Start menu
; and optional desktop shortcuts, an uninstaller, and puts MeshFixer in
; Explorer's "Open with" list for .stl, .obj and .3mf without taking over
; whatever app already opens those files.

#ifndef AppVersion
  #error Pass /DAppVersion=<version>
#endif
#ifndef SourceDir
  #error Pass /DSourceDir=<folder with MeshFixer.exe>
#endif
#ifndef OutputDir
  #define OutputDir "."
#endif

#define AppName "MeshFixer"
#define AppExe "MeshFixer.exe"
#define ProgId "MeshFixer.Mesh"

[Setup]
; Never change AppId: Windows uses it to find the installed copy on upgrade
; and uninstall.
AppId={{88E7F57B-78DD-436F-B6BF-AE7F12ED786F}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=The Vger Group
AppPublisherURL=https://github.com/thevgergroup/MeshFixer
AppSupportURL=https://github.com/thevgergroup/MeshFixer/issues
AppUpdatesURL=https://github.com/thevgergroup/MeshFixer/releases
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir={#OutputDir}
OutputBaseFilename=MeshFixer-{#AppVersion}-windows-x64-setup
SetupIconFile=..\..\crates\mf-desktop\assets\icon\MeshFixer.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; Wizard art from scripts/make-installer-art.sh; Inno picks the size that
; fits the display scale (100%, 150%, 200%).
WizardImageFile=wizard-164x314.bmp,wizard-246x459.bmp,wizard-328x604.bmp
WizardSmallImageFile=small-55x55.bmp,small-83x80.bmp,small-110x106.bmp
ChangesAssociations=yes
CloseApplications=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\cli\meshfixer.exe"; DestDir: "{app}\cli"; Flags: ignoreversion
Source: "{#SourceDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\THIRD_PARTY_NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{group}\{cm:UninstallProgram,{#AppName}}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; The ProgID MeshFixer opens files through. HKA is HKCU\Software\Classes
; for a per-user install and HKLM\Software\Classes for an all-users one.
Root: HKA; Subkey: "Software\Classes\{#ProgId}"; ValueType: string; ValueName: ""; ValueData: "3D mesh"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\{#ProgId}\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExe},0"
Root: HKA; Subkey: "Software\Classes\{#ProgId}\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""
; "Open with" entries only: the user's existing default app is left alone.
Root: HKA; Subkey: "Software\Classes\.stl\OpenWithProgids"; ValueType: string; ValueName: "{#ProgId}"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\.obj\OpenWithProgids"; ValueType: string; ValueName: "{#ProgId}"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\.3mf\OpenWithProgids"; ValueType: string; ValueName: "{#ProgId}"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes"; ValueType: string; ValueName: ".stl"; ValueData: ""; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes"; ValueType: string; ValueName: ".obj"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes"; ValueType: string; ValueName: ".3mf"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
