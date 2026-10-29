; Hadion installer (Inno Setup 6): a per-user install, no admin rights.
;
;   Installs to   %LOCALAPPDATA%\Programs\hadion
;   Shortcut      Start menu (this user), optional desktop shortcut
;   Uninstaller   Settings > Apps, and the Start menu folder
;
; Built by scripts/package.ps1 after it staged dist\hadion-<version>\:
;   ISCC /DAppVersion=0.2.0 /DSourceDir=..\..\dist\hadion-0.2.0 /DOutputDir=..\..\dist packaging\windows\hadion.iss
;
; The folder is writable by the user, so Hadion can update itself in place
; (it stages hadion.exe.new beside hadion.exe and swaps them on restart);
; the uninstaller removes those leftovers too. Settings live in
; %APPDATA%\hadion and crash reports in %LOCALAPPDATA%\hadion: they are kept,
; like the user's repositories.

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\dist\hadion-" + AppVersion
#endif
#ifndef OutputDir
  #define OutputDir "..\..\dist"
#endif
#ifndef Platform
  #define Platform "windows-x86_64"
#endif

[Setup]
; Never change the AppId: it is how upgrades find the previous install.
AppId={{40210AA5-54DE-4445-A3F3-CCB11C5BF2BA}
AppName=Hadion
AppVersion={#AppVersion}
AppVerName=Hadion {#AppVersion}
AppPublisher=Dylan Westraadt
AppPublisherURL=https://github.com/saziknows/hadion-gui
VersionInfoVersion={#AppVersion}
VersionInfoProductName=Hadion
DefaultDirName={localappdata}\Programs\hadion
DefaultGroupName=Hadion
DisableProgramGroupPage=yes
DisableDirPage=auto
; Per user: no UAC prompt, no machine-wide changes.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=hadion-{#AppVersion}-{#Platform}-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; The brand's Windows icon (docs/brand), also embedded in hadion.exe by build.rs.
SetupIconFile=..\..\crates\hadion\assets\brand\hadion.ico
UninstallDisplayName=Hadion
UninstallDisplayIcon={app}\hadion.exe
; Hadion may be running during an upgrade: ask to close it.
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\hadion.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\LICENSE*"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#SourceDir}\licenses\*"; DestDir: "{app}\licenses"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\Hadion"; Filename: "{app}\hadion.exe"; Comment: "Git client with branch visualisation"
Name: "{userprograms}\Uninstall Hadion"; Filename: "{uninstallexe}"
Name: "{userdesktop}\Hadion"; Filename: "{app}\hadion.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\hadion.exe"; Description: "{cm:LaunchProgram,Hadion}"; Flags: nowait postinstall skipifsilent

[InstallDelete]
; A self-update that was staged but never applied is out of date now.
Type: files; Name: "{app}\hadion.exe.new"
Type: files; Name: "{app}\hadion.exe.new.part"
Type: files; Name: "{app}\hadion.exe.old"

[UninstallDelete]
; Files the updater creates beside the executable.
Type: files; Name: "{app}\hadion.exe.new"
Type: files; Name: "{app}\hadion.exe.new.part"
Type: files; Name: "{app}\hadion.exe.old"
Type: dirifempty; Name: "{app}"
