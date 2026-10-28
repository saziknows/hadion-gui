; gitvis installer (Inno Setup 6): a per-user install, no admin rights.
;
;   Installs to   %LOCALAPPDATA%\Programs\gitvis
;   Shortcut      Start menu (this user), optional desktop shortcut
;   Uninstaller   Settings > Apps, and the Start menu folder
;
; Built by scripts/package.ps1 after it staged dist\gitvis-<version>\:
;   ISCC /DAppVersion=0.2.0 /DSourceDir=..\..\dist\gitvis-0.2.0 /DOutputDir=..\..\dist packaging\windows\gitvis.iss
;
; The folder is writable by the user, so gitvis can update itself in place
; (it stages gitvis.exe.new beside gitvis.exe and swaps them on restart);
; the uninstaller removes those leftovers too. Settings live in
; %APPDATA%\gitvis and crash reports in %LOCALAPPDATA%\gitvis: they are kept,
; like the user's repositories.

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\dist\gitvis-" + AppVersion
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
AppName=gitvis
AppVersion={#AppVersion}
AppVerName=gitvis {#AppVersion}
AppPublisher=gitvis
VersionInfoVersion={#AppVersion}
DefaultDirName={localappdata}\Programs\gitvis
DefaultGroupName=gitvis
DisableProgramGroupPage=yes
DisableDirPage=auto
; Per user: no UAC prompt, no machine-wide changes.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=gitvis-{#AppVersion}-{#Platform}-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
UninstallDisplayName=gitvis
UninstallDisplayIcon={app}\gitvis.exe
; gitvis may be running during an upgrade: ask to close it.
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\gitvis.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\LICENSE*"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#SourceDir}\licenses\*"; DestDir: "{app}\licenses"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\gitvis"; Filename: "{app}\gitvis.exe"; Comment: "Git client with branch visualisation"
Name: "{userprograms}\Uninstall gitvis"; Filename: "{uninstallexe}"
Name: "{userdesktop}\gitvis"; Filename: "{app}\gitvis.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\gitvis.exe"; Description: "{cm:LaunchProgram,gitvis}"; Flags: nowait postinstall skipifsilent

[InstallDelete]
; A self-update that was staged but never applied is out of date now.
Type: files; Name: "{app}\gitvis.exe.new"
Type: files; Name: "{app}\gitvis.exe.new.part"
Type: files; Name: "{app}\gitvis.exe.old"

[UninstallDelete]
; Files the updater creates beside the executable.
Type: files; Name: "{app}\gitvis.exe.new"
Type: files; Name: "{app}\gitvis.exe.new.part"
Type: files; Name: "{app}\gitvis.exe.old"
Type: dirifempty; Name: "{app}"
