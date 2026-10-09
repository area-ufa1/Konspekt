// Чистая логика статуса Claude Code: без Tauri и алиасов, её грузит node --test.

export interface ClaudeCliAuth {
  loggedIn: boolean;
  authMethod?: string | null;
  subscriptionType?: string | null;
  email?: string | null;
  orgName?: string | null;
}

export interface ClaudeCliStatus {
  installed: boolean;
  path?: string | null;
  version?: string | null;
  auth?: ClaudeCliAuth | null;
  /** ANTHROPIC_API_KEY задан — CLI будет тратить ключ, а не подписку. */
  api_key_env_detected: boolean;
  error?: string | null;
}

/** Готов, когда установлен и выполнен вход. */
export function isClaudeCliReady(status: ClaudeCliStatus | null): boolean {
  return !!status?.installed && !!status.auth?.loggedIn;
}

/** Что мешает пользоваться провайдером — код; текст даёт компонент через i18n. */
export function claudeCliBlockingReason(status: ClaudeCliStatus | null): 'not-installed' | 'not-logged-in' | null {
  if (!status || !status.installed) return 'not-installed';
  if (!status.auth?.loggedIn) return 'not-logged-in';
  return null;
}

/** Haiku 5.5 появился в Claude Code 2.1.293; "2.1.292 (Claude Code)" → false. */
export function supportsHaiku55(version: string | null | undefined): boolean {
  const match = /(\d+)\.(\d+)\.(\d+)/.exec(version ?? '');
  if (!match) return false;
  const [major, minor, patch] = match.slice(1).map(Number);
  if (major !== 2) return major > 2;
  if (minor !== 1) return minor > 1;
  return patch >= 293;
}
