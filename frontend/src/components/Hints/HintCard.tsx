"use client";

import { useEffect, useState } from 'react';
import { useTranslations } from 'next-intl';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { X } from 'lucide-react';
import { clock } from '@/lib/assistant';
import type { HintEvent } from '@/lib/hints';

/**
 * Карточка подсказки (вариант A макета): строка «Вопрос · 41:12 · ✕», вопрос
 * курсивом, короткий ответ крупно, «Подробнее ▾» над свёрнутым markdown.
 * `collapsible={false}` показывает подробности сразу, без переключателя.
 */
export function HintCard({ hint, onClose, collapsible = true }: { hint: HintEvent; onClose?: () => void; collapsible?: boolean }) {
  const t = useTranslations('hints');
  const [open, setOpen] = useState(false);
  useEffect(() => { setOpen(false); }, [hint.id]);

  const title = hint.kind === 'hint-hotkey' ? t('lastQuestion') : t('question');
  const body =
    hint.state === 'empty' ? t('noQuestion')
    : hint.state === 'error' ? t('failed', { message: hint.short })
    : hint.short || t('writing');
  const showDetails = !collapsible || open;

  return (
    <div className="grid gap-2 rounded-xl border border-[var(--af-border)] bg-[var(--af-panel)] p-3 text-[var(--af-text)] shadow-lg">
      <div className="flex items-center gap-2 text-xs text-[var(--af-text-3)]">
        <span className="h-2 w-2 rounded-full bg-sky-400" />
        <span>{title}{hint.askedAtSec > 0 ? ` · ${clock(hint.askedAtSec)}` : ''}</span>
        {onClose && (
          <button type="button" onClick={onClose} aria-label={t('close')} title={t('close')}
            className="ml-auto rounded p-0.5 hover:bg-[var(--af-hover)]"><X size={14} /></button>
        )}
      </div>
      {hint.question && <div className="text-sm italic text-[var(--af-text-2)]">«{hint.question}»</div>}
      <div className={`text-[15px] leading-snug ${hint.state === 'done' || hint.state === 'writing' ? 'font-semibold' : 'text-[var(--af-text-2)]'}`}>
        {body}
        {hint.state === 'writing' && hint.short && <span className="ml-0.5 inline-block h-4 w-1.5 animate-pulse bg-[var(--af-text)] align-[-2px]" />}
      </div>
      {hint.details && (
        <>
          {collapsible && (
            <button type="button" onClick={() => setOpen((v) => !v)} aria-expanded={open}
              className="w-fit text-xs text-sky-400 hover:underline">{open ? `${t('less')} ▴` : `${t('more')} ▾`}</button>
          )}
          {showDetails && (
            <div className="prose prose-sm max-h-[360px] max-w-none overflow-auto border-t border-[var(--af-border)] pt-2 dark:prose-invert prose-p:my-1 prose-ul:my-1">
              <ReactMarkdown remarkPlugins={[remarkGfm]}>{hint.details}</ReactMarkdown>
            </div>
          )}
        </>
      )}
    </div>
  );
}
