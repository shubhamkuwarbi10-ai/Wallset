; wallSet installer (Inno Setup 6). Build with tools\build-installer.ps1,
; which stages the release binaries into dist\wallSet first.
;
; Per-user install: no administrator rights, files go to
; %LOCALAPPDATA%\Programs\wallSet, matching the per-user service and the
; per-user autostart entry the service manages itself.

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#define Stage "..\dist\wallSet"

[Setup]
AppId={{43508B77-3F4D-43B7-9F0C-86C1C1D1503B}
AppName=wallSet
AppVersion={#AppVersion}
AppVerName=wallSet {#AppVersion}
AppPublisher=Shubhamkuwarbi10-ai
AppPublisherURL=https://github.com/shubhamkuwarbi10-ai/Wallset
AppSupportURL=https://github.com/shubhamkuwarbi10-ai/Wallset/issues
PrivilegesRequired=lowest
DefaultDirName={autopf}\wallSet
DisableProgramGroupPage=yes
DisableDirPage=auto
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir=..\dist
OutputBaseFilename=wallSet-Setup-{#AppVersion}
SetupIconFile=wallSet.ico
UninstallDisplayIcon={app}\wallSet.ico
UninstallDisplayName=wallSet
WizardStyle=modern
Compression=lzma2/ultra64
SolidCompression=yes
; The service and renderer are stopped in code below before files change.
CloseApplications=no

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; Flags: unchecked

[Files]
Source: "{#Stage}\wallset-ui.exe";     DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\wallset-daemon.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\wallset-render.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\wallset.exe";        DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\libmpv-2.dll";       DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\LIBMPV-SOURCE.txt";  DestDir: "{app}"; Flags: ignoreversion
Source: "wallSet.ico";                 DestDir: "{app}"; Flags: ignoreversion
Source: "THIRD-PARTY-NOTICES.txt";     DestDir: "{app}"; Flags: ignoreversion
Source: "GPL-2.0.txt";                 DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\wallSet"; Filename: "{app}\wallset-ui.exe"; IconFilename: "{app}\wallSet.ico"; Comment: "Image and video wallpapers"
Name: "{autodesktop}\wallSet"; Filename: "{app}\wallset-ui.exe"; IconFilename: "{app}\wallSet.ico"; Tasks: desktopicon

[Registry]
; The service writes this autostart value itself when "Start with Windows" is
; on. Never written here (ValueType: none); only removed on uninstall.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "wallSet"; ValueType: none; Flags: uninsdeletevalue

[Run]
; An upgrade stopped the service; bring it back if wallSet was already set up.
Filename: "{app}\wallset-daemon.exe"; Flags: nowait runhidden; Check: HasSettings
Filename: "{app}\wallset-ui.exe"; Description: "Open wallSet"; Flags: nowait postinstall skipifsilent

[Code]
const
  WM_CLOSE = $0010;

{ Ask the service to stop (it stops its renderers too), close any settings
  window, then make sure nothing from this folder is still running. }
procedure StopWallSet();
var
  Wnd: HWND;
  I, Code: Integer;
begin
  Wnd := FindWindowByClassName('wallSet.Daemon');
  if Wnd <> 0 then
    PostMessage(Wnd, WM_CLOSE, 0, 0);
  for I := 1 to 20 do
  begin
    if FindWindowByClassName('wallSet.Daemon') = 0 then Break;
    Sleep(250);
  end;
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM wallset-ui.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM wallset-render.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM wallset-daemon.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

function HasSettings(): Boolean;
begin
  Result := FileExists(ExpandConstant('{userappdata}\wallSet\config.json'));
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  StopWallSet();
  Result := '';
end;

function InitializeUninstall(): Boolean;
begin
  StopWallSet();
  Result := True;
end;

{ Settings and logs live outside the program folder. Keep them unless the
  person asks otherwise; a silent uninstall always keeps them. }
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if (CurUninstallStep = usPostUninstall) and not UninstallSilent() then
    if MsgBox('Also remove your wallSet settings, logs and downloaded wallpapers?' + #13#10 +
              'Choose No to keep them for a later reinstall.',
              mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
    begin
      DelTree(ExpandConstant('{userappdata}\wallSet'), True, True, True);
      DelTree(ExpandConstant('{localappdata}\wallSet'), True, True, True);
    end;
end;
