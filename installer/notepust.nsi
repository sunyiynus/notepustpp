; Notepust NSIS installer (NSIS 3.x, Unicode).
;
; Required files, built from the repository root before compiling:
;   cargo build -p notepust-app -p search-worker --release
;   target\release\notepust.exe
;   target\release\search-worker.exe
;
; Build:
;   makensis installer\notepust.nsi
;
; Output:
;   installer\Notepust-Setup-x64.exe
;   (move the setup executable to the distribution folder afterwards)
;
; Design rules:
;   * Per-user installation only: RequestExecutionLevel user and HKCU writes.
;     No HKLM/HKCR writes, so no administrator rights are needed.
;   * The installer never changes the default handler. The ProgID and
;     OpenWithProgids entries are written by
;     `notepust.exe --register-file-types`; the optional context menu uses the
;     documented SystemFileAssociations verb, which adds a menu entry without
;     touching `.ext` default values or UserChoice.
;   * `--register-file-types` / `--unregister-file-types` must be handled
;     headlessly by notepust.exe and exit without opening a window. Wire these
;     flags in crates/app before shipping (see docs/INSTALLER.md).

Unicode true
SetCompressor /SOLID lzma
SetCompressorDictSize 64
RequestExecutionLevel user

!include "MUI2.nsh"
!include "LogicLib.nsh"

!define APP_NAME "Notepust"
!define APP_EXE "notepust.exe"
!define WORKER_EXE "search-worker.exe"
!define PROG_ID "Notepust.Document"
!define VERSION "0.1.0"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Notepust"
!define APP_KEY "Software\Notepust"
!define CONTEXT_MENU_LABEL "使用 Notepust 编辑"

Name "${APP_NAME}"
BrandingText "${APP_NAME}"
OutFile "${__FILEDIR__}\Notepust-Setup-x64.exe"
InstallDir "$LOCALAPPDATA\Programs\Notepust"
InstallDirRegKey HKCU "${APP_KEY}" "InstallDir"
ShowInstDetails show
ShowUninstDetails show

VIProductVersion "${VERSION}.0"
VIAddVersionKey /LANG=2052 "ProductName" "${APP_NAME}"
VIAddVersionKey /LANG=2052 "FileDescription" "${APP_NAME} 安装程序"
VIAddVersionKey /LANG=2052 "FileVersion" "${VERSION}"
VIAddVersionKey /LANG=2052 "ProductVersion" "${VERSION}"
VIAddVersionKey /LANG=2052 "LegalCopyright" "MIT OR Apache-2.0"

!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\${APP_EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "启动 ${APP_NAME}"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "SimpChinese"

; ---------------------------------------------------------------------------
; Optional per-type Explorer verb.
;
; SystemFileAssociations is the supported way to add a verb for one file type
; without becoming its default handler. Only the Notepust verb subtree is
; written; no `.ext` default value and no Explorer\UserChoice value is touched.
;
; Keep this list in sync with SUPPORTED_EXTENSIONS in
; crates/platform/src/file_assoc.rs.
; ---------------------------------------------------------------------------
!macro INSTALL_CONTEXT_MENU EXT
    WriteRegStr HKCU "Software\Classes\SystemFileAssociations\${EXT}\shell\Notepust" "" "${CONTEXT_MENU_LABEL}"
    WriteRegStr HKCU "Software\Classes\SystemFileAssociations\${EXT}\shell\Notepust" "Icon" '"$INSTDIR\${APP_EXE}",0'
    WriteRegStr HKCU "Software\Classes\SystemFileAssociations\${EXT}\shell\Notepust\command" "" '"$INSTDIR\${APP_EXE}" "%1"'
!macroend

!macro REMOVE_CONTEXT_MENU EXT
    DeleteRegKey HKCU "Software\Classes\SystemFileAssociations\${EXT}\shell\Notepust"
!macroend

; ---------------------------------------------------------------------------
; Required application files and per-user uninstall metadata.
; ---------------------------------------------------------------------------
Section "Notepust（必需）" SEC_MAIN
    SectionIn RO

    SetOutPath "$INSTDIR"
    File "${__FILEDIR__}\..\target\release\${APP_EXE}"
    File "${__FILEDIR__}\..\target\release\${WORKER_EXE}"
    WriteUninstaller "$INSTDIR\uninstall.exe"

    WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${APP_NAME}"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" '"$INSTDIR\${APP_EXE}",0'
    WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "Notepust"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
    WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
    WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
    WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
    WriteRegStr HKCU "${APP_KEY}" "InstallDir" "$INSTDIR"

    ; Registers the ProgID, Applications\notepust.exe and the OpenWithProgids
    ; values for every supported extension. Only HKCU is written.
    DetailPrint "正在注册文件关联（仅当前用户）..."
    ExecWait '"$INSTDIR\${APP_EXE}" --register-file-types' $0
    ${If} $0 != 0
        DetailPrint "警告：文件关联注册返回代码 $0。稍后可运行 notepust.exe --register-file-types"
    ${EndIf}
SectionEnd

Section /o "开始菜单快捷方式" SEC_START_MENU
    CreateDirectory "$SMPROGRAMS\${APP_NAME}"
    CreateShortcut "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}" "" "$INSTDIR\${APP_EXE}" 0
    CreateShortcut "$SMPROGRAMS\${APP_NAME}\卸载 ${APP_NAME}.lnk" "$INSTDIR\uninstall.exe"
SectionEnd

Section /o "桌面快捷方式" SEC_DESKTOP
    CreateShortcut "$DESKTOP\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}" "" "$INSTDIR\${APP_EXE}" 0
SectionEnd

Section /o "资源管理器右键菜单（使用 Notepust 编辑）" SEC_CONTEXT_MENU
    ; Adds a verb only for the supported extensions. This is not a default
    ; handler change; Windows may still ask the user how to open a file.
    !insertmacro INSTALL_CONTEXT_MENU ".txt"
    !insertmacro INSTALL_CONTEXT_MENU ".log"
    !insertmacro INSTALL_CONTEXT_MENU ".md"
    !insertmacro INSTALL_CONTEXT_MENU ".markdown"
    !insertmacro INSTALL_CONTEXT_MENU ".json"
    !insertmacro INSTALL_CONTEXT_MENU ".jsonc"
    !insertmacro INSTALL_CONTEXT_MENU ".toml"
    !insertmacro INSTALL_CONTEXT_MENU ".yaml"
    !insertmacro INSTALL_CONTEXT_MENU ".yml"
    !insertmacro INSTALL_CONTEXT_MENU ".ini"
    !insertmacro INSTALL_CONTEXT_MENU ".xml"
    !insertmacro INSTALL_CONTEXT_MENU ".html"
    !insertmacro INSTALL_CONTEXT_MENU ".htm"
    !insertmacro INSTALL_CONTEXT_MENU ".css"
    !insertmacro INSTALL_CONTEXT_MENU ".js"
    !insertmacro INSTALL_CONTEXT_MENU ".mjs"
    !insertmacro INSTALL_CONTEXT_MENU ".cjs"
    !insertmacro INSTALL_CONTEXT_MENU ".ts"
    !insertmacro INSTALL_CONTEXT_MENU ".tsx"
    !insertmacro INSTALL_CONTEXT_MENU ".jsx"
    !insertmacro INSTALL_CONTEXT_MENU ".py"
    !insertmacro INSTALL_CONTEXT_MENU ".rs"
    !insertmacro INSTALL_CONTEXT_MENU ".c"
    !insertmacro INSTALL_CONTEXT_MENU ".h"
    !insertmacro INSTALL_CONTEXT_MENU ".cpp"
    !insertmacro INSTALL_CONTEXT_MENU ".hpp"
    !insertmacro INSTALL_CONTEXT_MENU ".sql"
    !insertmacro INSTALL_CONTEXT_MENU ".sh"
    !insertmacro INSTALL_CONTEXT_MENU ".bash"
SectionEnd

; ---------------------------------------------------------------------------
; Uninstall
; ---------------------------------------------------------------------------
Section "Uninstall"
    ; Remove the optional context-menu verbs first. DeleteRegKey is a no-op
    ; when a verb was never installed.
    !insertmacro REMOVE_CONTEXT_MENU ".txt"
    !insertmacro REMOVE_CONTEXT_MENU ".log"
    !insertmacro REMOVE_CONTEXT_MENU ".md"
    !insertmacro REMOVE_CONTEXT_MENU ".markdown"
    !insertmacro REMOVE_CONTEXT_MENU ".json"
    !insertmacro REMOVE_CONTEXT_MENU ".jsonc"
    !insertmacro REMOVE_CONTEXT_MENU ".toml"
    !insertmacro REMOVE_CONTEXT_MENU ".yaml"
    !insertmacro REMOVE_CONTEXT_MENU ".yml"
    !insertmacro REMOVE_CONTEXT_MENU ".ini"
    !insertmacro REMOVE_CONTEXT_MENU ".xml"
    !insertmacro REMOVE_CONTEXT_MENU ".html"
    !insertmacro REMOVE_CONTEXT_MENU ".htm"
    !insertmacro REMOVE_CONTEXT_MENU ".css"
    !insertmacro REMOVE_CONTEXT_MENU ".js"
    !insertmacro REMOVE_CONTEXT_MENU ".mjs"
    !insertmacro REMOVE_CONTEXT_MENU ".cjs"
    !insertmacro REMOVE_CONTEXT_MENU ".ts"
    !insertmacro REMOVE_CONTEXT_MENU ".tsx"
    !insertmacro REMOVE_CONTEXT_MENU ".jsx"
    !insertmacro REMOVE_CONTEXT_MENU ".py"
    !insertmacro REMOVE_CONTEXT_MENU ".rs"
    !insertmacro REMOVE_CONTEXT_MENU ".c"
    !insertmacro REMOVE_CONTEXT_MENU ".h"
    !insertmacro REMOVE_CONTEXT_MENU ".cpp"
    !insertmacro REMOVE_CONTEXT_MENU ".hpp"
    !insertmacro REMOVE_CONTEXT_MENU ".sql"
    !insertmacro REMOVE_CONTEXT_MENU ".sh"
    !insertmacro REMOVE_CONTEXT_MENU ".bash"

    ; Removes our ProgID, Applications entry and OpenWithProgids values.
    ; The executable must exit promptly for the Delete calls below to work.
    DetailPrint "正在移除文件关联（仅当前用户）..."
    ExecWait '"$INSTDIR\${APP_EXE}" --unregister-file-types' $0
    ${If} $0 != 0
        DetailPrint "警告：文件关联移除返回代码 $0"
    ${EndIf}

    ; Safety net for builds whose CLI flags are not wired yet. These names are
    ; owned by Notepust, so deleting them never touches another application.
    DeleteRegKey HKCU "Software\Classes\${PROG_ID}"
    DeleteRegKey HKCU "Software\Classes\Applications\${APP_EXE}"
    DeleteRegValue HKCU "Software\Classes\.txt\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.log\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.md\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.markdown\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.json\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.jsonc\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.toml\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.yaml\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.yml\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.ini\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.xml\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.html\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.htm\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.css\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.js\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.mjs\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.cjs\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.ts\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.tsx\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.jsx\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.py\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.rs\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.c\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.h\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.cpp\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.hpp\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.sql\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.sh\OpenWithProgids" "${PROG_ID}"
    DeleteRegValue HKCU "Software\Classes\.bash\OpenWithProgids" "${PROG_ID}"

    Delete "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk"
    Delete "$SMPROGRAMS\${APP_NAME}\卸载 ${APP_NAME}.lnk"
    RMDir "$SMPROGRAMS\${APP_NAME}"
    Delete "$DESKTOP\${APP_NAME}.lnk"

    Delete "$INSTDIR\${APP_EXE}"
    Delete "$INSTDIR\${WORKER_EXE}"
    Delete "$INSTDIR\uninstall.exe"
    RMDir "$INSTDIR"

    DeleteRegKey HKCU "${UNINSTALL_KEY}"
    DeleteRegKey HKCU "${APP_KEY}"

    ; Settings, recovery database, logs and user documents are intentionally
    ; kept. The portable layout is untouched as well.
SectionEnd
