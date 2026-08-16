# Install the client, and print the layout block for the plugin that goes with
# it. The Windows half of install.sh, and the same shape: one program holding the
# hook the agents invoke, the daemon those hooks feed, and this installer.
# Nothing starts the daemon here, and nothing downloads the plugin here; zellij
# fetches that itself from the url in the layout.
#
# That program is installed twice, as agent-wrangler.exe and agent-wranglerw.exe.
# They are the same build differing in one thing: the second is linked so that
# Windows never gives it a console, and so never draws a window for it when
# something that has no console of its own runs it. That is what the agents and
# the zellij server do, so it is the second that the hooks and the layout name,
# and the first that is left for running by hand.
#
#   irm https://raw.githubusercontent.com/JimiSmith/zellij-agent-wrangler/main/install.ps1 | iex
#
# A piped script takes no arguments. To pass one, run it as a script block:
#
#   & ([scriptblock]::Create((irm <url>))) -Version v0.1.10 -AddToPath
[CmdletBinding()]
param(
    # The release to install. The latest one when this is not given.
    [string]$Version,

    # Where the client goes. Under Programs rather than beside the user's own
    # scripts because this is a downloaded binary rather than something they
    # wrote, and it is a path with no space in it on an ordinary account, which
    # is what the hook commands written into the agents' configs are quoted for.
    [string]$Bin = $(if ($env:AGENT_WRANGLER_BIN) { $env:AGENT_WRANGLER_BIN }
                     elseif ($env:LOCALAPPDATA) { Join-Path $env:LOCALAPPDATA 'Programs\agent-wrangler' }
                     else { Join-Path $env:USERPROFILE '.local\bin' }),

    # Put $Bin on the user's PATH as well. Off by default: the sidebar is told
    # the client's whole path and never needs PATH, so this only buys running
    # `agent-wrangler agents` by name from a shell, and it edits the environment
    # every later process inherits.
    [switch]$AddToPath
)

$ErrorActionPreference = 'Stop'
# Invoke-WebRequest draws a progress bar per chunk in Windows PowerShell, which
# costs more than the download does.
$ProgressPreference = 'SilentlyContinue'

$repo = 'JimiSmith/zellij-agent-wrangler'

if ($PSVersionTable.PSVersion.Major -lt 5) {
    throw "this needs PowerShell 5 or later; this is $($PSVersionTable.PSVersion)."
}

# Windows PowerShell defaults to protocols github stopped answering on. Adding
# to what is already there rather than replacing it leaves a session that has
# already been configured alone.
try {
    [Net.ServicePointManager]::SecurityProtocol =
        [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
} catch {
    # PowerShell 7 negotiates this itself and the type may not be settable.
}

# PROCESSOR_ARCHITECTURE is the architecture of *this process*, so a 32-bit
# PowerShell on a 64-bit machine says x86; PROCESSOR_ARCHITEW6432 is what the
# machine actually is, and is only set when the two differ.
$arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
switch ($arch) {
    'AMD64' { $target = 'x86_64-pc-windows-msvc' }
    # No arm64 client is released. Windows on arm runs the x64 one under
    # emulation, which for a program that reads the process table and talks to a
    # named pipe costs nothing worth a second build.
    'ARM64' { $target = 'x86_64-pc-windows-msvc' }
    default {
        throw "no released client for $arch. build one: cargo build --release -p agent-wrangler"
    }
}

# `gh` is used where it is there, because it is also what reaches a private
# repository; the api is the fallback for a machine without it.
$gh = (Get-Command gh -CommandType Application -ErrorAction SilentlyContinue) | Select-Object -First 1

if (-not $Version) {
    if ($gh) {
        $Version = (& $gh.Source release view --repo $repo --json tagName -q .tagName) | Select-Object -First 1
        if ($LASTEXITCODE -ne 0) { $Version = $null }
    } else {
        $Version = (Invoke-RestMethod -UseBasicParsing `
            -Uri "https://api.github.com/repos/$repo/releases/latest").tag_name
    }
}
if (-not $Version) { throw 'could not work out the latest version.' }

$client = "agent-wrangler-$Version-$target.exe"
$windowless = "agent-wranglerw-$Version-$target.exe"
$wasm = "zellij-agent-wrangler-$Version.wasm"

New-Item -ItemType Directory -Force -Path $Bin | Out-Null
$exe = Join-Path $Bin 'agent-wrangler.exe'
$quiet = Join-Path $Bin 'agent-wranglerw.exe'

# One released file into one installed name.
#
# Downloaded beside the real one and moved over it, never written to it: a
# download that dies halfway leaves nothing behind that could be run.
#
# Moving over it is also the only way there is on Windows. A running image
# cannot be deleted or overwritten - the daemon this is replacing is very likely
# running from that exact file - but it can be renamed, so the old one is moved
# aside first and the new one put in the name it left. What is running goes on
# running from the file it started as, under its new name, and the next run gets
# the new build whole.
function Install-Binary {
    param(
        [Parameter(Mandatory)][string]$Asset,
        [Parameter(Mandatory)][string]$Path
    )

    $name = [IO.Path]::GetFileNameWithoutExtension($Path)
    $temp = Join-Path $Bin ".$name.$PID.exe"
    try {
        if ($gh) {
            & $gh.Source release download $Version --repo $repo --pattern $Asset --output $temp --clobber
            if ($LASTEXITCODE -ne 0) { throw "gh could not download $Asset from $Version." }
        } else {
            Invoke-WebRequest -UseBasicParsing -OutFile $temp `
                -Uri "https://github.com/$repo/releases/download/$Version/$Asset"
        }
        # A file fetched from the internet carries a zone marker, and a marked
        # executable is what SmartScreen stops. Cleared here rather than left for
        # the user, because the thing that runs it is a hook inside an agent's
        # turn, where a blocked run is a row that never appears and says nothing.
        Unblock-File -LiteralPath $temp

        $aside = $null
        if (Test-Path -LiteralPath $Path) {
            $aside = Join-Path $Bin ".$name.old.$([guid]::NewGuid().ToString('N').Substring(0, 8)).exe"
            Move-Item -LiteralPath $Path -Destination $aside
        }
        try {
            Move-Item -LiteralPath $temp -Destination $Path
        } catch {
            if ($aside) { Move-Item -LiteralPath $aside -Destination $Path }
            throw
        }
    } finally {
        if (Test-Path -LiteralPath $temp) { Remove-Item -LiteralPath $temp -Force -ErrorAction SilentlyContinue }
    }
}

# Both, or neither worth having: the hooks and the layout name the windowless
# one, so installing the console one alone leaves every hook naming a file that
# is not there.
Install-Binary -Asset $client -Path $exe
Install-Binary -Asset $windowless -Path $quiet

# Every version moved aside, this run's and any left by an earlier one. The one
# a daemon is still running from refuses to go and is left for the next install,
# by which time that daemon has been restarted by a hook running the new build.
Get-ChildItem -LiteralPath $Bin -Filter '.agent-wrangler*.old.*.exe' -Force -ErrorAction SilentlyContinue |
    ForEach-Object { Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue }

& $exe install-hooks
if ($LASTEXITCODE -ne 0) { throw 'the hooks could not be installed.' }

if ($AddToPath) {
    # The user's own PATH, not the machine's: this installs for one user, and
    # writing the machine's would need an elevated shell to do it.
    $user = [Environment]::GetEnvironmentVariable('Path', 'User')
    $entries = @($user -split ';' | Where-Object { $_ })
    if ($entries -notcontains $Bin) {
        [Environment]::SetEnvironmentVariable('Path', (@($entries) + $Bin) -join ';', 'User')
        # The change reaches processes started after it, which is not this
        # session; setting it here as well means the lines below can be run
        # without opening another shell.
        $env:Path = "$env:Path;$Bin"
        $path = "$Bin was added to your PATH. Shells already open still have the old one."
    } else {
        $path = "$Bin was already on your PATH."
    }
} else {
    $path = "$Bin is not on your PATH, so 'agent-wrangler agents' and 'monitor' want its
full path. Run this again with -AddToPath to put it on, or leave it: nothing but
you looks there, and the sidebar is told the path outright."
}

# The block names the client outright, always. The sidebar reaches the daemon by
# running it, and left to itself it looks on PATH - but the one that matters is
# the zellij server's, inherited from whatever started zellij, which is not
# necessarily the shell this script is running in.
#
# It names the windowless one, because the server running it is the very case
# that draws a console window: the sidebar runs the client once for every tab it
# opens, and the server it is running under has no console for a child to be
# given.
#
# The path is written with its separators doubled: what the block goes into is
# KDL, where a quoted string takes backslash escapes, so a Windows path put in
# raw is a path with `\U` and `\a` in it rather than the one this just installed.
$url = "https://github.com/$repo/releases/download/$Version/$wasm"
$kdl = $quiet -replace '\\', '\\'
$block = @"
    pane size=32 borderless=true {
        plugin location="$url" {
            install_hooks "$kdl"
        }
    }
"@

$found = (Get-Command agent-wrangler -CommandType Application -ErrorAction SilentlyContinue |
    Select-Object -First 1).Source
if ($found -and $found -ne $exe) {
    $note = "Note that your PATH finds a different agent-wrangler first ($found).
The block names the one this script just installed, so the sidebar runs that
one whatever the path says."
} else {
    $note = "That names the windowless client this script installed, which is what
keeps a console window from flashing up each time the sidebar runs it. Leave the
path in even if $Bin is on your PATH: what has to find it is zellij, whose
environment comes from whatever started it rather than from this shell."
}

$installed = (& $exe --version) -join ' '

Write-Host @"

Installed $installed to $Bin, as agent-wrangler.exe to run yourself and
agent-wranglerw.exe for the hooks and the sidebar to run without a window.

$path

Give every tab a sidebar by putting this in your zellij layout, inside both
default_tab_template and new_tab_template:

$block

$note

The block is for a zellij running as this user. A zellij under WSL is a
different machine as far as the daemon's pipe is concerned: install the client
there too, from install.sh, and let its layout name that one.

Zellij downloads the plugin once and holds it. Updating means running this
script again and changing the version in the url to match: the url is what
zellij tells one build of the plugin from another.
"@
