; Installer for Psycho Waluigi HD. Build it with build_installer.ps1, which compiles the tools
; into stage\ and then runs ISCC on this script.
;
; The installer ships only this project's own programs: the mmfs2.dll proxy, the HD viewer, and
; the build tools (hd_setup.exe, cleanedge.exe). It asks for the player's own psychowaluigi.exe;
; when Install is clicked, hd_setup.exe unpacks the game's runtime from it, extracts the sprites
; and makes the 9x HD texture sets (xBRZ and cleanEdge), all into the install folder. Then the
; files are copied and the shortcuts made.

#define AppName "Psycho Waluigi HD"
#ifndef AppVersion
  #define AppVersion "1.0"
#endif
#define Stage "stage"

[Setup]
AppId={{A6A6F34E-A48F-4630-A3EB-189C59A3DA55}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
; Per-user install: the game writes saves, logs and controls.cfg next to itself.
PrivilegesRequired=lowest
DefaultDirName={autopf}\{#AppName}
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; The generated textures: sprites 18 MB, xBRZ 9x ~600 MB, cleanEdge 9x ~460 MB, game copy 30 MB.
ExtraDiskSpaceRequired=1200000000
WizardStyle=modern
Compression=lzma2
SolidCompression=yes
OutputDir=output
OutputBaseFilename=PsychoWaluigiHD-Setup
UninstallDisplayName={#AppName}
UninstallDisplayIcon={app}\psychowaluigi.exe

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "{#Stage}\mmfs2.dll"; DestDir: "{app}\hd_runtime"; Flags: ignoreversion
Source: "{#Stage}\hd_view.exe"; DestDir: "{app}\hd_runtime"; Flags: ignoreversion
Source: "{#Stage}\README.txt"; DestDir: "{app}"; Flags: ignoreversion isreadme
Source: "{#Stage}\licenses\*"; DestDir: "{app}\licenses"; Flags: ignoreversion
; Only needed while installing; extracted to {tmp}.
Source: "{#Stage}\hd_setup.exe"; Flags: dontcopy
Source: "{#Stage}\cleanedge.exe"; Flags: dontcopy

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\hd_runtime\hd_view.exe"; WorkingDir: "{app}\hd_runtime"; IconFilename: "{app}\psychowaluigi.exe"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\hd_runtime\hd_view.exe"; WorkingDir: "{app}\hd_runtime"; IconFilename: "{app}\psychowaluigi.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\hd_runtime\hd_view.exe"; WorkingDir: "{app}\hd_runtime"; Description: "Play {#AppName}"; Flags: postinstall nowait skipifsilent

[UninstallDelete]
; Everything hd_setup.exe and the game made. Saves (walusave*) and controls.cfg are kept.
Type: files; Name: "{app}\psychowaluigi.exe"
Type: filesandordirs; Name: "{app}\sprites"
Type: filesandordirs; Name: "{app}\sprites_xbrz9x_direct"
Type: filesandordirs; Name: "{app}\sprites_cleanedge9x"
Type: files; Name: "{app}\hd_runtime\stdrt.exe"
Type: files; Name: "{app}\hd_runtime\mmfs2_orig.dll"
Type: files; Name: "{app}\hd_runtime\CCTrans.dll"
Type: files; Name: "{app}\hd_runtime\*.mfx"
Type: files; Name: "{app}\hd_runtime\*.sft"
Type: files; Name: "{app}\hd_runtime\image_keys.tsv"
Type: files; Name: "{app}\hd_runtime\image_masks.bin"
Type: files; Name: "{app}\hd_runtime\*.log"
Type: filesandordirs; Name: "{app}\hd_runtime\capture"
Type: filesandordirs; Name: "{app}\hd_runtime\shots"
Type: dirifempty; Name: "{app}\hd_runtime"

[Code]
var
  GamePage: TInputFileWizardPage;
  BuildPage: TOutputProgressWizardPage;
  ToolError: String;

procedure InitializeWizard;
begin
  GamePage := CreateInputFilePage(wpSelectDir,
    'Original game', 'Where is your copy of Psycho Waluigi?',
    'Psycho Waluigi HD is built from your own copy of the game. Select psychowaluigi.exe, then click Next.' + #13#10#13#10 +
    'It is copied into the install folder; the original is not changed.');
  GamePage.Add('&psychowaluigi.exe:', 'Psycho Waluigi (psychowaluigi.exe)|psychowaluigi.exe|Programs (*.exe)|*.exe', '.exe');
  GamePage.Values[0] := ExpandConstant('{param:GAME|' + GetPreviousData('GameExe', '') + '}');
  BuildPage := CreateOutputProgressPage('Building the HD version',
    'Unpacking the game and upscaling its textures. This takes a few minutes.');
end;

procedure RegisterPreviousData(PreviousDataKey: Integer);
begin
  SetPreviousData(PreviousDataKey, 'GameExe', GamePage.Values[0]);
end;

{ hd_setup.exe prints "@<percent>|<text>" for progress and "error: <message>" on failure. }
procedure ToolOutput(const S: String; const Error, FirstLine: Boolean);
var
  Bar: Integer;
  Pct: Integer;
begin
  Log('hd_setup: ' + S);
  if Copy(S, 1, 7) = 'error: ' then
    ToolError := Copy(S, 8, Length(S))
  else if Copy(S, 1, 1) = '@' then begin
    Bar := Pos('|', S);
    if Bar > 0 then begin
      Pct := StrToIntDef(Copy(S, 2, Bar - 2), -1);
      if Pct >= 0 then
        BuildPage.SetProgress(Pct, 100);
      BuildPage.SetText(Copy(S, Bar + 1, Length(S)), '');
    end;
  end;
end;

function RunTool(const Params: String): Boolean;
var
  Code: Integer;
begin
  ToolError := '';
  Result := ExecAndLogOutput(ExpandConstant('{tmp}\hd_setup.exe'), Params, ExpandConstant('{tmp}'),
    SW_HIDE, ewWaitUntilTerminated, Code, @ToolOutput) and (Code = 0);
  if not Result and (ToolError = '') then
    ToolError := 'hd_setup.exe failed (exit code ' + IntToStr(Code) + ').';
end;

function NextButtonClick(CurPageID: Integer): Boolean;
begin
  Result := True;
  if CurPageID = GamePage.ID then begin
    if not FileExists(GamePage.Values[0]) then begin
      MsgBox('Select your psychowaluigi.exe.', mbError, MB_OK);
      Result := False;
      exit;
    end;
    ExtractTemporaryFile('hd_setup.exe');
    if not RunTool('--check --game "' + GamePage.Values[0] + '"') then begin
      MsgBox('This file can''t be used:' + #13#10#13#10 + ToolError, mbError, MB_OK);
      Result := False;
    end;
  end;
end;

{ Build before copying anything, so a failure leaves nothing installed. Runs in silent installs
  too (/GAME="...\psychowaluigi.exe"). }
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Ok: Boolean;
begin
  Result := '';
  ExtractTemporaryFile('hd_setup.exe');
  ExtractTemporaryFile('cleanedge.exe');
  BuildPage.SetProgress(0, 100);
  BuildPage.Show;
  try
    Ok := ForceDirectories(ExpandConstant('{app}')) and
      RunTool('--game "' + GamePage.Values[0] + '" --dest "' + ExpandConstant('{app}') + '"');
  finally
    BuildPage.Hide;
  end;
  if not Ok then
    Result := 'Building the HD version failed:' + #13#10#13#10 + ToolError;
end;

{ Bring over existing saves from the original game's folder. }
procedure CurStepChanged(CurStep: TSetupStep);
var
  Src: String;
  I: Integer;
begin
  if CurStep = ssPostInstall then begin
    Src := ExtractFilePath(GamePage.Values[0]);
    for I := 1 to 3 do
      if FileExists(Src + 'walusave' + IntToStr(I)) and not FileExists(ExpandConstant('{app}\walusave' + IntToStr(I))) then
        FileCopy(Src + 'walusave' + IntToStr(I), ExpandConstant('{app}\walusave' + IntToStr(I)), True);
  end;
end;

function UpdateReadyMemo(Space, NewLine, MemoUserInfoInfo, MemoDirInfo, MemoTypeInfo,
  MemoComponentsInfo, MemoGroupInfo, MemoTasksInfo: String): String;
begin
  Result := MemoDirInfo + NewLine + NewLine +
    'Original game:' + NewLine + Space + GamePage.Values[0] + NewLine + NewLine +
    'Clicking Install builds the HD textures first (a few minutes, about 1.1 GB).';
  if MemoTasksInfo <> '' then
    Result := Result + NewLine + NewLine + MemoTasksInfo;
end;
