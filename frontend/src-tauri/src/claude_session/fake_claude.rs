//! Поддельный Claude Code для тестов claude_session (только Windows).
//! У каждого теста своя обёртка `claude.cmd` в своей временной папке, режим
//! вшит в неё аргументом: общей переменной окружения нет, тесты диалога и
//! менеджера идут параллельно без замков.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) struct FakeClaude {
    dir: tempfile::TempDir,
    pub binary: PathBuf,
}

impl FakeClaude {
    pub(crate) fn new(mode: &str) -> FakeClaude {
        Self::in_dir(mode, tempfile::tempdir().unwrap())
    }

    /// Обёртка в папке с пробелом в пути — как `%APPDATA%\npm\claude.cmd`
    /// у пользователя «Иван Петров».
    pub(crate) fn spaced(mode: &str) -> FakeClaude {
        let dir = tempfile::Builder::new().prefix("fake claude ").tempdir().unwrap();
        assert!(dir.path().to_string_lossy().contains(' '));
        Self::in_dir(mode, dir)
    }

    fn in_dir(mode: &str, dir: tempfile::TempDir) -> FakeClaude {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-claude/fake.ps1");
        let binary = dir.path().join("claude.cmd");
        // CRLF: cmd неверно разбирает батники с одними LF.
        let wrapper = format!(
            "@powershell -NoProfile -ExecutionPolicy Bypass -File \"{}\" {} \"{}\" %*\r\n",
            script.display(),
            mode,
            dir.path().display()
        );
        std::fs::write(&binary, wrapper).unwrap();
        FakeClaude { dir, binary }
    }

    /// Подделка прочла первое сообщение (разогрев или первый вопрос).
    pub(crate) fn started(&self) -> bool {
        self.dir.path().join("started").exists()
    }

    /// Подделка дочитала stdin до конца и вышла сама.
    pub(crate) fn exited(&self) -> bool {
        self.dir.path().join("exited").exists()
    }

    /// Текст, который подделка получила ходом `turn` (у перезапущенного процесса — с хода 1).
    pub(crate) fn message(&self, turn: u32) -> Option<String> {
        std::fs::read_to_string(self.dir.path().join(format!("msg{turn}.txt"))).ok()
    }

    /// Флаги claude последнего запуска, как их увидела подделка.
    pub(crate) fn args(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("args.txt"))
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let line = line.trim_start_matches('\u{feff}');
                let line = line.strip_prefix('[').unwrap_or(line);
                line.strip_suffix(']').unwrap_or(line).to_string()
            })
            .collect()
    }

    /// Пара «флаг значение» дошла до подделки именно так.
    pub(crate) fn got_pair(&self, flag: &str, value: &str) -> bool {
        self.args().windows(2).any(|pair| pair[0] == flag && pair[1] == value)
    }

    /// PID, записанный подделкой: `fake` — сам powershell, `grandchild` — внук в режиме `hang`.
    pub(crate) fn pid(&self, name: &str) -> Option<u32> {
        let text = std::fs::read_to_string(self.dir.path().join(format!("{name}.pid"))).ok()?;
        text.trim().parse().ok()
    }
}

/// Жив ли процесс с этим PID.
pub(crate) fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code) != 0;
        CloseHandle(handle);
        ok && code == STILL_ACTIVE as u32
    }
}

impl FakeClaude {
    pub(crate) async fn wait_for(&self, what: fn(&FakeClaude) -> bool, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if what(self) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        what(self)
    }
}
