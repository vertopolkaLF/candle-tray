; Candle Tray NSIS installer template. Placeholders are filled by build.ps1.

Unicode true
ManifestDPIAware true

!include "MUI2.nsh"
!include "x64.nsh"
!include "LogicLib.nsh"

Name "{{PRODUCT_NAME}}"
BrandingText "{{PRODUCT_NAME}} {{VERSION}}"
OutFile "{{OUT_FILE}}"
InstallDir "{{INSTALL_DIR}}"
RequestExecutionLevel user
SetCompressor /SOLID lzma

VIProductVersion "{{VERSION}}.0"
VIAddVersionKey "ProductName" "{{PRODUCT_NAME}}"
VIAddVersionKey "CompanyName" "{{PUBLISHER}}"
VIAddVersionKey "FileDescription" "{{PRODUCT_NAME}} Setup ({{ARCH}})"
VIAddVersionKey "FileVersion" "{{VERSION}}"
VIAddVersionKey "ProductVersion" "{{VERSION}}"
VIAddVersionKey "LegalCopyright" "Copyright (C) {{PUBLISHER}}"

!define MUI_ABORTWARNING
!define MUI_ICON "{{ICON_FILE}}"
!define MUI_UNICON "{{ICON_FILE}}"

!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES

!define MUI_FINISHPAGE_RUN "$INSTDIR\candle-tray.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Launch Candle Tray"
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"


!macro KillRunningAppBody
  ; Kill via Win32 APIs only, without a console window.
  Push $0
  Push $1
  Push $2
  Push $3
  Push $4
  Push $5

  ; TH32CS_SNAPPROCESS = 2. A 32-bit NSIS PROCESSENTRY32W is 556 bytes.
  System::Call 'kernel32::CreateToolhelp32Snapshot(i 2, i 0) i .r0'
  ${If} $0 <> -1
    System::Alloc 556
    Pop $1
    System::Call '*$1(i 556)'
    System::Call 'kernel32::Process32FirstW(i r0, i r1) i .r2'
    ${While} $2 <> 0
      ; th32ProcessID -> $3, szExeFile -> $4
      System::Call '*$1(i,i,i.r3,i,i,i,i,i,i,&w260.r4)'
      ${If} $4 == "candle-tray.exe"
        ; PROCESS_TERMINATE | SYNCHRONIZE; wait until the executable is released.
        System::Call 'kernel32::OpenProcess(i 0x00100001, i 0, i r3) i .r5'
        ${If} $5 <> 0
          System::Call 'kernel32::TerminateProcess(i r5, i 0)'
          System::Call 'kernel32::WaitForSingleObject(i r5, i 5000)'
          System::Call 'kernel32::CloseHandle(i r5)'
        ${EndIf}
      ${EndIf}
      System::Call 'kernel32::Process32NextW(i r0, i r1) i .r2'
    ${EndWhile}
    System::Free $1
    System::Call 'kernel32::CloseHandle(i r0)'
  ${EndIf}

  Pop $5
  Pop $4
  Pop $3
  Pop $2
  Pop $1
  Pop $0
!macroend

Function KillRunningApp
  !insertmacro KillRunningAppBody
FunctionEnd

Function un.KillRunningApp
  !insertmacro KillRunningAppBody
FunctionEnd

Function .onInit
  SetShellVarContext current
  Call KillRunningApp
FunctionEnd

Function un.onInit
  SetShellVarContext current
  Call un.KillRunningApp
FunctionEnd

Section "Install"
  Call KillRunningApp
  SetOutPath "$INSTDIR"
  File /r "{{SOURCE_DIR}}\*.*"

  CreateDirectory "$SMPROGRAMS\{{PRODUCT_NAME}}"
  CreateShortCut "$SMPROGRAMS\{{PRODUCT_NAME}}\{{PRODUCT_NAME}}.lnk" "$INSTDIR\candle-tray.exe"

  WriteUninstaller "$INSTDIR\Uninstall.exe"

  WriteRegStr SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\{{PRODUCT_NAME}}" \
    "DisplayName" "{{PRODUCT_NAME}} ({{ARCH}})"
  WriteRegStr SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\{{PRODUCT_NAME}}" \
    "DisplayVersion" "{{VERSION}}"
  WriteRegStr SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\{{PRODUCT_NAME}}" \
    "Publisher" "{{PUBLISHER}}"
  WriteRegStr SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\{{PRODUCT_NAME}}" \
    "DisplayIcon" "$INSTDIR\candle-tray.exe"
  WriteRegStr SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\{{PRODUCT_NAME}}" \
    "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\{{PRODUCT_NAME}}" \
    "InstallLocation" "$INSTDIR"
  WriteRegDWORD SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\{{PRODUCT_NAME}}" \
    "NoModify" 1
  WriteRegDWORD SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\{{PRODUCT_NAME}}" \
    "NoRepair" 1
SectionEnd

Section "Uninstall"
  Call un.KillRunningApp
  Delete "$SMPROGRAMS\{{PRODUCT_NAME}}\{{PRODUCT_NAME}}.lnk"
  RMDir "$SMPROGRAMS\{{PRODUCT_NAME}}"

  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Candle Tray"
  DeleteRegKey SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\{{PRODUCT_NAME}}"
  DeleteRegKey HKCU "Software\Classes\candle-tray"

  ; Keep settings in $APPDATA\Candle Tray silently, and preserve unrelated files.
  Delete "$INSTDIR\candle-tray.exe"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\THIRD_PARTY_NOTICES.md"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
SectionEnd
