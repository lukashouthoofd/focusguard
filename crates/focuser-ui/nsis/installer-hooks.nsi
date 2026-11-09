; FocusGuard's single-owner, per-machine service lifecycle.
!define FG_HOOK_DIR "${__FILEDIR__}"

!macro FG_EMBED_LIFECYCLE_HELPER
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File "${FG_HOOK_DIR}\service-lifecycle.ps1"
  File /oname=extension-identity.json "${FG_HOOK_DIR}\..\..\..\extension\identity.json"
  SetOutPath "$INSTDIR"
!macroend

!macro FG_RUN_LIFECYCLE mode
  nsExec::ExecToLog '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\service-lifecycle.ps1" -Mode ${mode} -InstallDir "$INSTDIR" -DeleteData "$DeleteAppDataCheckboxState"'
  Pop $0
  ${If} $0 != 0
    IfSilent fg_lifecycle_${mode}_silent_error fg_lifecycle_${mode}_interactive_error
    fg_lifecycle_${mode}_silent_error:
      DetailPrint "FocusGuard could not ${mode}; the lifecycle helper exited with code $0."
      Abort
    fg_lifecycle_${mode}_interactive_error:
    MessageBox MB_ICONSTOP|MB_OK "FocusGuard could not ${mode}. Exit code: $0. The installer stopped so it does not leave the service half-configured."
    Abort
  ${EndIf}
!macroend

!macro FG_RUN_INSTALLED_LIFECYCLE mode
  nsExec::ExecToLog '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$INSTDIR\service-lifecycle.ps1" -Mode ${mode} -InstallDir "$INSTDIR" -DeleteData "$DeleteAppDataCheckboxState"'
  Pop $0
  ${If} $0 != 0
    IfSilent fg_installed_lifecycle_${mode}_silent_error fg_installed_lifecycle_${mode}_interactive_error
    fg_installed_lifecycle_${mode}_silent_error:
      DetailPrint "FocusGuard could not ${mode}; the lifecycle helper exited with code $0."
      Abort
    fg_installed_lifecycle_${mode}_interactive_error:
    MessageBox MB_ICONSTOP|MB_OK "FocusGuard could not ${mode}. Exit code: $0. The installer stopped so it does not leave the service half-configured."
    Abort
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro FG_EMBED_LIFECYCLE_HELPER
  ; Per-machine Tauri installers terminate every matching UI process. Refuse
  ; to proceed if another Windows account owns one before invoking that check.
  !insertmacro FG_RUN_LIFECYCLE PreflightInstall
  !insertmacro CheckIfAppIsRunning "focusguard-ui.exe" "FocusGuard"
  !insertmacro FG_RUN_LIFECYCLE PrepareInstall
!macroend

!macro NSIS_HOOK_POSTINSTALL
  !insertmacro FG_RUN_LIFECYCLE Install
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ${If} $UpdateMode = 1
    ; The old uninstaller runs before the replacement files are copied. Stop
    ; the service here, while keeping its registration and hosts policy.
    !insertmacro FG_RUN_INSTALLED_LIFECYCLE PreflightInstall
    !insertmacro CheckIfAppIsRunning "focusguard-ui.exe" "FocusGuard"
    !insertmacro FG_RUN_INSTALLED_LIFECYCLE PrepareInstall
  ${Else}
    !insertmacro FG_RUN_INSTALLED_LIFECYCLE PreflightRemove
    !insertmacro CheckIfAppIsRunning "focusguard-ui.exe" "FocusGuard"
    !insertmacro FG_RUN_INSTALLED_LIFECYCLE Remove
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
!macroend
