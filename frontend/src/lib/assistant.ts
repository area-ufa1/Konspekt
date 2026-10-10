// Логика вкладки «Ассистент» без React — её проверяет node --test.

export interface AssistantTurn {
  id: string;
  askedAtSec: number;
  question: string;
  answer: string;
  model: string;
  createdAt: string;
}

export type PanelTurn = AssistantTurn & { status: 'pending' | 'done' | 'error' };

export const QUICK_QUESTION_KEYS = ['quickMissed', 'quickTerm', 'quickNow'] as const;

export function applyAnswerEvent(
  turns: PanelTurn[],
  event: { id: string; text: string; done: boolean },
): PanelTurn[] {
  if (!turns.some((turn) => turn.id === event.id)) return turns;
  return turns.map((turn) =>
    turn.id === event.id
      ? { ...turn, answer: event.text, status: event.done ? 'done' : turn.status }
      : turn,
  );
}

export function mergeLiveTurns(local: PanelTurn[], saved: AssistantTurn[]): PanelTurn[] {
  const byId = new Map<string, PanelTurn>(local.map((turn) => [turn.id, turn]));
  for (const turn of saved) byId.set(turn.id, { ...turn, status: 'done' });
  return [...byId.values()].sort((a, b) => a.askedAtSec - b.askedAtSec);
}

export function clock(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = String(total % 60).padStart(2, '0');
  return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${s}` : `${m}:${s}`;
}
