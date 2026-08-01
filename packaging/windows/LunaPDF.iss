#define MyAppName "LunaPDF"
#define MyAppPublisher "LunaPDF contributors"
#define MyAppUrl "https://github.com/takenoko9973/LunaPDF"
#define MyAppExeName "LunaPDF.exe"
#define MyAppVersion GetEnv("LUNAPDF_VERSION")
#define MyPayloadDir GetEnv("LUNAPDF_PAYLOAD_DIR")
#define MyOutputDir GetEnv("LUNAPDF_OUTPUT_DIR")

#if MyAppVersion == ""
  #error LUNAPDF_VERSION is required
#endif
#if MyPayloadDir == ""
  #error LUNAPDF_PAYLOAD_DIR is required
#endif
#if MyOutputDir == ""
  #error LUNAPDF_OUTPUT_DIR is required
#endif

[Setup]
AppId={{B18E4933-D40B-4AD5-AEBC-EDB9CE1FE806}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppUrl}
AppSupportURL={#MyAppUrl}/issues
AppUpdatesURL={#MyAppUrl}/releases
DefaultDirName={localappdata}\Programs\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#MyOutputDir}
OutputBaseFilename=LunaPDF-Setup-{#MyAppVersion}-x64
SetupIconFile=..\..\assets\windows\lunapdf.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ChangesAssociations=yes
SetupLogging=yes
VersionInfoVersion={#MyAppVersion}
VersionInfoCompany={#MyAppPublisher}
VersionInfoDescription={#MyAppName} per-user installer
VersionInfoProductName={#MyAppName}
VersionInfoProductVersion={#MyAppVersion}

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#MyPayloadDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; WorkingDir: "{app}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; WorkingDir: "{app}"; Tasks: desktopicon

[Registry]
; LunaPDF専用キーだけをアンインストール時に削除し、.pdfやUserChoiceの所有状態は変更しない。
Root: HKCU; Subkey: "Software\Classes\LunaPDF.Document.1"; ValueType: string; ValueData: "PDF document"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\LunaPDF.Document.1\DefaultIcon"; ValueType: string; ValueData: "{app}\{#MyAppExeName},0"
Root: HKCU; Subkey: "Software\Classes\LunaPDF.Document.1\shell\open"; ValueType: string; ValueName: "MultiSelectModel"; ValueData: "Player"
Root: HKCU; Subkey: "Software\Classes\LunaPDF.Document.1\shell\open\command"; ValueType: string; ValueData: """{app}\{#MyAppExeName}"" ""%1"" %*"

Root: HKCU; Subkey: "Software\Classes\Applications\LunaPDF.exe"; ValueType: string; ValueName: "FriendlyAppName"; ValueData: "{#MyAppName}"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Applications\LunaPDF.exe\DefaultIcon"; ValueType: string; ValueData: "{app}\{#MyAppExeName},0"
Root: HKCU; Subkey: "Software\Classes\Applications\LunaPDF.exe\shell\open"; ValueType: string; ValueName: "MultiSelectModel"; ValueData: "Player"
Root: HKCU; Subkey: "Software\Classes\Applications\LunaPDF.exe\shell\open\command"; ValueType: string; ValueData: """{app}\{#MyAppExeName}"" ""%1"" %*"
Root: HKCU; Subkey: "Software\Classes\Applications\LunaPDF.exe\SupportedTypes"; ValueType: string; ValueName: ".pdf"; ValueData: ""

; Open With候補値だけを消し、Windows共有の.pdfキーは残す。
Root: HKCU; Subkey: "Software\Classes\.pdf\OpenWithProgids"; ValueType: string; ValueName: "LunaPDF.Document.1"; ValueData: ""; Flags: uninsdeletevalue

; 子のCapabilitiesを先に消した後、LunaPDF専用の空の親キーだけを片付ける。
Root: HKCU; Subkey: "Software\LunaPDF"; Flags: uninsdeletekeyifempty
Root: HKCU; Subkey: "Software\LunaPDF\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "{#MyAppName}"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\LunaPDF\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "A lightweight desktop PDF viewer"
Root: HKCU; Subkey: "Software\LunaPDF\Capabilities"; ValueType: string; ValueName: "ApplicationIcon"; ValueData: "{app}\{#MyAppExeName},0"
Root: HKCU; Subkey: "Software\LunaPDF\Capabilities\FileAssociations"; ValueType: string; ValueName: ".pdf"; ValueData: "LunaPDF.Document.1"
Root: HKCU; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "LunaPDF"; ValueData: "Software\LunaPDF\Capabilities"; Flags: uninsdeletevalue

[UninstallDelete]
Type: filesandordirs; Name: "{userappdata}\LunaPDF"; Check: ShouldDeleteUserData

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; WorkingDir: "{app}"; Flags: nowait postinstall skipifsilent

[Code]

var
  DeleteUserData: Boolean;

function InitializeUninstall: Boolean;
begin
  { サイレントアンインストールでは設定を保持する }
  if UninstallSilent then
  begin
    DeleteUserData := False;
  end
  else
  begin
    DeleteUserData :=
      MsgBox(
        'LunaPDF の設定とセッションデータも削除しますか？' + #13#10 + #13#10 +
        '開いていたタブ、表示状態、注釈色履歴などが削除されます。' + #13#10 +
        'この操作は元に戻せません。',
        mbConfirmation,
        MB_YESNO or MB_DEFBUTTON2
      ) = IDYES;
  end;

  Result := True;
end;

function ShouldDeleteUserData: Boolean;
begin
  Result := DeleteUserData;
end;
