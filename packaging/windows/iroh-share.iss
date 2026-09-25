#ifndef AppVersion
  #error AppVersion must be supplied by the release build
#endif
#ifndef BuildDir
  #define BuildDir "..\..\target\x86_64-pc-windows-msvc\release"
#endif

[Setup]
AppId={{A4875C2E-DAE3-4BE1-A517-8B2648D88F86}
AppName=Iroh Share
AppVersion={#AppVersion}
AppPublisher=n0-computer
AppPublisherURL=https://github.com/n0-computer/iroh-share
DefaultDirName={localappdata}\Programs\Iroh Share
DefaultGroupName=Iroh Share
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir=..\..\dist
OutputBaseFilename=iroh-share-{#AppVersion}-windows-x64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\iroh-share-gui.exe
CloseApplications=yes
RestartApplications=no
SetupLogging=yes
InfoBeforeFile=install-info.txt

[Files]
Source: "{#BuildDir}\iroh-share.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BuildDir}\iroh-share-background.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BuildDir}\iroh-share-gui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BuildDir}\iroh-share-tui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\iroh-share-proto\UI.md"; DestDir: "{app}"; Flags: ignoreversion

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Iroh Share"; ValueData: """{app}\iroh-share-background.exe"""; Flags: uninsdeletevalue

[Icons]
Name: "{group}\Iroh Share"; Filename: "{app}\iroh-share-gui.exe"; WorkingDir: "{app}"
Name: "{group}\Start background daemon"; Filename: "{app}\iroh-share-background.exe"; WorkingDir: "{app}"
Name: "{group}\Stop background daemon"; Filename: "{app}\iroh-share-background.exe"; Parameters: "stop"; WorkingDir: "{app}"
Name: "{group}\Uninstall Iroh Share"; Filename: "{uninstallexe}"

[Run]
Filename: "{app}\iroh-share-gui.exe"; Description: "Open Iroh Share"; Flags: nowait postinstall skipifsilent

[Code]
function StopDaemon(): Boolean;
var
  Code: Integer;
  Helper: String;
begin
  Helper := ExpandConstant('{app}\iroh-share-background.exe');
  Result := True;
  if FileExists(Helper) then
    Result := Exec(Helper, 'stop', '', SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  if not StopDaemon() then
    Result := 'The background daemon could not be stopped. Close it and retry. Details are in %LOCALAPPDATA%\iroh-share\launcher.log.';
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
    if not Exec(ExpandConstant('{app}\iroh-share-background.exe'), '--setup-gui', '', SW_HIDE, ewWaitUntilTerminated, Code) or (Code <> 0) then
      SuppressibleMsgBox('Iroh Share is installed, but background setup failed. See %LOCALAPPDATA%\iroh-share\launcher.log, then run iroh-share-background.exe --setup-gui to retry.', mbError, MB_OK, IDOK);
  end;
end;
