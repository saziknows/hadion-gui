<#
Takes the README screenshots (docs/screenshots/*.png).

Each scene is one scripted run (HADION_SCRIPT) of a copy of the debug build,
started from an empty state folder (HADION_STATE_DIR: default settings and
panel sizes, never your own) at 1440 x 900, with the pointer parked in an
empty corner (`pointer`), on demo repositories built with a throwaway git
identity. Containers uses the fake_docker example; Review and Actions read
this repository on GitHub through the gh_guard example (writes blocked), so
they need `gh auth` and run only with -GitHub.

  cargo build -p hadion
  cargo build -p hadion-core --example fake_docker --example gh_guard
  ./scripts/readme-shots.ps1 [-Dir <work folder>] [-GitHub] [-Only history,conflict,...]

Look at every picture before committing it: a window on the same desktop
can still be clicked into while a scene runs.
#>
param(
  [string]$Dir = (Join-Path ([IO.Path]::GetTempPath()) "hadion-readme-shots"),
  [switch]$GitHub,
  [string[]]$Only = @()
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$ex = Join-Path $root "target\debug\examples"
New-Item -ItemType Directory -Force $Dir | Out-Null
$repos = Join-Path $Dir "repos"
$out = Join-Path $Dir "shots"
New-Item -ItemType Directory -Force $out | Out-Null
$exe = Join-Path $Dir "hadion-test.exe"
Copy-Item (Join-Path $root "target\debug\hadion.exe") $exe -Force

# Demo repositories, with an identity of their own. Git Bash, not whatever
# `bash` is first on PATH (System32\bash.exe is WSL).
function BashPath($p) { "/" + $p.Substring(0, 1).ToLower() + ($p.Substring(2) -replace '\\', '/') }
$bash = Join-Path (Split-Path (Split-Path (Get-Command git).Source)) "bin\bash.exe"
if (-not (Test-Path $bash)) { throw "Git Bash not found next to git ($bash)" }
$bashDir = BashPath $Dir
$bashRoot = BashPath $root
$build = @"
set -e
export GIT_CONFIG_GLOBAL='$bashDir/gitconfig'
printf '[user]\n\tname = Ada Lovelace\n\temail = ada@example.com\n[init]\n\tdefaultBranch = main\n[commit]\n\tgpgsign = false\n[core]\n\tautocrlf = false\n' > "`$GIT_CONFIG_GLOBAL"
R='$bashDir/repos'; rm -rf "`$R"; mkdir -p "`$R"
bash '$bashRoot/scripts/make-demo-repo.sh' "`$R/acme-web" > /dev/null
mv "`$R/acme-web/work" "`$R/acme-web/acme-web"
bash '$bashRoot/scripts/make-conflict-repos.sh' "`$R/conflicts" > /dev/null
bash '$bashRoot/scripts/make-leaky-repo.sh' "`$R/leaky" > /dev/null
S="`$R/shop"; mkdir -p "`$S/web" "`$S/worker" "`$S/migrate" "`$S/ops" "`$S/extras"; cd "`$S"; git init -q -b main
T=`$(( `$(date +%s) - 20*86400 ))
c() { T=`$((T + 7200 + RANDOM % 30000)); git add -A; GIT_AUTHOR_DATE="`$T +0000" GIT_COMMITTER_DATE="`$T +0000" git commit -q -m "`$1" --author="`$2"; }
printf 'services:\n  web:\n    build: ./web\n    ports: ["8080:80"]\n  db:\n    image: postgres:16-alpine\n  worker:\n    build: ./worker\n  migrate:\n    build: ./migrate\n    profiles: [tools]\nvolumes:\n  pgdata: {}\n  certs: {}\n' > compose.yaml
printf 'FROM nginx:1.27-alpine\n' > web/Dockerfile; printf 'FROM node:22-alpine\n' > worker/Dockerfile; printf 'FROM node:22-alpine\n' > migrate/Dockerfile
c "Compose project for the shop" "Grace Hopper <grace@example.com>"
printf 'services:\n  cache:\n    image: redis:7\n  mail:\n    image: axllent/mailpit:v1.21\n' > ops/compose.yaml; c "Dev tools: cache and mail catcher" "Ada Lovelace <ada@example.com>"
printf 'services:\n  grafana:\n    image: grafana/grafana\n' > extras/docker-compose.yml; c "Dashboards for local runs" "Margaret Hamilton <margaret@example.com>"
echo '<h1>Shop</h1>' > web/index.html; c "Landing page" "Grace Hopper <grace@example.com>"
git rev-parse HEAD > '$bashDir/shop-rev.txt'
echo 'console.log(1)' > worker/worker.js; c "Worker: retry failed jobs" "Linus Benedict <linus@example.com>"
echo 'console.log(2)' > migrate/migrate.js; c "Migrations for orders" "Ada Lovelace <ada@example.com>"
echo '<p>Free shipping</p>' >> web/index.html; c "Banner: free shipping" "Margaret Hamilton <margaret@example.com>"
git remote add origin https://git.example.com/acme/shop.git; git update-ref refs/remotes/origin/main HEAD
"@
$buildFile = Join-Path $Dir "build-repos.sh"
[IO.File]::WriteAllText($buildFile, ($build -replace "`r`n", "`n"))
& $bash $buildFile
if ($LASTEXITCODE -ne 0) { throw "building the demo repositories failed" }

function Scene($name, $repo, $steps, $extra = @{}) {
  if ($Only.Count -and -not ($Only -contains $name)) { return }
  $state = Join-Path $Dir "state\$name"
  Remove-Item $state -Recurse -Force -ErrorAction SilentlyContinue
  New-Item -ItemType Directory -Force $state | Out-Null
  foreach ($v in 'HADION_DOCKER', 'HADION_GH', 'FAKE_DOCKER_REPO', 'FAKE_DOCKER_REVISION', 'FAKE_DOCKER_SOURCE', 'FAKE_DOCKER_SCENARIO') {
    Remove-Item "Env:$v" -ErrorAction SilentlyContinue
  }
  $env:HADION_STATE_DIR = $state; $env:HADION_NO_RESTORE = "1"; $env:HADION_NOTIFY = "off"
  $env:GH_GUARD_LOG = Join-Path $Dir "gh_guard.log"
  foreach ($k in $extra.Keys) { Set-Item "Env:$k" $extra[$k] }
  $env:HADION_SCRIPT = (@("window 1440 900", "pointer 2 898") + $steps + @("quit")) -join "; "
  $p = Start-Process $exe -ArgumentList "`"$repo`"" -PassThru
  if (-not $p.WaitForExit(120000)) { Stop-Process -Id $p.Id -Force; Write-Warning "$name timed out" }
  Write-Output "$name exit $($p.ExitCode)"
}

$shop = Join-Path $repos "shop"
$docker = @{
  HADION_DOCKER = Join-Path $ex "fake_docker.exe"; FAKE_DOCKER_REPO = $shop
  FAKE_DOCKER_REVISION = (Get-Content (Join-Path $Dir "shop-rev.txt")).Trim()
  FAKE_DOCKER_SOURCE = "https://git.example.com/acme/shop"; FAKE_DOCKER_SCENARIO = "normal"
}
$gh = @{ HADION_GH = Join-Path $ex "gh_guard.exe" }
$web = Join-Path $repos "acme-web\acme-web"

Scene "history" $web @("wait 6", "head", "wait 2", "shot $out\history.png")
Scene "history-light" $web @("theme", "rail", "wait 6", "head", "wait 2", "shot $out\history-light.png")
Scene "palette" $web @("wait 6", "palette", "palette-query rebase", "wait 2", "shot $out\palette.png")
Scene "conflict" (Join-Path $repos "conflicts\merge") @("rail", "wait 6", "conflict src/lib.rs", "wait 3", "shot $out\conflict.png")
Scene "containers" $shop @("wait 6", "ship containers", "wait 5", "container 3", "wait 3", "shot $out\containers.png") $docker
Scene "exposure" (Join-Path $repos "leaky") @("wait 6", "security", "scan-history 0", "wait 16", "finding 0", "wait 2", "shot $out\exposure.png")
Scene "insights" (Join-Path $repos "leaky") @("wait 6", "insights", "wait 8", "shot $out\insights.png")
if ($GitHub) {
  Scene "review" $root @("wait 6", "review", "wait 8", "pr 0", "wait 6", "pr-tab files", "wait 5", "shot $out\review.png") $gh
  Scene "actions" $root @("wait 6", "github", "wait 8", "status passed", "run 0", "wait 6", "job 0", "wait 6", "shot $out\actions.png") $gh
}
Copy-Item (Join-Path $out "*.png") (Join-Path $root "docs\screenshots") -Force
Get-ChildItem (Join-Path $root "docs\screenshots") -Filter *.png | Select-Object Name, Length | Format-Table -AutoSize
