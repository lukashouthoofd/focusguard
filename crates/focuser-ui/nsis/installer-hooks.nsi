; FocusGuard NSIS hooks. The generated installer has not been run on this host.
; The Windows logon task is a temporary app-start mechanism, not a service or
; an anti-bypass boundary. A service-backed installer is still required.

!macro NSIS_HOOK_POSTINSTALL
  nsExec::ExecToLog 'schtasks /create /tn "FocusGuard" /tr "\"$INSTDIR\focusguard-ui.exe\" --autostart" /sc onlogon /rl highest /f'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; Remove only FocusGuard's marked hosts-file section, and only on uninstall.
  ; During an update the new app needs the entries to remain in place.
  ${If} $UpdateMode <> 1
    !insertmacro CheckIfAppIsRunning "$INSTDIR\focusguard-ui.exe" "FocusGuard"
    nsExec::ExecToLog '"$INSTDIR\focusguard-ui.exe" --remove-hosts-blocks'
    Pop $0
    ${If} $0 != 0
      ExecShellWait "runas" "$INSTDIR\focusguard-ui.exe" "--remove-hosts-blocks" SW_HIDE
    ${EndIf}
  ${EndIf}

  nsExec::ExecToLog 'schtasks /delete /tn "FocusGuard" /f'
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Keep the data deletion scoped to FocusGuard's new product directories.
  ; Never delete the upstream Focuser directories during migration.
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    SetShellVarContext current
    RMDir /r "$APPDATA\publikhq\FocusGuard"
    RMDir "$APPDATA\publikhq"
    RMDir /r "$LOCALAPPDATA\publikhq\FocusGuard"
    RMDir "$LOCALAPPDATA\publikhq"
  ${EndIf}
!macroend
