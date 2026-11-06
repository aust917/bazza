$ErrorActionPreference = 'Stop'
[Console]::InputEncoding = [Text.UTF8Encoding]::new($false)
try {
    $spec = [Console]::In.ReadToEnd() | ConvertFrom-Json
    if ($spec.task_name -notmatch '^BazzaService-[a-fA-F0-9]{32}$') { throw 'Invalid task name' }
    $sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    $existing = Get-ScheduledTask -TaskName $spec.task_name -TaskPath '\' -ErrorAction SilentlyContinue
    if ($existing) {
        $principalId = $existing.Principal.UserId
        if ($principalId -notlike 'S-1-*') { $principalId = ([System.Security.Principal.NTAccount]::new($principalId)).Translate([System.Security.Principal.SecurityIdentifier]).Value }
        if ($existing.Actions.Count -ne 1 -or $existing.Actions[0].Execute -cne $spec.executable -or $existing.Actions[0].Arguments -cne $spec.arguments -or $principalId -ne $sid -or $existing.Principal.RunLevel -ne 'Limited' -or $existing.Principal.LogonType -ne 'Interactive' -or $existing.Triggers.Count -ne 0) { throw 'Broker definition mismatch' }
        if (-not $existing.Settings.AllowDemandStart -or $existing.Settings.ExecutionTimeLimit -ne 'PT0S' -or $existing.Settings.MultipleInstances -ne 'IgnoreNew' -or -not $existing.Settings.Hidden -or $existing.Settings.DisallowStartIfOnBatteries -or $existing.Settings.StopIfGoingOnBatteries) { throw 'Broker lifetime settings mismatch' }
    }
    switch ($env:BAZZA_BROKER_OPERATION) {
        'register' {
            if (-not $existing) {
                $action = New-ScheduledTaskAction -Execute $spec.executable -Argument $spec.arguments
                $principal = New-ScheduledTaskPrincipal -UserId $sid -LogonType Interactive -RunLevel Limited
                $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew -Hidden
                $task = New-ScheduledTask -Action $action -Principal $principal -Settings $settings
                Register-ScheduledTask -TaskName $spec.task_name -TaskPath '\' -InputObject $task | Out-Null
            }
        }
        'start' { if (-not $existing) { throw 'Broker not registered' }; Start-ScheduledTask -TaskName $spec.task_name -TaskPath '\' }
        'unregister' { if ($existing) { Stop-ScheduledTask -TaskName $spec.task_name -TaskPath '\'; Unregister-ScheduledTask -TaskName $spec.task_name -TaskPath '\' -Confirm:$false } }
        'unregister-idle' {
            if ($existing) {
                if ($existing.State -notin @('Ready', 'Disabled')) { throw 'Broker service is active' }
                Unregister-ScheduledTask -TaskName $spec.task_name -TaskPath '\' -Confirm:$false
            }
        }
        default { throw 'Unsupported broker operation' }
    }
} catch {
    [Console]::Error.WriteLine('Bazza broker control failed')
    exit 1
}
