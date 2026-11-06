!macro BAZZA_REQUIRE_WRITABLE_EXE filename inUse
  ${If} ${FileExists} "$INSTDIR\${filename}"
    ; Open the existing file for writing without changing it. A loaded image is locked.
    System::Call 'kernel32::CreateFileW(w "$INSTDIR\${filename}", i 0x40000000, i 7, p 0, i 3, i 0x80, p 0) p.r0'
    StrCmp $0 "-1" ${inUse}
    System::Call 'kernel32::CloseHandle(p r0)'
  ${EndIf}
!macroend

Var BazzaMcpExistingInstall

!macro NSIS_HOOK_PREINSTALL
  StrCpy $BazzaMcpExistingInstall "0"
  IfFileExists "$INSTDIR\${MAINBINARYNAME}.exe" +2 0
  IfFileExists "$INSTDIR\bazza-mcp.exe" 0 +2
  StrCpy $BazzaMcpExistingInstall "1"
  Push $0
  nsis_tauri_utils::FindProcessCurrentUser "${MAINBINARYNAME}.exe"
  Pop $0
  StrCmp $0 "0" bazza_mcp_install_locked
  !insertmacro BAZZA_REQUIRE_WRITABLE_EXE "${MAINBINARYNAME}.exe" bazza_mcp_install_locked
  !insertmacro BAZZA_REQUIRE_WRITABLE_EXE "bazza-mcp.exe" bazza_mcp_install_locked
  !insertmacro BAZZA_REQUIRE_WRITABLE_EXE "bazza-service.exe" bazza_mcp_install_locked
  Pop $0
  Goto bazza_mcp_preinstall_done

  bazza_mcp_install_locked:
  Pop $0
  DetailPrint "Файлы Bazza используются. Закройте Bazza и подключённые MCP-клиенты, подождите минуту и запустите установку снова."
  IfSilent +3
  StrCmp $PassiveMode "1" +2
  MessageBox MB_OK|MB_ICONEXCLAMATION "Файлы Bazza используются. Закройте Bazza и подключённые MCP-клиенты, подождите минуту и запустите установку снова."
  SetErrorLevel 2
  Abort

  bazza_mcp_preinstall_done:
!macroend

!macro NSIS_HOOK_POSTINSTALL
  Push $0
  Push $1
  StrCpy $1 ""
  StrCmp $UpdateMode "1" bazza_mcp_setup
  StrCmp $BazzaMcpExistingInstall "1" bazza_mcp_setup
  IfSilent bazza_mcp_setup
  StrCmp $PassiveMode "1" bazza_mcp_setup
  MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "Подключить Bazza к Codex? Codex сможет искать встречи в библиотеке и читать их расшифровки, итоги и заметки. Доступ можно отключить в разделе «Настройки → Доступ Codex через MCP»." IDYES bazza_mcp_codex_opt_in
  Goto bazza_mcp_setup_claude

  bazza_mcp_codex_opt_in:
  StrCpy $1 " --enable-codex"

  bazza_mcp_setup_claude:
  MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "Подключить Bazza к Claude Code и Claude Desktop? Они смогут искать встречи в библиотеке и читать их расшифровки, итоги и заметки. Доступ каждого клиента можно отключить в разделе «Настройки → Доступ Claude через MCP»." IDYES bazza_mcp_claude_opt_in
  Goto bazza_mcp_setup

  bazza_mcp_claude_opt_in:
  StrCpy $1 "$1 --enable-claude"

  bazza_mcp_setup:
  ClearErrors
  ExecWait '"$INSTDIR\bazza-mcp.exe" install-mcp$1' $0
  IfErrors bazza_mcp_setup_failed
  StrCmp $0 "0" bazza_mcp_setup_done

  bazza_mcp_setup_failed:
  DetailPrint "Bazza установлен. Не удалось подготовить библиотеку или настроить доступ через MCP."
  IfSilent +3
  StrCmp $PassiveMode "1" +2
  MessageBox MB_OK|MB_ICONEXCLAMATION "Bazza установлен. Не удалось подготовить библиотеку или настроить доступ через MCP."
  SetErrorLevel 2

  bazza_mcp_setup_done:
  Pop $1
  Pop $0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  Push $0
  nsis_tauri_utils::FindProcessCurrentUser "${MAINBINARYNAME}.exe"
  Pop $0
  StrCmp $0 "0" bazza_mcp_remove_failed
  !insertmacro BAZZA_REQUIRE_WRITABLE_EXE "${MAINBINARYNAME}.exe" bazza_mcp_remove_failed
  !insertmacro BAZZA_REQUIRE_WRITABLE_EXE "bazza-mcp.exe" bazza_mcp_remove_failed
  IfFileExists "$INSTDIR\bazza-mcp.exe" 0 bazza_mcp_remove_done
  ClearErrors
  ExecWait '"$INSTDIR\bazza-mcp.exe" uninstall-mcp' $0
  IfErrors bazza_mcp_remove_failed
  StrCmp $0 "0" bazza_mcp_remove_done

  bazza_mcp_remove_failed:
  Pop $0
  DetailPrint "Не удалось отключить MCP Bazza. Закройте Bazza и подключённые MCP-клиенты, подождите минуту и повторите удаление."
  IfSilent +3
  StrCmp $PassiveMode "1" +2
  MessageBox MB_OK|MB_ICONEXCLAMATION "Не удалось отключить MCP Bazza. Закройте Bazza и подключённые MCP-клиенты, подождите минуту и повторите удаление."
  SetErrorLevel 2
  Abort

  bazza_mcp_remove_done:
  Pop $0
!macroend
