; AI-Track per-user installer (NSIS, zlib-licensed build tool). Built by
; `cargo xtask package --target win`, which passes:
;   /DVERSION=0.0.1  /DNUMVERSION=0.0.1.0  /DSTAGE=<folder with the files>
;   /DOUTFILE=<setup.exe>  /DICON=<aitrack.ico>
;
; The auto-updater (docs/updater.md) runs `setup.exe /S /RELAUNCH` over an existing install:
;   /S         silent: no windows at all
;   /RELAUNCH  start aitrack.exe again when the files are in place
; The installer first waits (up to 60 s) for a running AI-Track to exit, because Windows
; will not let it replace a running executable.

Unicode true
SetCompressor /SOLID lzma
RequestExecutionLevel user

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"

!insertmacro GetParameters
!insertmacro GetOptions
!insertmacro GetSize

!define APP "AI-Track"
!define EXE "aitrack.exe"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP}"

Name "${APP} ${VERSION}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\${APP}"
; An update lands in the folder the user installed to the first time.
InstallDirRegKey HKCU "Software\${APP}" "InstallDir"
BrandingText "${APP} ${VERSION}"

VIProductVersion "${NUMVERSION}"
VIAddVersionKey "ProductName" "${APP}"
VIAddVersionKey "FileDescription" "${APP} installer"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "MIT License"

!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "Run ${APP}"

!insertmacro MUI_PAGE_LICENSE "${STAGE}\LICENSE.txt"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "Spanish"

; In: $R0 = file. Out: $R1 = 1 when it cannot be opened for writing (a running exe).
Function IsLocked
  StrCpy $R1 0
  IfFileExists "$R0" 0 done
  ClearErrors
  FileOpen $R2 "$R0" a
  IfErrors locked
  FileClose $R2
  Goto done
  locked:
  StrCpy $R1 1
  done:
FunctionEnd

; Waits until none of the executables is running. Gives up after 60 s.
Function WaitForApp
  StrCpy $R9 0
  loop:
    StrCpy $R1 0
    StrCpy $R0 "$INSTDIR\${EXE}"
    Call IsLocked
    ${If} $R1 == 0
      StrCpy $R0 "$INSTDIR\aitrack-plugin-host.exe"
      Call IsLocked
    ${EndIf}
    ${If} $R1 == 0
      StrCpy $R0 "$INSTDIR\aitrack-plugin-scanner.exe"
      Call IsLocked
    ${EndIf}
    ${If} $R1 == 0
      StrCpy $R0 "$INSTDIR\aitrack-ml-worker.exe"
      Call IsLocked
    ${EndIf}
    ${If} $R1 == 0
      Return
    ${EndIf}
    IntOp $R9 $R9 + 1
    ${If} $R9 > 120
      ${If} ${Silent}
        SetErrorLevel 5
        Abort
      ${Else}
        MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "${APP} is still running. Close it and choose Retry." IDRETRY retry
        Abort
        retry:
        StrCpy $R9 0
      ${EndIf}
    ${EndIf}
    Sleep 500
    Goto loop
FunctionEnd

Section "Install"
  SetOutPath "$INSTDIR"
  Call WaitForApp

  ; The content folder is ours alone; clearing it keeps files of older versions from lingering.
  RMDir /r "$INSTDIR\content"
  File /r "${STAGE}\*.*"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateShortcut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\${EXE}" "" "$INSTDIR\${EXE}" 0

  WriteRegStr HKCU "Software\${APP}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${APP}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "${APP}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\${EXE}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "EstimatedSize" "$0"

  ; /RELAUNCH: the updater is replacing a running app and wants it back.
  ${GetParameters} $R0
  ClearErrors
  ${GetOptions} $R0 "/RELAUNCH" $R1
  ${IfNot} ${Errors}
    Exec '"$INSTDIR\${EXE}"'
  ${EndIf}
SectionEnd

Section "Uninstall"
  Call un.WaitForApp
  Delete "$SMPROGRAMS\${APP}.lnk"
  Delete "$INSTDIR\aitrack.exe"
  Delete "$INSTDIR\aitrack-plugin-host.exe"
  Delete "$INSTDIR\aitrack-plugin-scanner.exe"
  Delete "$INSTDIR\aitrack-ml-worker.exe"
  Delete "$INSTDIR\aitrack-mcp.exe"
  Delete "$INSTDIR\LICENSE.txt"
  Delete "$INSTDIR\NOTICE.txt"
  RMDir /r "$INSTDIR\content"
  Delete "$INSTDIR\uninstall.exe"
  ; Only removes the folder when nothing else is in it. The settings, projects and
  ; library live in %APPDATA%\AI-Track and are kept on purpose.
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  DeleteRegKey HKCU "Software\${APP}"
SectionEnd

Function un.WaitForApp
  StrCpy $R9 0
  loop:
    StrCpy $R1 0
    ClearErrors
    IfFileExists "$INSTDIR\${EXE}" 0 done
    FileOpen $R2 "$INSTDIR\${EXE}" a
    IfErrors 0 free
    StrCpy $R1 1
    free:
    ${If} $R1 == 0
      FileClose $R2
      Return
    ${EndIf}
    IntOp $R9 $R9 + 1
    ${If} $R9 > 120
      MessageBox MB_OK|MB_ICONEXCLAMATION "${APP} is still running. Close it and run the uninstaller again."
      Abort
    ${EndIf}
    Sleep 500
    Goto loop
  done:
FunctionEnd
