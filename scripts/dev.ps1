# Start backend (with auto-reload) and the Vite dev server side by side.
$root = Split-Path -Parent $PSScriptRoot
Start-Process pwsh -ArgumentList "-NoExit", "-Command", "Set-Location '$root\backend'; .\.venv\Scripts\python.exe -m agentflow.main --reload"
Start-Process pwsh -ArgumentList "-NoExit", "-Command", "Set-Location '$root\frontend'; npm run dev"
