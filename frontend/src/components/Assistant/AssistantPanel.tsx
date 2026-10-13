"use client";

/**
 * Вопросы Claude во время записи (Konspekt). Ответ печатается по мере
 * генерации: бэкенд шлёт событие assistant-answer с текстом на данный момент.
 */

import { useEffect, useRef, useState } from 'react';
import { useTranslations } from 'next-intl';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { MessageSquare, Send } from 'lucide-react';
import {
  AssistantTurn, PanelTurn, QUICK_QUESTION_KEYS, applyAnswerEvent, clock, isQuestion, mergeLiveTurns,
} from '@/lib/assistant';

interface AssistantStatus { available: boolean; reason: string | null; model: string | null }

const STATUS_POLL_MS = 5000;

export function AssistantPanel() {
  const t = useTranslations('assistant');
  const [turns, setTurns] = useState<PanelTurn[]>([]);
  const [question, setQuestion] = useState('');
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState<AssistantStatus | null>(null);
  const bottom = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    let stopped = false;
    let timer: number | undefined;
    invoke<AssistantTurn[]>('assistant_live_turns')
      .then((saved) => { if (!stopped) setTurns((local) => mergeLiveTurns(local, saved.filter(isQuestion))); })
      .catch(() => {});
    // Следующий опрос — только после ответа на предыдущий, без наложения вызовов.
    const poll = async () => {
      try {
        const next = await invoke<AssistantStatus>('assistant_status');
        if (!stopped) setStatus(next);
      } catch {
        // Статус — подсказка; без него поле просто остаётся доступным.
      }
      if (!stopped) timer = window.setTimeout(poll, STATUS_POLL_MS);
    };
    void poll();
    const unlisten = listen<{ id: string; text: string; done: boolean }>('assistant-answer', (event) =>
      setTurns((current) => applyAnswerEvent(current, event.payload)));
    return () => {
      stopped = true;
      if (timer !== undefined) window.clearTimeout(timer);
      void unlisten.then((stop) => stop());
    };
  }, []);

  useEffect(() => { bottom.current?.scrollIntoView({ block: 'end' }); }, [turns]);

  const ask = async (text: string) => {
    const q = text.trim();
    if (!q || busy) return;
    const id = crypto.randomUUID();
    setQuestion('');
    setTurns((current) => [...current, { id, askedAtSec: 0, question: q, answer: '', model: '', createdAt: '', status: 'pending' }]);
    setBusy(true);
    try {
      const turn = await invoke<AssistantTurn>('assistant_ask', { id, question: q });
      setTurns((current) => current.map((x) => (x.id === id ? { ...turn, status: 'done' } : x)));
    } catch (error) {
      const message = typeof error === 'string' ? error : (error as Error)?.message ?? '';
      setTurns((current) => current.map((x) => (x.id === id ? { ...x, answer: t('failed', { message }), status: 'error' } : x)));
    } finally {
      setBusy(false);
    }
  };

  const unavailable = status && !status.available ? status.reason || t('unavailable') : null;

  return (
    <div className="flex h-full min-h-0 flex-col">
      {unavailable && (
        <div className="mx-3 mt-3 rounded-lg border border-[var(--af-border)] bg-[var(--af-panel-2)] px-3 py-2 text-xs text-[var(--af-text-2)]">{unavailable}</div>
      )}
      <div className="min-h-0 flex-1 space-y-3 overflow-y-auto px-4 py-4">
        {turns.length === 0 ? (
          <div className="flex h-full flex-col items-center justify-center gap-2 text-center text-sm text-[var(--af-text-3)]">
            <MessageSquare size={20} />
            <p className="max-w-xs">{t('empty')}</p>
          </div>
        ) : (
          turns.map((turn) => (
            <div key={turn.id} className="space-y-1">
              <div className="ml-auto w-fit max-w-[90%] rounded-lg bg-[var(--af-panel-2)] px-3 py-1.5 text-sm text-[var(--af-text)]">
                {turn.question}
                {turn.askedAtSec > 0 && <span className="ml-2 text-xs text-[var(--af-text-3)]">{t('askedAt', { time: clock(turn.askedAtSec) })}</span>}
              </div>
              <div className="px-1 py-1 text-sm text-[var(--af-text)]">
                {turn.status === 'pending' && !turn.answer ? (
                  <span className="inline-flex items-center gap-1 text-[var(--af-text-3)]">
                    <span className="h-2 w-2 animate-pulse rounded-full bg-[var(--af-text-3)]" /> {t('thinking')}
                  </span>
                ) : (
                  <div className="prose prose-sm max-w-none dark:prose-invert prose-p:my-1 prose-ul:my-1">
                    <ReactMarkdown remarkPlugins={[remarkGfm]}>{turn.answer}</ReactMarkdown>
                  </div>
                )}
              </div>
            </div>
          ))
        )}
        <div ref={bottom} />
      </div>
      <div className="flex flex-wrap gap-1.5 px-3 pb-2">
        {QUICK_QUESTION_KEYS.map((key) => (
          <button key={key} type="button" disabled={busy || !!unavailable} onClick={() => void ask(t(key))}
            className="rounded-full border border-[var(--af-border)] px-2.5 py-1 text-xs text-[var(--af-text-2)] hover:bg-[var(--af-hover)] disabled:opacity-40">
            {t(key)}
          </button>
        ))}
      </div>
      <div className="px-3 pb-3">
        <div className="flex items-center gap-2 rounded-xl border border-[var(--af-border)] bg-[var(--af-panel)] py-1.5 pl-3 pr-1.5 focus-within:border-[var(--af-border-strong)]">
          <textarea value={question} onChange={(e) => setQuestion(e.target.value)} rows={1}
            onKeyDown={(e) => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); void ask(question); } }}
            placeholder={t('placeholder')} disabled={!!unavailable}
            className="af-bare flex-1 resize-none border-0 bg-transparent py-1 text-sm text-[var(--af-text)] placeholder:text-[var(--af-text-3)] focus:outline-none focus:ring-0" />
          <button type="button" onClick={() => void ask(question)} disabled={busy || !question.trim() || !!unavailable}
            title={t('send')} aria-label={t('send')}
            className="flex h-8 w-8 items-center justify-center rounded-lg text-[var(--af-text-2)] hover:bg-[var(--af-hover)] disabled:opacity-40">
            <Send size={16} />
          </button>
        </div>
      </div>
    </div>
  );
}
