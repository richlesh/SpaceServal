; SpaceServal — Inno Setup installer
; -----------------------------------------------------------------------------
; Builds a single Setup.exe that installs the GUI app to Program Files and
; creates Start Menu (and optional desktop) shortcuts. A matching uninstaller
; reverses all of it.
;
; Invoked by CI as:
;   iscc /DAppVersion=X.Y.Z /DArch=<x64|arm64> /DSourceDir=<dir> spaceserval.iss
; where <dir> contains spaceserval-gui.exe.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef Arch
  #define Arch "x64"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\target\release"
#endif

#define AppName "SpaceServal"
#define AppPublisher "Glowing Cat Software"
#define AppURL "https://glowingcat.com/SpaceServal.html"
#define GuiExe "spaceserval-gui.exe"

[Setup]
AppId={{B2F7A91C-3E4D-4A8B-9C1E-7D5F2A6B8C04}}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
; Per-user install by default (no admin needed); the user may elect an
; all-users install, which elevates.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
DisableProgramGroupPage=yes
LicenseFile=..\..\LICENSE
SetupIconFile=..\..\gui\src\resources\app_icon.ico
UninstallDisplayIcon={app}\{#GuiExe}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
OutputDir=.
OutputBaseFilename=SpaceServal-{#Arch}-Setup
#if Arch == "arm64"
ArchitecturesAllowed=arm64
ArchitecturesInstallIn64BitMode=arm64
#else
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
#endif

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; \
  GroupDescription: "Additional shortcuts:"; Flags: unchecked

[Files]
Source: "{#SourceDir}\{#GuiExe}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#GuiExe}"
Name: "{group}\Uninstall {#AppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#GuiExe}"; Tasks: desktopicon

[Run]
Description: "Launch {#AppName}"; Filename: "{app}\{#GuiExe}"; \
  Flags: nowait postinstall skipifsilent
