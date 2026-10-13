// Логика подсказок без React — её проверяет node --test.

import type { AssistantTurn, TurnKind } from './assistant.ts';

export type HintState = 'writing' | 'done' | 'empty' | 'error';

export interface HintEvent {
  id: string;
  kind: TurnKind;
  askedAtSec: number;
  question: string;
  short: string;
  details: string;
  state: HintState;
}

export function parseHintAnswer(answer: string): { short: string; details: string } {
  try {
    const parsed = JSON.parse(answer);
    if (parsed && typeof parsed.short === 'string') {
      return { short: parsed.short, details: typeof parsed.details === 'string' ? parsed.details : '' };
    }
  } catch {
    // Не JSON — показываем как есть.
  }
  return { short: answer, details: '' };
}

export function hintFromTurn(turn: AssistantTurn): HintEvent {
  const { short, details } = parseHintAnswer(turn.answer);
  return { id: turn.id, kind: turn.kind ?? 'hint', askedAtSec: turn.askedAtSec, question: turn.question, short, details, state: 'done' };
}

export function applyHintEvent(list: HintEvent[], event: HintEvent): HintEvent[] {
  const rest = list.filter((hint) => hint.id !== event.id);
  // «Пустой» итог не хранится и убирает своё «пишет», если оно было.
  if (event.state === 'empty') return rest.length === list.length ? list : rest;
  return [...rest, event].sort((a, b) => a.askedAtSec - b.askedAtSec);
}
