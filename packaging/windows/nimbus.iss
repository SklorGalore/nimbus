; Inno Setup script for the Windows installer.
;
; Build (the release workflow does this; windows-latest runners have Inno
; Setup, so nothing is downloaded):
;   iscc /DVersion=0.8.0 /DSource=..\..\dist\nimbus-0.8.0-windows-x86_64 ^
;        /DOut=..\..\dist packaging\windows\nimbus.iss
;
; What it installs: nimbus.exe under Program Files, a Start Menu entry, an
; "Open with Nimbus" item on the right-click menu of files and of a
; folder's background, and an uninstaller. Nothing is put on PATH or
; registered as a default program; the Nimbus key under
; Software\Classes is removed at uninstall.
;
; Unsigned: SmartScreen shows "Windows protected your PC" until enough
; people have downloaded a given file. "More info" > "Run anyway" gets
; past it; the README says so.

#ifndef Version
  #define Version "0.0.0"
#endif
#ifndef Source
  #define Source "..\..\dist\nimbus"
#endif
#ifndef Out
  #define Out "..\..\dist"
#endif

[Setup]
AppId={{7F1B2C0E-5D3A-4E6B-9C1F-2A8D4E6F0B11}
AppName=Nimbus
AppVersion={#Version}
AppVerName=Nimbus {#Version}
AppPublisher=SklorGalore
AppPublisherURL=https://github.com/SklorGalore/nimbus
AppSupportURL=https://github.com/SklorGalore/nimbus/issues
AppUpdatesURL=https://github.com/SklorGalore/nimbus/releases
DefaultDirName={autopf}\Nimbus
DefaultGroupName=Nimbus
DisableProgramGroupPage=yes
LicenseFile={#Source}\LICENSE
OutputDir={#Out}
OutputBaseFilename=nimbus-{#Version}-windows-x86_64-setup
SetupIconFile={#Source}\nimbus.ico
UninstallDisplayIcon={app}\nimbus.exe
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ChangesAssociations=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "openwith"; Description: "Add ""Open with Nimbus"" to the right-click menu"; GroupDescription: "Shell integration:"

[Files]
Source: "{#Source}\nimbus.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\nimbus.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\HackNerdFont-LICENSE.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Nimbus"; Filename: "{app}\nimbus.exe"
Name: "{autodesktop}\Nimbus"; Filename: "{app}\nimbus.exe"; Tasks: desktopicon

[Registry]
; "Open with Nimbus" on any file, and on a folder's background (opens the
; folder as the sidebar root). HKA is HKLM for an all-users install and
; HKCU otherwise; uninsdeletekey removes each key at uninstall.
Root: HKA; Subkey: "Software\Classes\*\shell\Nimbus"; ValueType: string; ValueName: ""; ValueData: "Open with Nimbus"; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\*\shell\Nimbus"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\nimbus.exe"""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\*\shell\Nimbus\command"; ValueType: string; ValueName: ""; ValueData: """{app}\nimbus.exe"" ""%1"""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Nimbus"; ValueType: string; ValueName: ""; ValueData: "Open with Nimbus"; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Nimbus"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\nimbus.exe"""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Nimbus\command"; ValueType: string; ValueName: ""; ValueData: """{app}\nimbus.exe"" ""%V"""; Tasks: openwith
; Lets "Open with" dialogs list Nimbus for any file type.
Root: HKA; Subkey: "Software\Classes\Applications\nimbus.exe\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\nimbus.exe"" ""%1"""; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Applications\nimbus.exe"; ValueType: string; ValueName: "FriendlyAppName"; ValueData: "Nimbus"

[Run]
Filename: "{app}\nimbus.exe"; Description: "{cm:LaunchProgram,Nimbus}"; Flags: nowait postinstall skipifsilent
; Restart to Update (M31) runs this installer with /VERYSILENT /RELAUNCH=1
; after Nimbus quits; start the new version when it is done. Nimbus
; restores its last session when started with no arguments.
Filename: "{app}\nimbus.exe"; Flags: nowait; Check: RelaunchAsked

[Code]
function RelaunchAsked: Boolean;
begin
  Result := ExpandConstant('{param:RELAUNCH|0}') = '1';
end;
