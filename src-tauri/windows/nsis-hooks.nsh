!macro NSIS_HOOK_POSTINSTALL
  nsExec::ExecToLog '"$INSTDIR\logcrate_index_service.exe" --install'
  Pop $0
  ${If} $0 != 0
    DetailPrint "LogCrate Index Service install/repair returned $0"
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::ExecToLog '"$INSTDIR\logcrate_index_service.exe" --uninstall'
  Pop $0
  DetailPrint "LogCrate Index Service uninstall returned $0"
!macroend
