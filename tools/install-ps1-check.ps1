# What makes `irm https://fennecdb.com/install.ps1 | iex` safe to run, held by
# PowerShell's own parser and not by a reading of its lines: `iex` runs the
# text it is given in the caller's session, so a download cut short runs what
# came before the cut, and the script must be definitions up to its last line.
#
#   pwsh tools/install-ps1-check.ps1 [site/install.ps1]
#
# Run by ci.yml's `windows` job (under Windows PowerShell 5.1 and PowerShell
# 7) and by site/build.py wherever `pwsh` is on the PATH (the site's deploy,
# CI's ubuntu job), which refuses to ship the file when it fails. What it
# holds, from the syntax tree:
#
#   - the file parses with no error;
#   - its top level is `function` definitions, each named for fennec so none
#     shadows a command of the session it is defined in, and then the one
#     last statement, `Install-Fennec @args` -- no param block, no
#     begin/process/end block, no trap, no `using`, no other statement, on
#     whatever line or after whatever `;`;
#   - no command anywhere in it, a function's body and a string's `$( )`
#     included, is Invoke-Expression, iex, Invoke-Command, Add-Type,
#     Start-Job, Start-Process or the like, under any module prefix or
#     quoting; a command whose name is computed is only `& (Join-Path ...)`,
#     the call of a file the script put together;
#   - nothing makes a script block from a text: no [scriptblock] type, no
#     member named Create, NewScriptBlock, InvokeScript, ExpandString,
#     InvokeCommand or Invoke, no member whose name is computed.
#
# Strings and comments are not commands: the usage text names `irm | iex`.
# Every problem is printed as `file:line: what`, and any makes the exit
# status 1. A new bypass is a new case in this list and in the check: the
# refusals are what the parser can say exactly, not a proof that nothing
# else runs -- the file is read in review as well.

param([string] $Path)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

if (-not $Path) {
    $Path = Join-Path (Join-Path (Join-Path $PSScriptRoot '..') 'site') 'install.ps1'
}
$full = (Resolve-Path -LiteralPath $Path).Path
$label = $Path

$script:problems = 0
function Problem([int] $line, [string] $what) {
    Write-Host ($label + ':' + $line + ': ' + $what)
    $script:problems++
}

$tokens = $null
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile($full, [ref] $tokens, [ref] $errors)
foreach ($e in $errors) {
    Problem $e.Extent.StartLineNumber ('does not parse: ' + $e.Message)
}

if ($ast -and -not $errors) {
    # The top level.
    if ($ast.ParamBlock -or $ast.BeginBlock -or $ast.ProcessBlock -or $ast.DynamicParamBlock) {
        Problem $ast.Extent.StartLineNumber 'the top level has a param, begin, process or dynamicparam block'
    }
    if ($ast.UsingStatements -and @($ast.UsingStatements).Count) {
        Problem $ast.UsingStatements[0].Extent.StartLineNumber 'the top level has a using statement'
    }
    $end = $ast.EndBlock
    $statements = @()
    if ($end) {
        if (-not $end.Unnamed) { Problem $end.Extent.StartLineNumber 'the top level is a named end block' }
        if ($end.Traps -and @($end.Traps).Count) { Problem $end.Traps[0].Extent.StartLineNumber 'the top level has a trap' }
        $statements = @($end.Statements)
    }
    $last = $null
    if ($statements.Count) { $last = $statements[$statements.Count - 1] }
    foreach ($s in $statements) {
        $line = $s.Extent.StartLineNumber
        if ($s -is [System.Management.Automation.Language.FunctionDefinitionAst]) {
            if ($s.IsWorkflow -or $s.IsFilter) { Problem $line ('"' + $s.Name + '" is a workflow or a filter, not a function') }
            elseif ($s.Name -cnotmatch '^[A-Za-z]+-Fennec[A-Za-z]*$') { Problem $line ('function "' + $s.Name + '" is not named Verb-Fennec...') }
        }
        elseif ($s -eq $last) {
            if ($s.Extent.Text -cne 'Install-Fennec @args') { Problem $line ('the last statement is [' + $s.Extent.Text + '], not [Install-Fennec @args]') }
        }
        else {
            Problem $line ('outside a function, before the last line: [' + $s.Extent.Text + ']')
        }
    }
    if (-not $last -or $last -is [System.Management.Automation.Language.FunctionDefinitionAst]) {
        Problem 1 'the file does not end by calling Install-Fennec @args'
    }

    # Everything in it, nested script blocks and strings' $( ) included.
    $banned = @(
        'invoke-expression', 'iex', 'invoke-command', 'icm', 'add-type',
        'start-job', 'sajb', 'start-threadjob', 'start-process', 'saps',
        'set-alias', 'sal', 'new-alias', 'nal', 'new-module', 'import-module'
    )
    $bannedMembers = @(
        'create', 'newscriptblock', 'invokescript', 'expandstring',
        'invokecommand', 'invoke', 'invokereturnasis', 'invokewithcontext'
    )
    $all = $ast.FindAll({ param($n) $true }, $true)
    foreach ($n in $all) {
        $line = $n.Extent.StartLineNumber
        if ($n -is [System.Management.Automation.Language.CommandAst]) {
            $name = $n.GetCommandName()
            if ($name) {
                # Microsoft.PowerShell.Utility\Invoke-Expression is the same command.
                $bare = $name.Substring($name.LastIndexOf('\') + 1).ToLowerInvariant()
                if ($banned -contains $bare) { Problem $line ('the command ' + $name + ' is refused') }
            }
            else {
                $first = $n.CommandElements[0]
                $inner = $null
                if ($first -is [System.Management.Automation.Language.ParenExpressionAst]) {
                    $p = $first.Pipeline
                    if ($p -is [System.Management.Automation.Language.PipelineAst] -and @($p.PipelineElements).Count -eq 1 `
                            -and $p.PipelineElements[0] -is [System.Management.Automation.Language.CommandAst]) {
                        $inner = $p.PipelineElements[0].GetCommandName()
                    }
                }
                $amp = $n.InvocationOperator -eq [System.Management.Automation.Language.TokenKind]::Ampersand
                if (-not ($amp -and $inner -and ($inner -ieq 'Join-Path'))) {
                    Problem $line ('a command whose name is computed, only & (Join-Path ...) is allowed: [' + $n.Extent.Text + ']')
                }
            }
        }
        elseif ($n -is [System.Management.Automation.Language.MemberExpressionAst]) {
            if ($n.Member -is [System.Management.Automation.Language.StringConstantExpressionAst]) {
                if ($bannedMembers -contains $n.Member.Value.ToLowerInvariant()) { Problem $line ('the member ' + $n.Member.Value + ' is refused') }
            }
            else {
                Problem $line ('a member whose name is computed: [' + $n.Extent.Text + ']')
            }
        }
        elseif ($n -is [System.Management.Automation.Language.TypeExpressionAst] -or $n -is [System.Management.Automation.Language.TypeConstraintAst]) {
            if ($n.TypeName.FullName -match '(?i)(^|\.)scriptblock(\[\])?$') { Problem $line 'the [scriptblock] type is refused' }
        }
    }
}

Write-Host ($PSVersionTable.PSVersion.ToString() + ': ' + $label + ', ' + $script:problems + ' problems')
if ($script:problems) { exit 1 }
