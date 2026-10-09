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
        let dir = tempfile::tempdir().unwrap();
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
