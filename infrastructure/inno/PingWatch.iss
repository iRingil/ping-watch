; Installer of the release build: a normal per-user installation or a portable copy in any folder.

#define AppName "Ping Watch"
#define AppExeName "PingWatch.exe"
#define BuiltExe "..\..\target\release\ping-watch.exe"
#define Art "..\..\assets\installer\"
; The version comes from the executable's properties; Windows pads it to four numbers, the app's has three.
#define VerMajor
#define VerMinor
#define VerPatch
#define VerBuild
#expr GetVersionComponents(BuiltExe, VerMajor, VerMinor, VerPatch, VerBuild)
#define AppVersion Str(VerMajor) + "." + Str(VerMinor) + "." + Str(VerPatch)
#define WizardImages Art + "wizard-202x386.png," + Art + "wizard-269x515.png," + Art + "wizard-336x643.png," + Art + "wizard-403x772.png," + Art + "wizard-430x824.png," + Art + "wizard-498x953.png," + Art + "wizard-534x1022.png"
#define WizardSmallImages Art + "wizard-small-58.png," + Art + "wizard-small-77.png," + Art + "wizard-small-97.png," + Art + "wizard-small-116.png," + Art + "wizard-small-124.png," + Art + "wizard-small-143.png," + Art + "wizard-small-159.png"

[Setup]
AppId={{3B7C1E52-9D4A-4F0B-8E26-5A1C7D9E2F48}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=iRingil
AppPublisherURL=https://github.com/iRingil/ping-watch
AppSupportURL=https://github.com/iRingil/ping-watch/issues
AppCopyright=Copyright (c) 2026 iRingil
VersionInfoVersion={#AppVersion}
; Per-user, no administrator rights: %LOCALAPPDATA%\Programs\Ping Watch.
PrivilegesRequired=lowest
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; A portable copy has no uninstaller entry; the app itself still writes its Run value (when turned on) and its
; notification registration, as the README says.
Uninstallable=not IsPortable
UninstallDisplayIcon={app}\{#AppExeName}
UninstallDisplayName={#AppName}
SetupIconFile=..\..\assets\icon.ico
LicenseFile=..\..\LICENSE.md
OutputDir=..\..\build
OutputBaseFilename=PingWatch-{#AppVersion}-setup
Compression=lzma2/max
SolidCompression=yes
; A running Ping Watch is closed before its files are replaced.
CloseApplications=yes
; Light or dark as Windows is set, like the program itself.
WizardStyle=modern dynamic
DisableWelcomePage=no
; One file per display scale; Setup picks the one closest to the image area.
WizardImageFile={#WizardImages}
WizardImageFileDynamicDark={#WizardImages}
WizardSmallImageFile={#WizardSmallImages}
WizardSmallImageFileDynamicDark={#WizardSmallImages}
ShowLanguageDialog=auto

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "ru"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "de"; MessagesFile: "compiler:Languages\German.isl"
Name: "fr"; MessagesFile: "compiler:Languages\French.isl"
Name: "es"; MessagesFile: "compiler:Languages\Spanish.isl"

[CustomMessages]
en.ModeCaption=Installation type
en.ModeDescription=How should {#AppName} be installed?
en.ModeNormal=Normal: for this Windows user, with a Start menu shortcut and an uninstaller
en.ModePortable=Portable: into any folder (a USB stick, for example); settings and history stay in that folder
en.DeleteUserData=Delete the settings and the history of {#AppName} as well?
ru.ModeCaption=Тип установки
ru.ModeDescription=Как установить {#AppName}?
ru.ModeNormal=Обычная: для этого пользователя Windows, с ярлыком в меню «Пуск» и деинсталлятором
ru.ModePortable=Портативная: в любую папку (например, на флешку); настройки и история остаются в этой папке
ru.DeleteUserData=Удалить также настройки и историю {#AppName}?
de.ModeCaption=Installationsart
de.ModeDescription=Wie soll {#AppName} installiert werden?
de.ModeNormal=Normal: für diesen Windows-Benutzer, mit Startmenü-Verknüpfung und Deinstallationsprogramm
de.ModePortable=Portabel: in einen beliebigen Ordner (z. B. einen USB-Stick); Einstellungen und Verlauf bleiben dort
de.DeleteUserData=Auch die Einstellungen und den Verlauf von {#AppName} löschen?
fr.ModeCaption=Type d'installation
fr.ModeDescription=Comment installer {#AppName} ?
fr.ModeNormal=Normale : pour cet utilisateur Windows, avec un raccourci dans le menu Démarrer et un programme de désinstallation
fr.ModePortable=Portable : dans n'importe quel dossier (une clé USB, par exemple) ; réglages et historique restent dans ce dossier
fr.DeleteUserData=Supprimer aussi les réglages et l'historique de {#AppName} ?
es.ModeCaption=Tipo de instalación
es.ModeDescription=¿Cómo instalar {#AppName}?
es.ModeNormal=Normal: para este usuario de Windows, con acceso directo en el menú Inicio y desinstalador
es.ModePortable=Portátil: en cualquier carpeta (una memoria USB, por ejemplo); la configuración y el historial se quedan en ella
es.DeleteUserData=¿Eliminar también la configuración y el historial de {#AppName}?

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked; Check: not IsPortable

[Files]
Source: "{#BuiltExe}"; DestDir: "{app}"; DestName: "{#AppExeName}"; Flags: ignoreversion
Source: "..\..\LICENSE.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "portable.txt"; DestDir: "{app}"; Flags: ignoreversion; Check: IsPortable

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExeName}"; Check: not IsPortable
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(AppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent

[Code]
var
  ModePage: TInputOptionWizardPage;

function IsPortable: Boolean;
begin
  Result := (ModePage <> nil) and ModePage.Values[1];
end;

procedure InitializeWizard;
begin
  ModePage := CreateInputOptionPage(wpLicense, CustomMessage('ModeCaption'), CustomMessage('ModeDescription'), '', True, False);
  ModePage.Add(CustomMessage('ModeNormal'));
  ModePage.Add(CustomMessage('ModePortable'));
  // /PORTABLE=1 on the command line chooses the portable type, also for a silent installation.
  ModePage.Values[1] := ExpandConstant('{param:portable|0}') = '1';
  ModePage.Values[0] := not ModePage.Values[1];
end;

function NextButtonClick(CurPageID: Integer): Boolean;
var
  NormalDir, PortableDir: String;
begin
  Result := True;
  if CurPageID = ModePage.ID then
  begin
    NormalDir := ExpandConstant('{autopf}\{#AppName}');
    PortableDir := ExpandConstant('{userdesktop}\{#AppName}');
    // Only the other type's suggestion is swapped: a folder from /DIR, a previous installation or the user stays.
    if IsPortable and SameText(WizardForm.DirEdit.Text, NormalDir) then
      WizardForm.DirEdit.Text := PortableDir
    else if not IsPortable and SameText(WizardForm.DirEdit.Text, PortableDir) then
      WizardForm.DirEdit.Text := NormalDir;
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  UserData: String;
begin
  if CurUninstallStep = usUninstall then
  begin
    // "Start with Windows" from the settings would otherwise point at a removed program.
    RegDeleteValue(HKEY_CURRENT_USER, 'Software\Microsoft\Windows\CurrentVersion\Run', '{#AppName}');
    // The program registers itself for notifications on first start.
    RegDeleteKeyIncludingSubkeys(HKEY_CURRENT_USER, 'Software\Classes\AppUserModelId\iRingil.PingWatch');
  end;
  if CurUninstallStep = usPostUninstall then
  begin
    UserData := ExpandConstant('{localappdata}\{#AppName}');
    if DirExists(UserData) and
       (SuppressibleMsgBox(CustomMessage('DeleteUserData'), mbConfirmation, MB_YESNO or MB_DEFBUTTON2, IDNO) = IDYES) then
      DelTree(UserData, True, True, True);
  end;
end;
