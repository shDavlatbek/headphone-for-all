; SPDX-License-Identifier: MIT OR Apache-2.0
;
; Inno Setup script for the Headphone for All Windows installer.
;
; Build with packaging/windows/build-installer.ps1 (CI), or by hand:
;   ISCC.exe /DAppVersion=1.0.0 /DSourceDir=<release dir> packaging\windows\hfa.iss
; SourceDir defaults to the Flutter release output
; app\build\windows\x64\runner\Release (the script adds the MSVC runtime DLLs
; to a staged copy of it). Requires Inno Setup 6.3 or newer.
;
; Names shared with the runner (app/windows/runner/app_identity.h):
;   AppMutex  = the single-instance mutex, so Setup / Uninstall ask to close a
;               running app instead of failing on locked files;
;   AppUserModelID of the shortcuts = the runner's explicit AppUserModelID.

#ifndef AppVersion
  #define AppVersion "1.0.0"
#endif
#ifndef AppArch
  #define AppArch "x64"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\app\build\windows\" + AppArch + "\runner\Release"
#endif
#ifndef OutputDir
  #define OutputDir "..\dist"
#endif

#define AppName "Headphone for All"
#define AppExeName "headphone_for_all.exe"
#define AppUserModelId "io.github.shdavlatbek.hfa"
#define AppMutexName "io.github.shdavlatbek.hfa.SingleInstance"
#define AppPublisher "headphone-for-all contributors"
#define AppUrl "https://github.com/shDavlatbek/headphone-for-all"
; %APPDATA%\<CompanyName>\<ProductName> of the exe's version resource
; (path_provider), see app/windows/runner/Runner.rc.
#define AppDataSubdir "io.github.shdavlatbek\Headphone for All"

#if AppArch == "arm64"
  #define ArchAllowed "arm64"
#else
  #define ArchAllowed "x64compatible"
#endif

[Setup]
; Never change AppId: it identifies the installation for upgrades.
AppId={{85BFE6C3-CCD5-4475-AE3E-AA081E26A283}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}/issues
AppUpdatesURL={#AppUrl}/releases
AppMutex={#AppMutexName}
CloseApplications=yes
RestartApplications=no
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
; Per-user install by default (no UAC prompt); the dialog offers an
; all-users install, which also enables the firewall task.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed={#ArchAllowed}
ArchitecturesInstallIn64BitMode={#ArchAllowed}
; Windows 10 2004: the first version with process-loopback capture.
MinVersion=10.0.19041
OutputDir={#OutputDir}
OutputBaseFilename=Headphone_for_All-{#AppVersion}-windows-{#AppArch}-setup
SetupIconFile=..\..\app\windows\runner\resources\app_icon.ico
UninstallDisplayIcon={app}\{#AppExeName}
UninstallDisplayName={#AppName}
VersionInfoVersion={#AppVersion}
VersionInfoProductName={#AppName}
VersionInfoCompany={#AppPublisher}
VersionInfoDescription={#AppName} Setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "autostart"; Description: "Start {#AppName} when I sign in"; GroupDescription: "Other:"; Flags: unchecked
Name: "firewall"; Description: "Allow {#AppName} through Windows Firewall on private networks (needed to be a hub)"; GroupDescription: "Other:"; Check: IsAdminInstallMode

[InstallDelete]
; Assets of an older version must not linger next to the new ones.
Type: filesandordirs; Name: "{app}\data"

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "..\..\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExeName}"; AppUserModelID: "{#AppUserModelId}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExeName}"; AppUserModelID: "{#AppUserModelId}"; Tasks: desktopicon
Name: "{autostartup}\{#AppName}"; Filename: "{app}\{#AppExeName}"; AppUserModelID: "{#AppUserModelId}"; Tasks: autostart

[Run]
; Replace (not duplicate) the rule on upgrades.
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#AppName}"""; Flags: runhidden; Tasks: firewall
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""{#AppName}"" dir=in action=allow program=""{app}\{#AppExeName}"" enable=yes profile=private"; Flags: runhidden; Tasks: firewall
Filename: "{app}\{#AppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(AppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#AppName}"""; Flags: runhidden; RunOnceId: "DeleteFirewallRule"; Check: IsAdminInstallMode

[Code]
{ Settings, the device identity and the pairings live in the user's roaming
  AppData. Keep them by default (a reinstall keeps its pairings); an
  interactive uninstall offers to delete them. }
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  DataDir: String;
begin
  if CurUninstallStep <> usPostUninstall then
    Exit;
  DataDir := ExpandConstant('{userappdata}\{#AppDataSubdir}');
  if UninstallSilent or not DirExists(DataDir) then
    Exit;
  if MsgBox('Also delete the settings, device identity and pairings of {#AppName}?' + #13#10#13#10 + DataDir,
            mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
  begin
    DelTree(DataDir, True, True, True);
    { Removes the company folder only when it is empty. }
    RemoveDir(ExtractFileDir(DataDir));
  end;
end;
