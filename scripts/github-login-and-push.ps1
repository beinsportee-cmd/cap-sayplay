# Run in PowerShell (Cursor terminal). Use when browser device login is blocked.
# 1) On phone or home PC: https://github.com/settings/tokens → Generate new token (classic)
#    Scopes: repo, workflow, read:org. Copy the token (shown once).
# 2) Run:  cd E:\nosave\recording-less\cap-sayplay
#          .\scripts\github-login-and-push.ps1

$ErrorActionPreference = "Stop"
$env:Path = [System.Environment]::GetEnvironmentVariable("Path", "Machine") + ";" +
  [System.Environment]::GetEnvironmentVariable("Path", "User")

if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
  Write-Error "Install GitHub CLI first: winget install GitHub.cli"
}

$token = Read-Host "Paste GitHub token (input hidden)" -AsSecureString
$bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($token)
$plain = [Runtime.InteropServices.Marshal]::PtrToStringAuto($bstr)
[Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr)

$plain | gh auth login --with-token
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

gh auth status
$user = (gh api user -q .login)
$repoName = "cap-sayplay"
$exists = gh repo view "$user/$repoName" 2>$null
if (-not $exists) {
  gh repo create $repoName --public --description "SayPlay Recorder (Cap AGPL fork for school Studio recording)" --source=. --remote=origin
} else {
  if (-not (git remote get-url origin 2>$null)) {
    git remote add origin "https://github.com/$user/$repoName.git"
  }
}

git push -u origin sayplay
Write-Host ""
Write-Host "Done. Repo: https://github.com/$user/$repoName"
Write-Host "Run Windows build: Actions → build-windows-sayplay → Run workflow (branch sayplay)"
