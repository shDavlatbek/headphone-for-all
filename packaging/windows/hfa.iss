; SPDX-License-Identifier: MIT OR Apache-2.0
;
; Inno Setup script for the Headphone for All Windows installer.
;
; Build with packaging/windows/build-installer.ps1 (CI), or by hand:
;   ISCC.exe /DAppVersion=0.1.0 /DSourceDir=<release dir> packaging\windows\hfa.iss
; (for a pre-release also /DAppVersionNumeric=0.1.0 with /DAppVersion=0.1.0-rc.1)
; SourceDir defaults to the Flutter release output
; app\build\windows\x64\runner\Release (the script adds the MSVC runtime DLLs
; to a staged copy of it). Requires Inno Setup 6.3 or newer.
;
; Names shared with the runner (app/windows/runner/app_identity.h):
;   AppMutex  = the single-instance mutex, so Setup / Uninstall ask to close a
;               running app instead of failing on locked files;
;   AppQuitMessage / AppWindowClass = the registered message the main window
;               handles by quitting at once, and that window's class: [Code]
;               uses them to quit a running app (which may be hidden in the
;               tray) before the AppMutex check;
;   AppAutostartArg = the argument that starts the app hidden in the tray;
;   AppUserModelID of the shortcuts = the runner's explicit AppUserModelID.

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
; Version resource of the setup .exe: numbers only, so a pre-release
; AppVersion (1.2.3-beta.1) needs the numeric part here (the build script
; passes it).
#ifndef AppVersionNumeric
  #define AppVersionNumeric AppVersion
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
#define AppQuitMessage "io.github.shdavlatbek.hfa.Quit"
#define AppWindowClass "io.github.shdavlatbek.hfa.MainWindow"
#define AppAutostartArg "--autostart"
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
VersionInfoVersion={#AppVersionNumeric}
VersionInfoProductName={#AppName}
VersionInfoCompany={#AppPublisher}
VersionInfoDescription={#AppName} Setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Messages]
; Shown when the app is still running after [Code] asked it to quit (for
; example an instance started "as administrator", which a per-user Setup may
; not message). Closing the window is not enough while the hub or a sender
; runs: it only hides the window to the tray.
SetupAppRunningError=Setup has detected that %1 is still running.%n%nQuit it with "Quit" in the menu of its icon in the notification area (closing the window only hides it there), then click OK to continue, or Cancel to exit.
UninstallAppRunningError=Uninstall has detected that %1 is still running.%n%nQuit it with "Quit" in the menu of its icon in the notification area (closing the window only hides it there), then click OK to continue, or Cancel to exit.

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
; The app starts hidden in the notification area; it does not start the hub by
; itself (that is a switch in the app), so the text does not promise more.
Name: "autostart"; Description: "Start {#AppName} in the notification area when I sign in (switch the hub on from there)"; GroupDescription: "Other:"; Flags: unchecked
; Private networks only, on purpose: a hub on a network marked Public (the
; default for a newly joined Wi-Fi) is not covered, see packaging/README.md.
Name: "firewall"; Description: "Allow {#AppName} through Windows Firewall on private networks (needed to be a hub; mark your network Private)"; GroupDescription: "Other:"; Check: IsAdminInstallMode

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
; Starts hidden in the tray (the runner skips showing the window).
Name: "{autostartup}\{#AppName}"; Filename: "{app}\{#AppExeName}"; Parameters: "{#AppAutostartArg}"; AppUserModelID: "{#AppUserModelId}"; Tasks: autostart

[Run]
; Replace (not duplicate) the rule on upgrades.
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#AppName}"""; Flags: runhidden; Tasks: firewall
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""{#AppName}"" dir=in action=allow program=""{app}\{#AppExeName}"" enable=yes profile=private"; Flags: runhidden; Tasks: firewall
Filename: "{app}\{#AppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(AppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#AppName}"""; Flags: runhidden; RunOnceId: "DeleteFirewallRule"; Check: IsAdminInstallMode

[Code]
const
  { How long to wait for a running app to exit after asking it to quit. }
  QuitTimeoutMs = 10000;
  QuitPollMs = 200;

{ Asks a running app (window visible or hidden in the tray) to quit and waits
  for its single-instance mutex to disappear. Returns whether it is gone. The
  runner quits on the registered quit message at once, stopping the hub and
  the sender; posting to an elevated instance fails (UIPI), and the AppMutex
  check that follows then asks the user to quit it from the tray. }
function QuitRunningApp(): Boolean;
var
  Wnd: HWND;
  Msg: Cardinal;
  Waited: Integer;
begin
  Result := not CheckForMutexes('{#AppMutexName}');
  if Result then
    Exit;
  Msg := RegisterWindowMessage('{#AppQuitMessage}');
  Wnd := FindWindowByClassName('{#AppWindowClass}');
  if (Msg = 0) or (Wnd = 0) or not PostMessage(Wnd, Msg, 0, 0) then
  begin
    Log('Could not ask the running app to quit.');
    Exit;
  end;
  Waited := 0;
  while CheckForMutexes('{#AppMutexName}') and (Waited < QuitTimeoutMs) do
  begin
    Sleep(QuitPollMs);
    Waited := Waited + QuitPollMs;
  end;
  Result := not CheckForMutexes('{#AppMutexName}');
  Log(Format('Running app quit: %d (waited %d ms).', [Ord(Result), Waited]));
end;

{ True when the app is not running, or it may be quit (silent runs and
  suppressed message boxes answer OK), which is then done. Cancel aborts. }
function OfferToQuitRunningApp(const Action: String): Boolean;
begin
  Result := True;
  if not CheckForMutexes('{#AppMutexName}') then
    Exit;
  if SuppressibleMsgBox('{#AppName} is running. ' + Action + ' will quit it now, which stops its hub and any stream it sends.' + #13#10#13#10 +
                        'Click OK to quit it and continue, or Cancel to exit.',
                        mbConfirmation, MB_OKCANCEL, IDOK) <> IDOK then
  begin
    Result := False;
    Exit;
  end;
  if not QuitRunningApp() then
    Log('The app is still running; Setup asks the user to quit it.');
end;

{ Runs before Setup's own AppMutex check (which remains the fallback). }
function InitializeSetup(): Boolean;
begin
  Result := OfferToQuitRunningApp('Setup');
end;

{ Settings, the device identity and the pairings live in the user's roaming
  AppData. Keep them by default (a reinstall keeps its pairings); an
  interactive uninstall offers to delete them. }
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  DataDir: String;
begin
  { Runs right before Uninstall's own AppMutex check, after the user
    confirmed the uninstall, so quit a running app without asking again. }
  if CurUninstallStep = usAppMutexCheck then
  begin
    if not QuitRunningApp() then
      Log('The app is still running; Uninstall asks the user to quit it.');
    Exit;
  end;
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
