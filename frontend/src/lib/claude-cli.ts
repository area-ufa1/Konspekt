/**
 * Claude Code CLI provider helpers.
 *
 * The 'claude-cli' summary provider runs the user's locally installed `claude`
 * executable instead of calling the Anthropic API, so summaries draw on a Claude
 * subscription rather than a pay-as-you-go key. Nothing here holds credentials —
 * the CLI owns sign-in — so the UI only ever asks the backend about status.
 */

import { invoke } from '@tauri-apps/api/core';
import type { ClaudeCliStatus } from './claude-cli-status';
export * from './claude-cli-status';

export interface ClaudeCliModel {
  id: string;
  display_name: string;
}

export interface ClaudeCliTestResult {
  status: string;
  message: string;
}

/** Model aliases shown in the picker; resolved by the CLI at call time. */
export const CLAUDE_CLI_FALLBACK_MODELS = ['sonnet', 'opus', 'haiku', 'default'];

export const CLAUDE_CLI_DEFAULT_MODEL = 'sonnet';

/** Where to send someone who does not have the CLI yet. */
export const CLAUDE_CODE_INSTALL_URL = 'https://claude.com/claude-code';

/**
 * Detect the CLI and read its sign-in state. Free — runs `--version` and
 * `auth status`, never a model.
 */
export async function getClaudeCliStatus(path?: string | null): Promise<ClaudeCliStatus> {
  return invoke<ClaudeCliStatus>('claude_cli_get_status', { path: path ?? null });
}

export async function listClaudeCliModels(): Promise<ClaudeCliModel[]> {
  return invoke<ClaudeCliModel[]>('claude_cli_list_models');
}

export async function getClaudeCliPath(): Promise<string | null> {
  return invoke<string | null>('claude_cli_get_path');
}

export async function saveClaudeCliPath(path: string | null): Promise<void> {
  return invoke<void>('claude_cli_save_path', { path });
}

/** Send a one-word prompt through the CLI. Costs a little subscription usage. */
export async function testClaudeCliConnection(
  path?: string | null,
  model?: string | null
): Promise<ClaudeCliTestResult> {
  return invoke<ClaudeCliTestResult>('claude_cli_test_connection', {
    path: path ?? null,
    model: model ?? null,
  });
}

export async function getClaudeLiveModel(): Promise<string> {
  return invoke<string>('claude_cli_get_live_model');
}

export async function saveClaudeLiveModel(model: string): Promise<void> {
  await invoke('claude_cli_save_live_model', { model });
}

export interface HintSettings { enabled: boolean; mic: boolean; hotkey: string }

export async function getHintSettings(): Promise<HintSettings> {
  return invoke<HintSettings>('claude_cli_get_hint_settings');
}

export async function saveHintSettings(settings: HintSettings): Promise<void> {
  await invoke('claude_cli_save_hint_settings', { settings });
}
