#ifndef AppVersion
  #error AppVersion must be supplied by the release build
#endif
#ifndef BuildDir
  #define BuildDir "..\..\target\x86_64-pc-windows-msvc\release"
#endif

[Setup]
AppId={{A4875C2E-DAE3-4BE1-A517-8B2648D88F86}
AppName=Blobtorrent
AppVersion={#AppVersion}
AppPublisher=n0-computer
AppPublisherURL=https://github.com/n0-computer/blobtorrent
DefaultDirName={localappdata}\Programs\Blobtorrent
DefaultGroupName=Blobtorrent
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir=..\..\dist
OutputBaseFilename=blobtorrent-{#AppVersion}-windows-x64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\blobtorrent-gui.exe
CloseApplications=yes
RestartApplications=no
SetupLogging=yes
InfoBeforeFile=install-info.txt

[Files]
Source: "{#BuildDir}\blobtorrent.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BuildDir}\blobtorrent-background.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BuildDir}\blobtorrent-gui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BuildDir}\blobtorrent-tui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\blobtorrent-proto\UI.md"; DestDir: "{app}"; Flags: ignoreversion

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Blobtorrent"; ValueData: """{app}\blobtorrent-background.exe"""; Flags: uninsdeletevalue

[Icons]
Name: "{group}\Blobtorrent"; Filename: "{app}\blobtorrent-gui.exe"; WorkingDir: "{userprofile}"
Name: "{group}\Start background daemon"; Filename: "{app}\blobtorrent-background.exe"; WorkingDir: "{userprofile}"
Name: "{group}\Stop background daemon"; Filename: "{app}\blobtorrent-background.exe"; Parameters: "stop"; WorkingDir: "{userprofile}"
Name: "{group}\Uninstall Blobtorrent"; Filename: "{uninstallexe}"

[Run]
Filename: "{app}\blobtorrent-gui.exe"; Description: "Open Blobtorrent"; Flags: nowait postinstall skipifsilent

[Code]
function StopDaemon(): Boolean;
var
  Code: Integer;
  Helper: String;
begin
  Helper := ExpandConstant('{app}\blobtorrent-background.exe');
  Result := True;
  if FileExists(Helper) then
    Result := Exec(Helper, 'stop', '', SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  if not StopDaemon() then
    Result := 'The background daemon could not be stopped. Close it and retry. Details are in %LOCALAPPDATA%\blobtorrent\launcher.log.';
end;

function InitializeUninstall(): Boolean;
begin
  Result := StopDaemon();
  if not Result then
    SuppressibleMsgBox('The daemon could not be stopped. Close it and retry uninstalling. Your data has not been removed.', mbError, MB_OK, IDOK);
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Code: Integer;
begin
  if CurStep = ssPostInstall then
  begin
    if not Exec(ExpandConstant('{app}\blobtorrent-background.exe'), '--setup-gui', '', SW_HIDE, ewWaitUntilTerminated, Code) or (Code <> 0) then
      SuppressibleMsgBox('Blobtorrent is installed, but background setup failed. See %LOCALAPPDATA%\blobtorrent\launcher.log, then run blobtorrent-background.exe --setup-gui to retry.', mbError, MB_OK, IDOK);
  end;
end;
