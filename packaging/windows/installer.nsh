; The host runs detached and keeps its executable open, so stop it before
; files are replaced or removed. Wallpapers return when the editor starts it.
!macro customInit
  nsExec::Exec 'taskkill /F /IM rinka-host.exe'
!macroend

!macro customUnInit
  nsExec::Exec 'taskkill /F /IM rinka-host.exe'
!macroend
