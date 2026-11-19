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
; A running Ping Watch is asked to quit before its files are replaced (PrepareToInstall below); the Restart
; Manager stays as a fallback for other programs holding the files.
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
en.StillRunning={#AppName} is still running and couldn't be closed. Quit it (its icon in the notification area → Exit), then run this again.
en.PrivilegedLeft=Something {#AppName} set up with administrator rights is still there: its sign-in task “%1” in Task Scheduler, or its rules in Windows Firewall. Remove them there by hand.
ru.ModeCaption=Тип установки
ru.ModeDescription=Как установить {#AppName}?
ru.ModeNormal=Обычная: для этого пользователя Windows, с ярлыком в меню «Пуск» и деинсталлятором
ru.ModePortable=Портативная: в любую папку (например, на флешку); настройки и история остаются в этой папке
ru.DeleteUserData=Удалить также настройки и историю {#AppName}?
ru.StillRunning={#AppName} всё ещё работает, и закрыть его не удалось. Закройте его (значок в области уведомлений → «Выход») и запустите это снова.
ru.PrivilegedLeft=Кое-что из того, что {#AppName} настроил с правами администратора, осталось: задача входа «%1» в Планировщике заданий или его правила в брандмауэре Windows. Удалите их там вручную.
de.ModeCaption=Installationsart
de.ModeDescription=Wie soll {#AppName} installiert werden?
de.ModeNormal=Normal: für diesen Windows-Benutzer, mit Startmenü-Verknüpfung und Deinstallationsprogramm
de.ModePortable=Portabel: in einen beliebigen Ordner (z. B. einen USB-Stick); Einstellungen und Verlauf bleiben dort
de.DeleteUserData=Auch die Einstellungen und den Verlauf von {#AppName} löschen?
de.StillRunning={#AppName} läuft noch und konnte nicht geschlossen werden. Beenden Sie es (Symbol im Infobereich → Beenden) und starten Sie dies dann erneut.
de.PrivilegedLeft=Etwas, das {#AppName} mit Administratorrechten eingerichtet hat, ist noch da: die Anmeldeaufgabe „%1“ in der Aufgabenplanung oder seine Regeln in der Windows-Firewall. Entfernen Sie sie dort von Hand.
fr.ModeCaption=Type d’installation
fr.ModeDescription=Comment installer {#AppName} ?
fr.ModeNormal=Normale : pour cet utilisateur Windows, avec un raccourci dans le menu Démarrer et un programme de désinstallation
fr.ModePortable=Portable : dans n’importe quel dossier (une clé USB, par exemple) ; réglages et historique restent dans ce dossier
fr.DeleteUserData=Supprimer aussi les réglages et l’historique de {#AppName} ?
fr.StillRunning={#AppName} est toujours en cours d’exécution et n’a pas pu être fermé. Quittez-le (son icône dans la zone de notification → Quitter), puis relancez ceci.
fr.PrivilegedLeft=Une partie de ce que {#AppName} a mis en place avec des droits d’administrateur est toujours là : sa tâche d’ouverture de session « %1 » dans le Planificateur de tâches, ou ses règles dans le pare-feu Windows. Supprimez-les à la main.
es.ModeCaption=Tipo de instalación
es.ModeDescription=¿Cómo instalar {#AppName}?
es.ModeNormal=Normal: para este usuario de Windows, con acceso directo en el menú Inicio y desinstalador
es.ModePortable=Portátil: en cualquier carpeta (una memoria USB, por ejemplo); la configuración y el historial se quedan en ella
es.DeleteUserData=¿Eliminar también la configuración y el historial de {#AppName}?
es.StillRunning={#AppName} sigue en ejecución y no se pudo cerrar. Ciérrelo (su icono en el área de notificación → Salir) y vuelva a ejecutar esto.
es.PrivilegedLeft=Algo de lo que {#AppName} configuró con derechos de administrador sigue ahí: su tarea de inicio de sesión «%1» en el Programador de tareas o sus reglas en el Firewall de Windows. Elimínelas allí a mano.

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked; Check: not IsPortable

[Files]
Source: "{#BuiltExe}"; DestDir: "{app}"; DestName: "{#AppExeName}"; Flags: ignoreversion
Source: "..\..\LICENSE.md"; DestDir: "{app}"; Flags: ignoreversion
; License notices of the components (build.cmd generates THIRD-PARTY.html with cargo-about).
Source: "..\..\build\THIRD-PARTY.html"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\assets\fonts\OFL.txt"; DestDir: "{app}"; DestName: "Roboto-Condensed-OFL.txt"; Flags: ignoreversion
Source: "portable.txt"; DestDir: "{app}"; Flags: ignoreversion; Check: IsPortable

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExeName}"; Check: not IsPortable
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(AppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent

[Code]
var
  ModePage: TInputOptionWizardPage;

const
  EVENT_MODIFY_STATE = $0002;
  // Named objects of a running Ping Watch (src/ui/single_instance.rs): the mutex it holds while it runs, and the
  // event that asks it to quit. They are open to the user's own processes, with or without administrator rights.
  RunningMark = 'Local\iRingil.PingWatch.Running';
  QuitEvent = 'Local\iRingil.PingWatch.Quit';

function OpenEvent(DesiredAccess: Cardinal; InheritHandle: Boolean; Name: String): THandle;
  external 'OpenEventW@kernel32.dll stdcall';
function SetEvent(Event: THandle): Boolean;
  external 'SetEvent@kernel32.dll stdcall';
function CloseHandle(Handle: THandle): Boolean;
  external 'CloseHandle@kernel32.dll stdcall';

// Asks a running Ping Watch to quit (as its tray menu's Exit does: tabs and history are saved) and waits for it;
// Windows' Restart Manager can't close its window, nor touch it when it runs as administrator. True when no Ping
// Watch runs any more.
function QuitPingWatch: Boolean;
var
  Event: THandle;
  Waited: Integer;
begin
  Result := not CheckForMutexes(RunningMark);
  if Result then
    Exit;
  Event := OpenEvent(EVENT_MODIFY_STATE, False, QuitEvent);
  if Event <> 0 then
  begin
    SetEvent(Event);
    CloseHandle(Event);
  end;
  // It stops its traces first: up to a probe timeout per target.
  for Waited := 1 to 75 do
  begin
    Sleep(200);
    if not CheckForMutexes(RunningMark) then
    begin
      Result := True;
      Exit;
    end;
  end;
  Log('Ping Watch did not quit');
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  if QuitPingWatch then
    Result := ''
  else
    Result := CustomMessage('StillRunning');
end;

function InitializeUninstall: Boolean;
begin
  Result := QuitPingWatch;
  if not Result then
    SuppressibleMsgBox(CustomMessage('StillRunning'), mbError, MB_OK, IDOK);
end;

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

// Whether Task Scheduler holds the task of this name (in its root folder), as far as this user can see.
function TaskExists(Task: String): Boolean;
var
  Service, Folder: Variant;
begin
  Result := False;
  try
    Service := CreateOleObject('Schedule.Service');
    Service.Connect;
    Folder := Service.GetFolder('\');
    Folder.GetTask(Task);
    Result := True;
  except
  end;
end;

// Whether Windows Firewall holds any of the rules the program adds for its TCP probes (src/probe/firewall.rs).
function FirewallRulesExist: Boolean;
var
  Policy, Rules: Variant;
  Names: array [0..2] of String;
  I: Integer;
begin
  Result := False;
  Names[0] := '{#AppName} - Hop answers (ICMPv4-In)';
  Names[1] := '{#AppName} - Hop answers (ICMPv6-In)';
  Names[2] := '{#AppName} - Target answers (TCP-In)';
  try
    Policy := CreateOleObject('HNetCfg.FwPolicy2');
    Rules := Policy.Rules;
  except
    Exit;
  end;
  for I := 0 to 2 do
    try
      Rules.Item(Names[I]);
      Result := True;
    except
    end;
end;

// Removes what the program set up with administrator rights, if the user had it do so: the Task Scheduler task that
// starts it with them at sign-in (src/store/autostart.rs) and its Windows Firewall rules. Left behind, the task and
// the rule for the program's path would serve whatever is put where the program was. Removing them needs the same
// rights, so the program is run with them to do it (Windows asks: UAC); said no to, the user is told what is left.
procedure RemovePrivileged;
var
  Task: String;
  Code: Integer;
begin
  Task := '{#AppName} (' + GetUserNameString + ')';
  if not (TaskExists(Task) or FirewallRulesExist) then
    Exit;
  ShellExec('runas', ExpandConstant('{app}\{#AppExeName}'), '--remove-traces', '', SW_HIDE, ewWaitUntilTerminated, Code);
  if TaskExists(Task) or FirewallRulesExist then
    SuppressibleMsgBox(FmtMessage(CustomMessage('PrivilegedLeft'), [Task]), mbError, MB_OK, IDOK);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  UserData: String;
begin
  if CurUninstallStep = usUninstall then
  begin
    RemovePrivileged;
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
