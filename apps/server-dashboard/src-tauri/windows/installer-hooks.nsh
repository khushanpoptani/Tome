!macro NSIS_HOOK_PREINSTALL
  DetailPrint "Removing earlier Tome Server firewall rules"
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="Tome Server (Private LAN)"'
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="Tome Server (Tailscale)"'
!macroend

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "Adding restricted Tome Server firewall rules for TCP 7331"
  nsExec::ExecToLog 'netsh advfirewall firewall add rule name="Tome Server (Private LAN)" dir=in action=allow protocol=TCP localport=7331 program="$INSTDIR\${MAINBINARYNAME}.exe" profile=private remoteip=LocalSubnet enable=yes'
  nsExec::ExecToLog 'netsh advfirewall firewall add rule name="Tome Server (Tailscale)" dir=in action=allow protocol=TCP localport=7331 program="$INSTDIR\${MAINBINARYNAME}.exe" profile=any remoteip=100.64.0.0-100.127.255.255 enable=yes'
  ${IfNot} ${Silent}
    ExecShell "open" "$INSTDIR\${MAINBINARYNAME}.exe"
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  DetailPrint "Removing Tome Server firewall rules"
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="Tome Server (Private LAN)"'
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="Tome Server (Tailscale)"'
  Delete "$SMSTARTUP\Tome Server.lnk"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Tome Server"
  ${IfNot} ${Silent}
    MessageBox MB_YESNO|MB_ICONQUESTION "Remove Tome Server settings, job history, and local data? Choose No to keep your data for a later reinstall." IDNO keep_tome_data
    nsExec::ExecToLog '"$INSTDIR\${MAINBINARYNAME}.exe" --remove-local-data'
    RMDir /r "$LOCALAPPDATA\com.khushanpoptani.tome-server"
    keep_tome_data:
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  DetailPrint "Tome Server uninstall cleanup complete"
!macroend
