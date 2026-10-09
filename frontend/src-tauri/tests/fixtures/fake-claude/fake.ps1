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
$turn = 0
while ($true) {
  $line = [Console]::In.ReadLine()
  if ($null -eq $line) { break }
  $turn++
  if ($turn -eq 1) { New-Item -ItemType File -Force (Join-Path $marks 'started') | Out-Null }
  if ($mode -eq 'crash-second' -and $turn -ge 2) { exit 3 }
  if ($mode -eq 'silent') { continue }
  if ($mode -eq 'slow-warmup' -and $turn -eq 1) { Start-Sleep -Seconds 3 }
  if ($mode -eq 'slow') { Start-Sleep -Milliseconds 800 }
  if ($mode -eq 'limit') {
    Say '{"type":"result","subtype":"success","is_error":true,"result":"usage limit reached"}'
    continue
  }
  $content = ($line | ConvertFrom-Json).message.content
  $len = $content.Length
  foreach ($part in @("turn $turn", " len $len")) {
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
  Say (@{ type = 'result'; subtype = 'success'; is_error = $false; result = "turn $turn len $len" } | ConvertTo-Json -Compress)
}
# stdin закрыт — вышли сами; тест видит, что процесс закрыт, а не брошен.
New-Item -ItemType File -Force (Join-Path $marks 'exited') | Out-Null
