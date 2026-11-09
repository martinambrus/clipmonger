Unicode true
!include "MUI2.nsh"
!include "nsDialogs.nsh"
!include "LogicLib.nsh"

Var ProfileSnapshot
Var ProfilePath
Var ProfileCheckbox
Var ProfileSelected

!ifndef INPUT_DIR
  !error "INPUT_DIR is required"
!endif
!ifndef OUTPUT_FILE
  !error "OUTPUT_FILE is required"
!endif
Name "Clipmonger"
OutFile "${OUTPUT_FILE}"
InstallDir "$PROGRAMFILES64\Clipmonger"
RequestExecutionLevel admin
ShowInstDetails show
ShowUninstDetails show
!define MUI_ICON "clipmonger.ico"
!define MUI_UNICON "clipmonger.ico"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_WELCOME
!insertmacro MUI_UNPAGE_CONFIRM
UninstPage custom un.ProfileNotice un.ProfileLeave
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_UNPAGE_FINISH
!insertmacro MUI_LANGUAGE "English"

Function .onInit
  SetRegView 64
  SetShellVarContext all
  StrCpy $INSTDIR "$PROGRAMFILES64\Clipmonger"
  IfFileExists "$PROGRAMFILES64\Clipmonger-Uninstall-Recovery.exe" 0 +3
    MessageBox MB_ICONSTOP "A previous recovery executable remains at $PROGRAMFILES64\Clipmonger-Uninstall-Recovery.exe. Resolve it before installing."
    Abort
  IfFileExists "$INSTDIR\*.*" 0 +3
    MessageBox MB_ICONSTOP "The installation directory already exists. Remove or uninstall its contents before installing this build."
    Abort
FunctionEnd

Function un.onInit
  SetRegView 64
  SetShellVarContext all
  StrCpy $INSTDIR "$PROGRAMFILES64\Clipmonger"

FunctionEnd

Function un.ProfileNotice
  StrCpy $ProfileSelected 0
  StrCpy $ProfileSnapshot ""
  StrCpy $ProfilePath ""
  ClearErrors
  SetOutPath "$INSTDIR"
  File /oname=cleanup.ps1 "cleanup.ps1"
  File /oname=profile-removal.cs "profile-removal.cs"
  ${IfNot} ${Errors}
    nsExec::ExecToStack '"$WINDIR\Sysnative\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$INSTDIR\cleanup.ps1" -InstallDir "$INSTDIR" -Mode ProfileInfo'
    Pop $0
    Pop $1
    ${If} $0 == 0
      # The helper emits only a bounded base64 path, SID and hexadecimal IDs.
      # Validate every character before putting this value in a command line.
      StrCpy $2 0
      StrCpy $3 ""
      ${Do}
        StrCpy $4 $1 1 $2
        ${If} $4 == ""
          ${ExitDo}
        ${EndIf}
        StrCpy $5 0
        ${Do}
          StrCpy $6 "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=:-" 1 $5
          ${If} $6 == ""
            StrCpy $3 "invalid"
            ${ExitDo}
          ${EndIf}
          ${If} $6 == $4
            ${ExitDo}
          ${EndIf}
          IntOp $5 $5 + 1
        ${Loop}
        IntOp $2 $2 + 1
      ${Loop}
      ${If} $3 == ""
        StrCpy $ProfileSnapshot $1
        StrCpy $2 0
        ${Do}
          StrCpy $3 $1 1 $2
          ${If} $3 == ":"
            ${ExitDo}
          ${EndIf}
          ${If} $3 == ""
            StrCpy $ProfileSnapshot ""
            ${ExitDo}
          ${EndIf}
          IntOp $2 $2 + 1
        ${Loop}
        ${If} $ProfileSnapshot != ""
          StrCpy $2 $1 $2
          System::Call 'crypt32::CryptStringToBinaryW(w r2, i 0, i 1, p 0, *i .r3, p 0, p 0) i .r4'
          ${If} $4 == 1
            IntOp $5 $3 + 2
            System::Alloc $5
            Pop $5
            System::Call 'kernel32::RtlZeroMemory(p r5, p r3)'
            System::Call 'crypt32::CryptStringToBinaryW(w r2, i 0, i 1, p r5, *i r3, p 0, p 0) i .r4'
            ${If} $4 == 1
              IntOp $6 $5 + $3
              System::Call '*$6(&i2 0)'
              System::Call '*$5(&w${NSIS_MAX_STRLEN} .r6)'
              StrCpy $ProfilePath $6
            ${Else}
              StrCpy $ProfileSnapshot ""
            ${EndIf}
            System::Free $5
          ${Else}
            StrCpy $ProfileSnapshot ""
          ${EndIf}
        ${EndIf}
      ${EndIf}
    ${EndIf}
  ${EndIf}
  !insertmacro MUI_HEADER_TEXT "Default profile" "Choose whether to remove your default profile."
  nsDialogs::Create 1018
  Pop $0
  ${If} $0 == error
    Abort
  ${EndIf}
  ${NSD_CreateCheckbox} 0 0 100% 24u "Delete my default Clipmonger profile and everything inside it"
  Pop $ProfileCheckbox
  ${If} $ProfileSnapshot == ""
    EnableWindow $ProfileCheckbox 0
    ${NSD_CreateLabel} 0 32u 100% 46u "The default profile could not be verified. Profile data will be kept."
  ${Else}
    ${NSD_CreateLabel} 0 32u 100% 64u "$ProfilePath"
  ${EndIf}
  Pop $0
  nsDialogs::Show
FunctionEnd

Function un.ProfileLeave
  ${NSD_GetState} $ProfileCheckbox $0
  ${If} $0 == ${BST_CHECKED}
    MessageBox MB_ICONEXCLAMATION|MB_YESNO|MB_DEFBUTTON2 "Delete this folder and everything inside it?$\r$\n$\r$\n$ProfilePath" IDYES +2
    Abort
    StrCpy $ProfileSelected 1
  ${EndIf}
FunctionEnd

!macro CheckInstall
  ${If} ${Errors}
    MessageBox MB_ICONSTOP "Installation did not finish. Run $INSTDIR\uninstall.exe to remove this partial installation. If that file was not created, remove the empty $INSTDIR directory and retry."
    Abort
  ${EndIf}
!macroend

Section "Install"
  ClearErrors
  SetOutPath "$INSTDIR"
  !insertmacro CheckInstall
  WriteUninstaller "$INSTDIR\uninstall.exe"
  !insertmacro CheckInstall
  File "${INPUT_DIR}/clipmonger.exe"
  !insertmacro CheckInstall
  File "${INPUT_DIR}/clipmonger-daemon.exe"
  !insertmacro CheckInstall
  File "${INPUT_DIR}/clipmonger-cli.exe"
  !insertmacro CheckInstall
  File /oname=cleanup.ps1 "cleanup.ps1"
  !insertmacro CheckInstall
  File /oname=profile-removal.cs "profile-removal.cs"
  !insertmacro CheckInstall
  SetOutPath "$INSTDIR\licenses"
  !insertmacro CheckInstall
  File /r "${INPUT_DIR}/licenses/*"
  !insertmacro CheckInstall
  SetOutPath "$INSTDIR"
  !insertmacro CheckInstall
  CreateDirectory "$SMPROGRAMS\Clipmonger"
  !insertmacro CheckInstall
  CreateShortcut "$SMPROGRAMS\Clipmonger\Clipmonger.lnk" "$INSTDIR\clipmonger.exe" "" "$INSTDIR\clipmonger.exe" 0
  !insertmacro CheckInstall
  CreateShortcut "$SMPROGRAMS\Clipmonger\Uninstall.lnk" "$INSTDIR\uninstall.exe"
  !insertmacro CheckInstall
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Clipmonger" "DisplayName" "Clipmonger"
  !insertmacro CheckInstall
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Clipmonger" "DisplayIcon" '"$INSTDIR\clipmonger.exe",0'
  !insertmacro CheckInstall
  # SHCNE_ASSOCCHANGED: an upgrade would otherwise keep the shell's cached icon.
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Clipmonger" "UninstallString" '"$INSTDIR\uninstall.exe"'
  !insertmacro CheckInstall
SectionEnd

Section "Uninstall"
  SetRegView 64
  ClearErrors
  SetOutPath "$INSTDIR"
  File /oname=cleanup.ps1 "cleanup.ps1"
  ${If} ${Errors}
    MessageBox MB_ICONSTOP "The cleanup helper could not be restored. The uninstaller and registration were kept; retry after correcting access or disk space."
    Abort
  ${EndIf}
  File /oname=profile-removal.cs "profile-removal.cs"
  ${If} ${Errors}
    MessageBox MB_ICONSTOP "The profile helper could not be restored. The uninstaller and registration were kept."
    Abort
  ${EndIf}
  StrCpy $0 -1
  ClearErrors
  ExecWait '"$WINDIR\Sysnative\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$INSTDIR\cleanup.ps1" -InstallDir "$INSTDIR"' $0
  ${If} ${Errors}
  ${OrIf} $0 != 0
    MessageBox MB_ICONSTOP "Cleanup could not establish ownership or stop installed processes. No program files were removed."
    Abort
  ${EndIf}
  ${IfNot} ${Silent}
  ${AndIf} $ProfileSelected == 1
    StrCpy $0 -1
    ClearErrors
    ExecWait '"$WINDIR\Sysnative\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$INSTDIR\cleanup.ps1" -InstallDir "$INSTDIR" -Mode ProfileDelete -Expected "$ProfileSnapshot"' $0
    ${If} ${Errors}
    ${OrIf} $0 != 0
      MessageBox MB_ICONSTOP "Profile removal failed. Some profile contents may already have been removed. The uninstaller and registration were kept."
      Abort
    ${EndIf}
  ${EndIf}
  ClearErrors
  Delete "$INSTDIR\clipmonger.exe"
  Delete "$INSTDIR\clipmonger-daemon.exe"
  Delete "$INSTDIR\clipmonger-cli.exe"
  Delete "$INSTDIR\licenses\fonts\Hack-Regular.txt"
  Delete "$INSTDIR\licenses\fonts\OFL.txt"
  Delete "$INSTDIR\licenses\fonts\README.md"
  Delete "$INSTDIR\licenses\fonts\UFL.txt"
  Delete "$INSTDIR\licenses\fonts\egui-icons.txt"
  Delete "$INSTDIR\licenses\fonts\emoji-icon-font-mit-license.txt"
  Delete "$INSTDIR\licenses\rust-upstream\23f18e03dc49df91622fe2a76176497404e46ced8a715d9d2b67a7446571cca3.txt"
  Delete "$INSTDIR\licenses\rust-upstream\508a77d2e7b51d98adeed32648ad124b7b30241a8e70b2e72c99f92d8e5874d1.txt"
  Delete "$INSTDIR\licenses\rust-upstream\5b19674a1db628a475850a131956ed49521b744e3dda8f5a94141f9aba681219.txt"
  Delete "$INSTDIR\licenses\rust-upstream\95ca92f5f8ea5231f1580b3a2a799e8260af3114b900e1def5355a7f44bcf60c.txt"
  Delete "$INSTDIR\licenses\rust-upstream\9eff396a99bafd208e7126602420d6864f6066c08b84485a5b032320a9c01997.txt"
  Delete "$INSTDIR\licenses\rust-upstream\README.md"
  Delete "$INSTDIR\licenses\rust-upstream\apache-2.0.txt"
  Delete "$INSTDIR\licenses\rust-upstream\apache-source.json"
  Delete "$INSTDIR\licenses\rust-upstream\c2cfccb812fe482101a8f04597dfc5a9991a6b2748266c47ac91b6a5aae15383.txt"
  Delete "$INSTDIR\licenses\rust-upstream\c8167fdeeed46d3f244d3f85c5bf998ce889343691c32be2c61a8bc4b5c08333.txt"
  Delete "$INSTDIR\licenses\rust-upstream\c9bff75738922193e67fa726fa225535870d2aa1059f91452c411736284ad566.txt"
  Delete "$INSTDIR\licenses\rust-upstream\e353f37b12aefbb9f9b29490e837cfee05d9bda70804b3562839a3285c1df1e5.txt"
  Delete "$INSTDIR\licenses\rust-upstream\eb227437252b2a7a9c1fc342c93ade1f3d7ce38cc6dd754f613db07d53ceff0b.txt"
  Delete "$INSTDIR\licenses\rust-upstream\sources.json"
  Delete "$INSTDIR\licenses\wayland\LGPL-2.1.txt"
  Delete "$INSTDIR\licenses\wayland\README.md"
  Delete "$INSTDIR\licenses\RUST-NOTICES.txt"
  IfFileExists "$INSTDIR\licenses\rust-upstream\*.*" 0 +2
  RMDir "$INSTDIR\licenses\rust-upstream"
  IfFileExists "$INSTDIR\licenses\fonts\*.*" 0 +2
  RMDir "$INSTDIR\licenses\fonts"
  IfFileExists "$INSTDIR\licenses\wayland\*.*" 0 +2
  RMDir "$INSTDIR\licenses\wayland"
  IfFileExists "$INSTDIR\licenses\*.*" 0 +2
  RMDir "$INSTDIR\licenses"
  Delete "$SMPROGRAMS\Clipmonger\Clipmonger.lnk"
  Delete "$SMPROGRAMS\Clipmonger\Uninstall.lnk"
  IfFileExists "$SMPROGRAMS\Clipmonger\*.*" 0 +2
  RMDir "$SMPROGRAMS\Clipmonger"
  ${If} ${Errors}
    MessageBox MB_ICONSTOP "Some installed files or shortcuts could not be removed. The uninstaller has been kept."
    Abort
  ${EndIf}
  Delete "$INSTDIR\profile-removal.cs"
  Delete "$INSTDIR\cleanup.ps1"
  ${If} ${Errors}
    MessageBox MB_ICONSTOP "The cleanup helper could not be removed. The uninstaller and registration were kept."
    Abort
  ${EndIf}
  # Query the key itself: absence is distinct from access or API failures.
  System::Call 'advapi32::RegOpenKeyExW(p 0x80000002, w "Software\Microsoft\Windows\CurrentVersion\Uninstall\Clipmonger", i 0, i 0x20119, *p .r1) i .r0'
  StrCpy $8 0
  ${If} $0 == 0
    StrCpy $8 1
    System::Call 'advapi32::RegCloseKey(p r1) i .r0'
    ${If} $0 != 0
      MessageBox MB_ICONSTOP "The uninstall registration could not be queried. The uninstaller was kept."
      Abort
    ${EndIf}
  ${ElseIf} $0 != 2
    MessageBox MB_ICONSTOP "The uninstall registration is inaccessible. The uninstaller was kept."
    Abort
  ${EndIf}
  # The recovery name is fixed and never overwritten. It remains available if
  # restoring the original executable fails during finalization.
  IfFileExists "$PROGRAMFILES64\Clipmonger-Uninstall-Recovery.exe" 0 +3
    MessageBox MB_ICONSTOP "A recovery executable already exists at $PROGRAMFILES64\Clipmonger-Uninstall-Recovery.exe. Preserve it and resolve that collision before retrying."
    Abort
  ClearErrors
  SetOutPath "$WINDIR"
  ${If} ${Errors}
    MessageBox MB_ICONSTOP "A safe working directory could not be selected. The uninstaller and registration were kept."
    Abort
  ${EndIf}
  ClearErrors
  Rename "$INSTDIR\uninstall.exe" "$PROGRAMFILES64\Clipmonger-Uninstall-Recovery.exe"
  ${If} ${Errors}
    MessageBox MB_ICONSTOP "The recovery copy could not be established. The uninstaller was kept."
    Abort
  ${EndIf}
  ClearErrors
  RMDir "$INSTDIR"
  ${If} ${Errors}
    ClearErrors
    Rename "$PROGRAMFILES64\Clipmonger-Uninstall-Recovery.exe" "$INSTDIR\uninstall.exe"
    ${If} ${Errors}
      MessageBox MB_ICONSTOP "The directory could not be removed. Recovery is at $PROGRAMFILES64\Clipmonger-Uninstall-Recovery.exe; restore it to $INSTDIR\uninstall.exe before retrying."
    ${Else}
      MessageBox MB_ICONSTOP "The directory could not be removed. The uninstaller and registration were kept."
    ${EndIf}
    Abort
  ${EndIf}
  ${If} $8 == 1
    ClearErrors
    DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Clipmonger"
    ${If} ${Errors}
      ClearErrors
      CreateDirectory "$INSTDIR"
      Rename "$PROGRAMFILES64\Clipmonger-Uninstall-Recovery.exe" "$INSTDIR\uninstall.exe"
      ${If} ${Errors}
        MessageBox MB_ICONSTOP "The registration could not be removed. Recovery is at $PROGRAMFILES64\Clipmonger-Uninstall-Recovery.exe; restore it to $INSTDIR\uninstall.exe before retrying."
      ${Else}
        MessageBox MB_ICONSTOP "The registration could not be removed. The uninstaller and registration were kept."
      ${EndIf}
      Abort
    ${EndIf}
  ${EndIf}
  ClearErrors
  Delete "$PROGRAMFILES64\Clipmonger-Uninstall-Recovery.exe"
  ${If} ${Errors}
    MessageBox MB_ICONEXCLAMATION "Clipmonger was removed, but its recovery executable remains at $PROGRAMFILES64\Clipmonger-Uninstall-Recovery.exe. Remove that file after resolving access."
  ${EndIf}
SectionEnd
