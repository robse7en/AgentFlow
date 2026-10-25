; Never let Tauri's subsequent Restart Manager check force-close an active session.
; Both installation and uninstall stop until the user closes this executable.
!macro AgentFlowRequireClosed
  !insertmacro RestartManager_StartSession $R0
  ${If} $R0 == ""
    SetErrorLevel 2
    Abort "Cannot check for an active AgentFlow session. Close AgentFlow and try again."
  ${EndIf}
  !insertmacro RestartManager_RegisterFile $R0 "$INSTDIR\${MAINBINARYNAME}.exe"
  ${If} $0 = 0
    System::Call 'RSTRTMGR::RmGetList(p R0, *i .r1, *i .r2, p 0, *i .r3) i .r0'
    ${If} $0 <> 0
      !insertmacro RestartManager_EndSession $R0
      ${IfNot} ${Silent}
        MessageBox MB_OK|MB_ICONEXCLAMATION "Close AgentFlow before installing or removing it. Your tasks and configuration will be kept."
      ${EndIf}
      SetErrorLevel 2
      Abort "AgentFlow is running, or its process state could not be checked."
    ${EndIf}
  ${Else}
    !insertmacro RestartManager_EndSession $R0
    SetErrorLevel 2
    Abort "Cannot check the installed executable. Close AgentFlow and try again."
  ${EndIf}
  !insertmacro RestartManager_EndSession $R0
!macroend

; An update starts the installer just before the previous AgentFlow exits. Give that process up to
; ten seconds to finish exiting before the check below refuses to continue.
!macro AgentFlowWaitForUpdatedExit
  ${If} $UpdateMode = 1
    StrCpy $R3 0
    ${Do}
      StrCpy $R4 1
      !insertmacro RestartManager_StartSession $R0
      ${If} $R0 != ""
        !insertmacro RestartManager_RegisterFile $R0 "$INSTDIR\${MAINBINARYNAME}.exe"
        ${If} $0 = 0
          System::Call 'RSTRTMGR::RmGetList(p R0, *i .r1, *i .r2, p 0, *i .r3) i .r0'
          ${IfThen} $0 = 0 ${|} StrCpy $R4 0 ${|}
        ${EndIf}
        !insertmacro RestartManager_EndSession $R0
      ${EndIf}
      ${If} $R4 = 0
      ${OrIf} $R3 >= 40
        ${Break}
      ${EndIf}
      IntOp $R3 $R3 + 1
      Sleep 250
    ${Loop}
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREINSTALL
  ; Silent NSIS installs skip the page that normally calculates Tauri's version comparison.
  ; Check the registered version here as well, before any application files are replaced.
  ReadRegStr $R1 SHCTX "${UNINSTKEY}" "DisplayVersion"
  ${If} $R1 != ""
    nsis_tauri_utils::SemverCompare "${VERSION}" $R1
    Pop $R2
    ${If} $R2 = -1
      ${IfNot} ${Silent}
        MessageBox MB_OK|MB_ICONEXCLAMATION "A newer AgentFlow version is already installed. Keep that version to protect your existing data."
      ${EndIf}
      SetErrorLevel 2
      Abort "A newer AgentFlow version is already installed."
    ${EndIf}
  ${EndIf}
  !insertmacro AgentFlowWaitForUpdatedExit
  !insertmacro AgentFlowRequireClosed
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro AgentFlowRequireClosed
  ; AgentFlow's data lives at the configured location, outside packaged resources.
  ; Retain Tauri's application data as well, even if the standard checkbox was selected.
  StrCpy $DeleteAppDataCheckboxState 0
!macroend
