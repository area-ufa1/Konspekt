"use client";

/** Вкладка «Подсказки» на экране записи: лента подсказок этой записи и счётчик. */

import { useEffect, useState } from 'react';
import { useTranslations } from 'next-intl';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Lightbulb } from 'lucide-react';
import type { AssistantTurn } from '@/lib/assistant';
import { HintCard } from '@/components/Hints/HintCard';
import { HintEvent, applyHintEvent, hintFromTurn } from '@/lib/hints';
import { getHintSettings } from '@/lib/claude-cli';

interface HintsStatus { enabled: boolean; available: boolean; reason: string | null; checked: number; shown: number; hotkeyError: string | null }

const STATUS_POLL_MS = 5000;

export function HintsPanel() {
  const t = useTranslations('hints');
  const [hints, setHints] = useState<HintEvent[]>([]);
  const [status, setStatus] = useState<HintsStatus | null>(null);
  const [hotkey, setHotkey] = useState('Ctrl+Shift+Space');

  useEffect(() => {
    let stopped = false;
    let timer: number | undefined;
    getHintSettings().then((s) => { if (!stopped) setHotkey(s.hotkey); }).catch(() => {});
    invoke<AssistantTurn[]>('hints_live')
      .then((saved) => { if (!stopped) setHints((local) => saved.map(hintFromTurn).reduce(applyHintEvent, local)); })
      .catch(() => {});
    const poll = async () => {
      try {
        const next = await invoke<HintsStatus>('hints_status');
        if (!stopped) setStatus(next);
      } catch {
        // Статус — справка; без него лента всё равно работает.
      }
      if (!stopped) timer = window.setTimeout(poll, STATUS_POLL_MS);
    };
    void poll();
    const unlisten = listen<HintEvent>('hint-update', (event) => setHints((current) => applyHintEvent(current, event.payload)));
    return () => {
      stopped = true;
      if (timer !== undefined) window.clearTimeout(timer);
      void unlisten.then((stop) => stop());
    };
  }, []);

  const notice = status && !status.enabled ? t('off') : status && !status.available ? status.reason : null;

  return (
    <div className="flex h-full min-h-0 flex-col">
      {notice && <div className="mx-3 mt-3 rounded-lg border border-[var(--af-border)] bg-[var(--af-panel-2)] px-3 py-2 text-xs text-[var(--af-text-2)]">{notice}</div>}
      {status?.hotkeyError && <div className="mx-3 mt-2 rounded-lg border border-amber-500/40 px-3 py-2 text-xs text-amber-500">{status.hotkeyError}</div>}
      <div className="min-h-0 flex-1 space-y-3 overflow-y-auto px-3 py-3">
        {hints.length === 0 ? (
          <div className="flex h-full flex-col items-center justify-center gap-2 text-center text-sm text-[var(--af-text-3)]">
            <Lightbulb size={20} />
            <p className="max-w-xs">{t('empty', { hotkey })}</p>
          </div>
        ) : (
          [...hints].reverse().map((hint) => <HintCard key={hint.id} hint={hint} />)
        )}
      </div>
      {status?.enabled && (
        <div className="px-3 pb-2 text-xs text-[var(--af-text-3)]">{t('counter', { checked: status.checked, shown: status.shown })}</div>
      )}
    </div>
  );
}
