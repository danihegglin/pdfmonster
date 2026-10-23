; Inno Setup script for the Windows installer. Built in CI by .github/workflows/release.yml:
;   iscc /DAppVersion=0.2.0 /DNumericVersion=0.2.0 /DArch=x86_64 /DSourceDir=<staging dir> /DOutputDir=dist packaging\windows\pdfmonster.iss
; SourceDir must hold pdfmonster.exe, pdfmonster.ico, README.md and LICENSE.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef NumericVersion
  #define NumericVersion "0.0.0"
#endif
#ifndef Arch
  #define Arch "x86_64"
#endif
#ifndef SourceDir
  #define SourceDir "."
#endif
#ifndef OutputDir
  #define OutputDir "."
#endif

#if Arch == "aarch64"
  #define InnoArch "arm64"
#else
  #define InnoArch "x64compatible"
#endif

[Setup]
AppId={{6C1F4E2B-9D3A-4B7E-8F21-5A0C9E7D3B14}
AppName=pdfmonster
AppVersion={#AppVersion}
AppVerName=pdfmonster {#AppVersion}
AppPublisher=Dani Hegglin
AppPublisherURL=https://github.com/danihegglin/pdfmonster
AppSupportURL=https://github.com/danihegglin/pdfmonster/issues
AppUpdatesURL=https://github.com/danihegglin/pdfmonster/releases
VersionInfoVersion={#NumericVersion}
DefaultDirName={autopf}\pdfmonster
DefaultGroupName=pdfmonster
DisableProgramGroupPage=yes
; Installs for the current user without admin rights; the dialog offers an all-users install.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed={#InnoArch}
ArchitecturesInstallIn64BitMode={#InnoArch}
LicenseFile={#SourceDir}\LICENSE
SetupIconFile={#SourceDir}\pdfmonster.ico
UninstallDisplayIcon={app}\pdfmonster.exe
OutputDir={#OutputDir}
OutputBaseFilename=pdfmonster-{#AppVersion}-windows-{#Arch}-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ChangesAssociations=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\pdfmonster.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\pdfmonster.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\pdfmonster"; Filename: "{app}\pdfmonster.exe"; IconFilename: "{app}\pdfmonster.ico"
Name: "{autodesktop}\pdfmonster"; Filename: "{app}\pdfmonster.exe"; IconFilename: "{app}\pdfmonster.ico"; Tasks: desktopicon

[Registry]
; Offer pdfmonster in "Open with" for PDFs without taking over the default PDF viewer.
Root: HKA; Subkey: "Software\Classes\pdfmonster.pdf"; ValueType: string; ValueData: "PDF Document"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\pdfmonster.pdf\DefaultIcon"; ValueType: string; ValueData: "{app}\pdfmonster.ico"
Root: HKA; Subkey: "Software\Classes\pdfmonster.pdf\shell\open\command"; ValueType: string; ValueData: """{app}\pdfmonster.exe"" ""%1"""
Root: HKA; Subkey: "Software\Classes\.pdf\OpenWithProgids"; ValueType: string; ValueName: "pdfmonster.pdf"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\pdfmonster.exe\SupportedTypes"; ValueType: string; ValueName: ".pdf"; ValueData: ""; Flags: uninsdeletekey
; Lets `pdfmonster` be started from the Run dialog.
Root: HKA; Subkey: "Software\Microsoft\Windows\CurrentVersion\App Paths\pdfmonster.exe"; ValueType: string; ValueData: "{app}\pdfmonster.exe"; Flags: uninsdeletekey

[Run]
Filename: "{app}\pdfmonster.exe"; Description: "{cm:LaunchProgram,pdfmonster}"; Flags: nowait postinstall skipifsilent
