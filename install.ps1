# Install the client. Print the layout block for the plugin that goes with it.
# This script is the Windows half of install.sh, with the same shape. One program
# holds the hook that the agents invoke, the daemon that those hooks feed, and
# this installer. This script does not start the daemon. This script does not
# download the plugin. Zellij fetches the plugin itself from the url in the
# layout.
#
# The script installs that program twice, as agent-wrangler.exe and as
# agent-wranglerw.exe. Both files come from the same build, with one difference.
# The link of the second file tells Windows to give it no console. A program
# without a console of its own can run the second file, and Windows draws no
# window. The agents and the zellij server are such programs, so the hooks and
# the layout name the second file. The first file remains for a run by hand.
#
#   irm https://raw.githubusercontent.com/JimiSmith/zellij-agent-wrangler/main/install.ps1 | iex
#
# A piped script takes no arguments. To pass an argument, run the script as a
# script block:
#
#   & ([scriptblock]::Create((irm <url>))) -Version v0.1.16 -AddToPath
[CmdletBinding()]
param(
    # The release to install. Without this parameter, the script takes the
    # latest release.
    [string]$Version,

    # The directory for the client. The client goes under Programs and not
    # beside the scripts of the user. The client is a downloaded binary and not
    # a file that the user wrote. On an ordinary account this path holds no
    # space. The quotes around the hook commands in the configs of the agents
    # cover a path with a space.
    [string]$Bin = $(if ($env:AGENT_WRANGLER_BIN) { $env:AGENT_WRANGLER_BIN }
                     elseif ($env:LOCALAPPDATA) { Join-Path $env:LOCALAPPDATA 'Programs\agent-wrangler' }
                     else { Join-Path $env:USERPROFILE '.local\bin' }),

    # Put $Bin on the PATH of the user as well. The default is off. The sidebar
    # gets the whole path of the client and never needs PATH. With this switch,
    # you can run `agent-wrangler agents` by name in a shell. The switch also
    # edits the environment that every later process inherits.
    [switch]$AddToPath
)

$ErrorActionPreference = 'Stop'
# Invoke-WebRequest draws a progress bar for each chunk in Windows PowerShell.
# That bar costs more than the download.
$ProgressPreference = 'SilentlyContinue'

$repo = 'JimiSmith/zellij-agent-wrangler'

if ($PSVersionTable.PSVersion.Major -lt 5) {
    throw "this needs PowerShell 5 or later. this is $($PSVersionTable.PSVersion)."
}

# Windows PowerShell defaults to protocols that github no longer answers on.
# This code adds to the current value and does not replace it. A session with
# its own configuration stays as it is.
try {
    [Net.ServicePointManager]::SecurityProtocol =
        [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
} catch {
    # PowerShell 7 negotiates this itself, and the type can be read-only.
}

# PROCESSOR_ARCHITECTURE holds the architecture of *this process*. A 32-bit
# PowerShell on a 64-bit machine therefore reports x86. PROCESSOR_ARCHITEW6432
# holds the architecture of the machine. It is set only for a difference between
# the two.
$arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
switch ($arch) {
    'AMD64' { $target = 'x86_64-pc-windows-msvc' }
    # There is no released arm64 client. Windows on arm runs the x64 client
    # under emulation. This program reads the process table and talks to a named
    # pipe, so the emulation costs too little for a second build.
    'ARM64' { $target = 'x86_64-pc-windows-msvc' }
    default {
        throw "no released client for $arch. build the client: cargo build --release -p agent-wrangler"
    }
}

# This script uses `gh` where `gh` is present, because `gh` also reaches a
# private repository. On a machine without `gh`, the api is the fallback.
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
if (-not $Version) { throw 'the script did not find the latest version. set it with -Version.' }

$client = "agent-wrangler-$Version-$target.exe"
$windowless = "agent-wranglerw-$Version-$target.exe"
$wasm = "zellij-agent-wrangler-$Version.wasm"

New-Item -ItemType Directory -Force -Path $Bin | Out-Null
$exe = Join-Path $Bin 'agent-wrangler.exe'
$quiet = Join-Path $Bin 'agent-wranglerw.exe'

# One released file into one installed name.
#
# The download goes beside the real file. A move then puts it in place. This
# function never writes to the real file. A download that stops halfway
# therefore leaves nothing to run.
#
# A move is also the only way on Windows. Windows cannot delete or overwrite a
# running image, and the daemon under replacement very probably runs from that
# exact file. Windows can rename a running image. The function therefore moves
# the old file aside and puts the new file in the name of the old one. The
# program that runs continues from its own file under the new name. The next run
# gets the whole new build.
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
            if ($LASTEXITCODE -ne 0) { throw "gh did not download $Asset from $Version." }
        } else {
            Invoke-WebRequest -UseBasicParsing -OutFile $temp `
                -Uri "https://github.com/$repo/releases/download/$Version/$Asset"
        }
        # A file from the internet carries a zone marker, and SmartScreen stops
        # a marked executable. This script clears the marker and does not leave
        # it to the user. A hook inside the turn of an agent runs the file. A
        # blocked run there gives a row that never appears, with no message.
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

# Install both files, or neither. The hooks and the layout name the windowless
# file. An install of the console file alone leaves every hook with the name of
# a file that is absent.
Install-Binary -Asset $client -Path $exe
Install-Binary -Asset $windowless -Path $quiet

# This command removes every file that a run moved aside, from this run and from
# an earlier run. A file that a daemon still runs from refuses to go, and it
# stays for the next install. By that time a hook started the daemon again from
# the new build.
Get-ChildItem -LiteralPath $Bin -Filter '.agent-wrangler*.old.*.exe' -Force -ErrorAction SilentlyContinue |
    ForEach-Object { Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue }

& $exe install-hooks
if ($LASTEXITCODE -ne 0) { throw 'the client did not install the hooks.' }

if ($AddToPath) {
    # This code writes the PATH of the user and not the PATH of the machine.
    # The install is for one user. A write to the PATH of the machine needs an
    # elevated shell.
    $user = [Environment]::GetEnvironmentVariable('Path', 'User')
    $entries = @($user -split ';' | Where-Object { $_ })
    if ($entries -notcontains $Bin) {
        [Environment]::SetEnvironmentVariable('Path', (@($entries) + $Bin) -join ';', 'User')
        # The change reaches the processes that start after it, and this
        # session is not one of them. This line sets the value here as well, so
        # the commands below can run without another shell.
        $env:Path = "$env:Path;$Bin"
        $path = "This script put $Bin on your PATH.
A shell that is already open keeps the old PATH."
    } else {
        $path = "$Bin is already on your PATH."
    }
} else {
    $path = "$Bin is not on your PATH. The commands 'agent-wrangler agents' and
'agent-wrangler monitor' therefore need the full path. To put $Bin on your PATH,
run this script again with -AddToPath. The sidebar does not need your PATH,
because the block below gives the sidebar the full path."
}

# The block always names the client in full. The sidebar runs the client to
# reach the daemon. Without a path, the sidebar looks on PATH. The PATH that
# matters belongs to the zellij server, which took it from the program that
# started zellij. That program is not always the shell of this script.
#
# The block names the windowless file, because a run under the server is the
# exact case that draws a console window. The sidebar runs the client once for
# every tab that it opens, and the server above it has no console for a child.
#
# The path carries doubled separators. The block goes into KDL, where a quoted
# string takes backslash escapes. A raw Windows path there becomes a path with
# `\U` and `\a` in it, and not the path of this install.
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
    $note = "Your PATH finds a different agent-wrangler first ($found).
The block names the client that this script installed. The sidebar therefore
runs that client, whatever the PATH says."
} else {
    $note = "The block names the windowless client that this script installed. The
windowless client draws no console window when the sidebar runs it.
If $Bin is on your PATH, still keep the full path in the block.
Zellij must find the client, and zellij takes its environment from the program
that started zellij, and not from this shell."
}

$installed = (& $exe --version) -join ' '

Write-Host @"

This script installed $installed to $Bin. The file agent-wrangler.exe is the
client, for a run by hand. The hooks and the sidebar run the windowless client,
agent-wranglerw.exe. The windowless client draws no window.

$path

To give every tab a sidebar, put this block in your zellij layout. Put the block
inside both default_tab_template and new_tab_template.

$block

$note

The block is for a zellij that runs as this user. For the pipe of the daemon, a
zellij under WSL is a different machine. To get a sidebar there, install the
client under WSL with install.sh. Then name that client in the layout there.

Zellij downloads the plugin once and holds it. To update, run this script
again. Then change the version in the url to match. Zellij tells one build of
the plugin from another by the url.
"@
