"use client";

import { useEffect, useState } from 'react';
import { useTranslations } from 'next-intl';
import { invoke } from '@tauri-apps/api/core';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { AssistantTurn, clock, isQuestion } from '@/lib/assistant';
import { hintFromTurn } from '@/lib/hints';
import { HintCard } from '@/components/Hints/HintCard';

export function AssistantTurnsSection({ meetingId }: { meetingId: string }) {
  const t = useTranslations('assistant');
  const th = useTranslations('hints');
  const [turns, setTurns] = useState<AssistantTurn[] | null>(null);

  useEffect(() => {
    let alive = true;
    setTurns(null);
    invoke<AssistantTurn[]>('api_get_assistant_turns', { meetingId })
      .then((loaded) => { if (alive) setTurns(loaded); })
      .catch((e) => {
        console.warn("api_get_assistant_turns failed:", e instanceof Error ? e.message : String(e));
        if (alive) setTurns([]);
      });
    return () => { alive = false; };
  }, [meetingId]);
  if (!turns || turns.length === 0) return null;
  const questions = turns.filter(isQuestion);
  const hints = turns.filter((turn) => !isQuestion(turn)).map(hintFromTurn);
  return (
    <>
      {questions.length > 0 && (
        <section>
          <h3 className="mb-3 text-base font-semibold text-[var(--af-text)]">{t('savedTitle')}</h3>
          <div className="space-y-4">
            {questions.map((turn) => (
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
      )}
      {hints.length > 0 && (
        <section className={questions.length > 0 ? 'mt-6' : undefined}>
          <h3 className="mb-3 text-base font-semibold text-[var(--af-text)]">{th('savedTitle')}</h3>
          <div className="space-y-3">{hints.map((hint) => <HintCard key={hint.id} hint={hint} />)}</div>
        </section>
      )}
    </>
  );
}
