; Tauri includes this file right after `SetCompressor /SOLID lzma`, before any
; data is compressed. A 128 MB window lets LZMA match cap-cli.exe against
; Cap.exe, which share most of their code; the default 8 MB window cannot.
SetCompressorDictSize 128

; Cap 0.6.0 ran its GPUI app as cap-gpui.exe next to Cap.exe. The template
; only closes Cap.exe before copying files, so a running cap-gpui.exe would keep
; the shared FFmpeg DLLs locked and stop the update with a file-in-use error.
!macro NSIS_HOOK_PREINSTALL
  !insertmacro CheckIfAppIsRunning "cap-gpui.exe" "${PRODUCTNAME}"
  !insertmacro CheckIfAppIsRunning "SayPlay Recorder.exe" "${PRODUCTNAME}"
  !insertmacro CheckIfAppIsRunning "Cap.exe" "Cap"
!macroend

!macro NSIS_HOOK_POSTINSTALL
  Delete "$INSTDIR\cap-gpui.exe"
  Delete "$INSTDIR\cap-exporter.exe"
  RMDir /r "$INSTDIR\assets\rive"

  ; The template's file association leaves the program path unquoted, and it
  ; has spaces. Quote it so a double-clicked .sayplay file reaches the app as
  ; one argument. Uninstall deletes this key with the rest of the class.
  WriteRegStr SHCTX "Software\Classes\SayPlayRecorder.ProjectFile\shell\open\command" "" '"$INSTDIR\${MAINBINARYNAME}.exe" "%1"'
  !insertmacro UPDATEFILEASSOC
!macroend
