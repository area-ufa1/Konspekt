# Поддельный Claude Code для тестов claude_session: читает stream-json из stdin,
# отвечает дельтами и result. Аргументы от обёртки: режим, папка для меток;
# дальше идут флаги claude — их не читаем.
$utf8 = [System.Text.UTF8Encoding]::new($false)
[Console]::InputEncoding = $utf8
[Console]::OutputEncoding = $utf8
$mode = $args[0]
$marks = $args[1]
function Say($json) {
  [Console]::Out.WriteLine($json)
  [Console]::Out.Flush()
}
[System.IO.File]::WriteAllText((Join-Path $marks 'fake.pid'), "$PID")
# Флаги claude, как они дошли (по одному в строке, в скобках): тест видит,
# что пустые значения (`--tools ""`) не потерялись по дороге через cmd.exe.
$flags = for ($i = 2; $i -lt $args.Count; $i++) { "[" + $args[$i] + "]" }
[System.IO.File]::WriteAllLines((Join-Path $marks 'args.txt'), [string[]]@($flags), $utf8)
if ($mode -eq 'stderr-exit') {
  # Как старый Claude Code, не знающий флага: строка в stderr и выход до чтения stdin.
  [Console]::Error.WriteLine("error: unknown option '--include-partial-messages'")
  [Console]::Error.Flush()
  exit 1
}
if ($mode -eq 'print') {
  # Разовый `claude --print --output-format json`: весь stdin — один промпт.
  $prompt = [Console]::In.ReadToEnd()
  Say (@{ type = 'result'; subtype = 'success'; is_error = $false; result = "print len $($prompt.Length)" } | ConvertTo-Json -Compress)
  exit 0
}
if ($mode -eq 'hang') {
  # Как node за npm-обёрткой: внук, который живёт сам по себе, и сам процесс,
  # который не выходит по закрытому stdin. Достать их может только убийство дерева.
  $info = [System.Diagnostics.ProcessStartInfo]::new('powershell.exe', '-NoProfile -Command Start-Sleep -Seconds 600')
  $info.UseShellExecute = $false
  $info.CreateNoWindow = $true
  $grandchild = [System.Diagnostics.Process]::Start($info)
  [System.IO.File]::WriteAllText((Join-Path $marks 'grandchild.pid'), "$($grandchild.Id)")
}
$turn = 0
while ($true) {
  $line = [Console]::In.ReadLine()
  if ($null -eq $line) { break }
  $turn++
  # Что пришло ходом N — тест читает, что именно ушло в Claude. Новый процесс
  # (перезапуск) пишет поверх с хода 1.
  $content = ($line | ConvertFrom-Json).message.content
  [System.IO.File]::WriteAllText((Join-Path $marks "msg$turn.txt"), $content, $utf8)
  if ($turn -eq 1) { New-Item -ItemType File -Force (Join-Path $marks 'started') | Out-Null }
  if ($mode -eq 'crash-second' -and $turn -ge 2) { exit 3 }
  if ($mode -eq 'silent' -or $mode -eq 'stuck') { continue }
  if ($mode -eq 'slow-warmup' -and $turn -eq 1) { Start-Sleep -Seconds 3 }
  if ($mode -eq 'slow') { Start-Sleep -Milliseconds 800 }
  if ($mode -eq 'limit') {
    Say '{"type":"result","subtype":"success","is_error":true,"result":"usage limit reached"}'
    continue
  }
  $len = $content.Length
  $parts = @("turn $turn", " len $len")
  $final = "turn $turn len $len"
  if ($mode -eq 'parrot') {
    # Повторяет присланное: тест видит, что вернулось из Claude.
    $parts = @($content)
    $final = $content
  }
  foreach ($part in $parts) {
    $delta = @{ type = 'stream_event'; event = @{ type = 'content_block_delta'; delta = @{ type = 'text_delta'; text = $part } } } | ConvertTo-Json -Compress -Depth 5
    if ($mode -eq 'split') {
      # Половина строки, пауза, вторая половина — как обрыв между двумя чтениями stdout.
      $half = [int]($delta.Length / 2)
      [Console]::Out.Write($delta.Substring(0, $half))
      [Console]::Out.Flush()
      Start-Sleep -Milliseconds 300
      Say $delta.Substring($half)
    } else {
      Say $delta
    }
  }
  Say (@{ type = 'result'; subtype = 'success'; is_error = $false; result = $final } | ConvertTo-Json -Compress)
}
# `hang` и `stuck` не выходят по закрытому stdin: достать их может только убийство дерева.
if ($mode -eq 'hang' -or $mode -eq 'stuck') { Start-Sleep -Seconds 600 }
# stdin закрыт — вышли сами; тест видит, что процесс закрыт, а не брошен.
New-Item -ItemType File -Force (Join-Path $marks 'exited') | Out-Null
