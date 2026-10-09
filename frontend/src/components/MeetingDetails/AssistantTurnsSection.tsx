"use client";

import { useEffect, useState } from 'react';
import { useTranslations } from 'next-intl';
import { invoke } from '@tauri-apps/api/core';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { AssistantTurn, clock } from '@/lib/assistant';

export function AssistantTurnsSection({ meetingId }: { meetingId: string }) {
  const t = useTranslations('assistant');
  const [turns, setTurns] = useState<AssistantTurn[] | null>(null);

  useEffect(() => {
    let alive = true;
    invoke<AssistantTurn[]>('api_get_assistant_turns', { meetingId })
      .then((loaded) => { if (alive) setTurns(loaded); })
      .catch(() => { if (alive) setTurns([]); });
    return () => { alive = false; };
  }, [meetingId]);

  if (!turns || turns.length === 0) return null;
  return (
    <section>
      <h3 className="mb-3 text-base font-semibold text-[var(--af-text)]">{t('savedTitle')}</h3>
      <div className="space-y-4">
        {turns.map((turn) => (
          <div key={turn.id} className="space-y-1">
            <div className="text-sm font-medium text-[var(--af-text)]">
              <span className="mr-2 text-xs text-[var(--af-text-3)]">[{clock(turn.askedAtSec)}]</span>
              {turn.question}
            </div>
            <div className="prose prose-sm max-w-none dark:prose-invert prose-p:my-1 prose-ul:my-1">
              <ReactMarkdown remarkPlugins={[remarkGfm]}>{turn.answer}</ReactMarkdown>
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}
