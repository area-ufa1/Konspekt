'use client';

import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslations } from 'next-intl';
import { AlertTriangle, CheckCircle2, ExternalLink, Loader2, XCircle } from 'lucide-react';

import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Switch } from '@/components/ui/switch';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import {
  CLAUDE_CODE_INSTALL_URL,
  ClaudeCliStatus,
  claudeCliBlockingReason,
  getClaudeCliPath,
  getClaudeCliStatus,
  getClaudeLiveModel,
  getHintSettings,
  HintSettings,
  saveClaudeCliPath,
  saveClaudeLiveModel,
  saveHintSettings,
  supportsHaiku55,
  testClaudeCliConnection,
} from '@/lib/claude-cli';

interface ClaudeCliSettingsProps {
  /** Даёт модалке запретить «Готово», пока CLI не готов. */
  onStatusChange?: (status: ClaudeCliStatus | null) => void;
}

const LIVE_MODELS = ['haiku', 'sonnet', 'opus'] as const;

/**
 * Панель статуса провайдера Claude Code. Ключа здесь нет: CLI уже вошёл в
 * аккаунт или не вошёл — панель говорит, что из этого правда, и что чинить.
 * Путь и модель живой панели хранятся здесь же; сохранение — только по действию
 * пользователя (проверка, уход фокуса из изменённого поля, смена модели).
 */
export function ClaudeCliSettings({ onStatusChange }: ClaudeCliSettingsProps) {
  const t = useTranslations('claudeCli');
  const [path, setPath] = useState('');
  const [savedPath, setSavedPath] = useState('');
  const [liveModel, setLiveModel] = useState<string>('haiku');
  const [hints, setHints] = useState<HintSettings | null>(null);
  const [hotkeyDraft, setHotkeyDraft] = useState('');
  const [hotkeyError, setHotkeyError] = useState<string | null>(null);
  const [localOnly, setLocalOnly] = useState(false);
  const [status, setStatus] = useState<ClaudeCliStatus | null>(null);
  const [isChecking, setIsChecking] = useState(false);
  const [isTesting, setIsTesting] = useState(false);
  const [testResult, setTestResult] = useState<{ ok: boolean; message: string } | null>(null);

  const refresh = useCallback(
    async (pathOverride?: string) => {
      setIsChecking(true);
      setTestResult(null);
      try {
        const trimmed = (pathOverride ?? path).trim();
        const next = await getClaudeCliStatus(trimmed ? trimmed : null);
        setStatus(next);
        onStatusChange?.(next);
      } catch (error) {
        const failed: ClaudeCliStatus = {
          installed: false,
          api_key_env_detected: false,
          error: error instanceof Error ? error.message : String(error),
        };
        setStatus(failed);
        onStatusChange?.(failed);
      } finally {
        setIsChecking(false);
      }
    },
    [path, onStatusChange]
  );

  useEffect(() => {
    getHintSettings()
      .then((loaded) => { setHints(loaded); setHotkeyDraft(loaded.hotkey); })
      .catch(() => setHints({ enabled: false, mic: false, hotkey: 'Ctrl+Shift+Space' }));
    invoke<boolean>('get_local_only_mode')
      .then(setLocalOnly)
      .catch(() => setLocalOnly(false));
  }, []);

  const persistHints = async (next: HintSettings) => {
    setHints(next);
    try {
      await saveHintSettings(next);
      setHotkeyError(null);
    } catch (error) {
      setHotkeyError(typeof error === 'string' ? error : (error as Error)?.message ?? '');
    }
  };

  // Один зонд при монтировании. Только чтение: ничего не сохраняем.
  useEffect(() => {
    let alive = true;
    Promise.all([getClaudeCliPath().catch(() => null), getClaudeLiveModel().catch(() => 'haiku')]).then(
      ([storedPath, storedModel]) => {
        if (!alive) return;
        setPath(storedPath ?? '');
        setSavedPath(storedPath ?? '');
        setLiveModel(storedModel);
        void refresh(storedPath ?? '');
      }
    );
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Пишем только изменённый путь: пассивный рендер и пустой blur ничего не сохраняют.
  const persistPath = async () => {
    const trimmed = path.trim();
    if (trimmed === savedPath) return;
    try {
      await saveClaudeCliPath(trimmed ? trimmed : null);
      setSavedPath(trimmed);
    } catch (error) {
      console.error('Failed to save the Claude Code path:', error);
    }
  };

  const handleCheck = async () => {
    await persistPath();
    await refresh(path);
  };

  const runTest = async () => {
    setIsTesting(true);
    setTestResult(null);
    try {
      const trimmed = path.trim();
      const result = await testClaudeCliConnection(trimmed ? trimmed : null, liveModel);
      setTestResult(
        result.status === 'success'
          ? { ok: true, message: t('testOk') }
          : { ok: false, message: t('testFailed', { message: result.message }) }
      );
    } catch (error) {
      setTestResult({
        ok: false,
        message: t('testFailed', { message: error instanceof Error ? error.message : String(error) }),
      });
    } finally {
      setIsTesting(false);
    }
  };

  const handleLiveModelChange = (value: string) => {
    setLiveModel(value);
    void saveClaudeLiveModel(value).catch((error) =>
      console.error('Failed to save the live-panel model:', error)
    );
  };

  const blockingReason = claudeCliBlockingReason(status);
  const auth = status?.auth;

  return (
    <div className="space-y-4 border-t pt-4">
      <div>
        <h4 className="text-sm font-semibold">{t('title')}</h4>
        <p className="text-xs text-muted-foreground mt-1">{t('description')}</p>
      </div>

      <div className="rounded-xl border bg-muted/40 p-3 text-sm">
        {status === null ? (
          <div className="flex items-center text-muted-foreground">
            <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            {t('checking')}
          </div>
        ) : blockingReason === null ? (
          <div className="space-y-1">
            <div className="flex items-center font-medium text-green-600">
              <CheckCircle2 className="mr-2 h-4 w-4 shrink-0" />
              {t('statusReady', {
                version: status.version ?? '',
                plan: auth?.subscriptionType ?? '—',
              })}
            </div>
            {status.path && <p className="break-all text-xs text-muted-foreground">{status.path}</p>}
            {(auth?.email || auth?.orgName) && (
              <p className="text-xs text-muted-foreground">
                {[auth?.email, auth?.orgName].filter(Boolean).join(' · ')}
              </p>
            )}
          </div>
        ) : blockingReason === 'not-logged-in' ? (
          <p className="flex items-start text-xs text-amber-600">
            <AlertTriangle className="mr-1.5 mt-0.5 h-3.5 w-3.5 shrink-0" />
            {t('notLoggedIn')}
          </p>
        ) : (
          <div className="space-y-2">
            <div className="flex items-start font-medium text-red-600">
              <XCircle className="mr-2 mt-0.5 h-4 w-4 shrink-0" />
              <span>{t('notInstalled')}</span>
            </div>
            {status.error && <p className="break-all text-xs text-muted-foreground">{status.error}</p>}
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => invoke('open_external_url', { url: CLAUDE_CODE_INSTALL_URL }).catch(() => {})}
            >
              <ExternalLink className="mr-2 h-3.5 w-3.5" />
              {t('install')}
            </Button>
          </div>
        )}
      </div>

      {status?.api_key_env_detected && (
        <p className="flex items-start rounded-lg bg-amber-500/10 p-2 text-xs">
          <AlertTriangle className="mr-1.5 mt-0.5 h-3.5 w-3.5 shrink-0 text-amber-600" />
          <span>{t('apiKeyEnvWarning')}</span>
        </p>
      )}

      <div>
        <Label htmlFor="claude-cli-path">{t('pathLabel')}</Label>
        <Input
          id="claude-cli-path"
          value={path}
          onChange={(e) => setPath(e.target.value)}
          onBlur={() => void persistPath()}
          placeholder={t('pathPlaceholder')}
          className="mt-1"
        />
      </div>

      <div>
        <Label>{t('liveModelLabel')}</Label>
        <Select value={liveModel} onValueChange={handleLiveModelChange}>
          <SelectTrigger className="mt-1">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {LIVE_MODELS.map((m) => (
              <SelectItem key={m} value={m}>
                {m}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <p className="text-xs text-muted-foreground mt-1">{t('liveModelHint')}</p>
        {liveModel === 'haiku' && status !== null && !supportsHaiku55(status.version) && (
          <p className="text-xs text-amber-600 mt-1">{t('haikuNeedsUpdate')}</p>
        )}
      </div>

      {hints && (
        <div className="space-y-3">
          <div className="flex items-start justify-between gap-3">
            <div>
              <Label htmlFor="claude-hints">{t('hintsLabel')}</Label>
              <p className="text-xs text-muted-foreground mt-1">{t('hintsHint')}</p>
              {localOnly && <p className="text-xs text-muted-foreground mt-1">{t('hintsLocalOnly')}</p>}
            </div>
            <Switch id="claude-hints" checked={hints.enabled} disabled={localOnly}
              onCheckedChange={(enabled) => void persistHints({ ...hints, enabled })} />
          </div>
          <div className="flex items-start justify-between gap-3">
            <div>
              <Label htmlFor="claude-hints-mic">{t('hintsMicLabel')}</Label>
              <p className="text-xs text-muted-foreground mt-1">{t('hintsMicHint')}</p>
            </div>
            <Switch id="claude-hints-mic" checked={hints.mic} disabled={localOnly || !hints.enabled}
              onCheckedChange={(mic) => void persistHints({ ...hints, mic })} />
          </div>
          <div>
            <Label htmlFor="claude-hints-hotkey">{t('hintsHotkeyLabel')}</Label>
            <Input id="claude-hints-hotkey" value={hotkeyDraft} disabled={localOnly || !hints.enabled} className="mt-1"
              onChange={(e) => setHotkeyDraft(e.target.value)}
              onBlur={() => void persistHints({ ...hints, hotkey: hotkeyDraft.trim() || 'Ctrl+Shift+Space' })} />
            <p className="text-xs text-muted-foreground mt-1">{t('hintsHotkeyHint')}</p>
            {hotkeyError && <p className="text-xs text-red-600 mt-1">{hotkeyError}</p>}
          </div>
        </div>
      )}

      <div className="flex gap-2">
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => void handleCheck()}
          disabled={isChecking}
          className="flex-1"
        >
          {isChecking ? (
            <>
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              {t('checking')}
            </>
          ) : (
            t('check')
          )}
        </Button>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={runTest}
          disabled={isTesting || blockingReason !== null}
          className="flex-1"
        >
          {isTesting ? <Loader2 className="mr-2 h-4 w-4 animate-spin" /> : null}
          {t('testButton')}
        </Button>
      </div>

      {testResult && (
        <p className={`text-xs ${testResult.ok ? 'text-green-600' : 'text-red-600'}`}>{testResult.message}</p>
      )}
    </div>
  );
}
