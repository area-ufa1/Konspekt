/**
 * Материалы предмета (Konspekt): чистые функции окна материалов и строки в
 * Ассистенте. Без runtime-импортов — файл гоняется под `node --test`.
 */

export const SUPPORTED = ['pdf', 'pptx', 'docx', 'txt', 'md'] as const;

export type CourseKind = (typeof SUPPORTED)[number];

export interface CourseFile {
  id: string;
  clientId: string;
  name: string;
  kind: CourseKind;
  source: 'added' | 'folder';
  folderPath: string | null;
  size: number;
  modified: number;
  hash: string;
  useFlag: boolean;
  state: 'reading' | 'waits-claude' | 'ready' | 'error';
  error: string | null;
  pages: number;
  pagesWaiting: number;
  pagesByClaude: number;
  tokens: number;
  addedAt: string;
}

export interface CourseOverview {
  files: CourseFile[];
  folder: string | null;
  folderUnreachable: boolean;
  summaries: number;
  summaryTokens: number;
  materialTokens: number;
  cap: number;
}

export interface StoredPage {
  number: number;
  text: string;
  byClaude: boolean;
  waitsClaude: boolean;
}

export interface CourseBlock {
  subject: string;
  text: string;
  files: number;
  summaries: number;
  droppedFiles: number;
  droppedSummaries: number;
  tokens: number;
}

export type Translate = (key: string, values?: Record<string, string | number | Date>) => string;

export function isSupported(path: string): boolean {
  const dot = path.lastIndexOf('.');
  if (dot < 0 || dot < path.lastIndexOf('\\') || dot < path.lastIndexOf('/')) return false;
  return (SUPPORTED as readonly string[]).includes(path.slice(dot + 1).toLowerCase());
}

/** 9800 → «~9,8 тыс.», 21000 → «~21 тыс.», 640 → «~640»; без тильды — для точных величин (потолок). */
export function formatTokens(n: number, approx = true): string {
  const mark = approx ? '~' : '';
  if (n < 1000) return `${mark}${Math.round(n)}`;
  const thousands = n / 1000;
  const text = thousands >= 10 ? String(Math.round(thousands)) : thousands.toFixed(1).replace('.0', '').replace('.', ',');
  return `${mark}${text} тыс.`;
}

/** Доли полосы в процентах; при перегрузе нормируются к сумме, чтобы полоса была заполнена целиком. */
export function capState(materials: number, summaries: number, cap: number) {
  const total = materials + summaries;
  const over = cap > 0 ? total > cap : total > 0;
  const base = over ? total : cap;
  if (base <= 0) return { percentMaterials: 0, percentSummaries: 0, over };
  return {
    percentMaterials: (materials * 100) / base,
    percentSummaries: (summaries * 100) / base,
    over,
  };
}

export function statusLine(
  block: Pick<CourseBlock, 'subject' | 'files' | 'summaries' | 'tokens' | 'droppedFiles' | 'droppedSummaries'>,
  t: Translate,
): string {
  const head = t('status', {
    subject: block.subject,
    files: block.files,
    summaries: block.summaries,
    tokens: formatTokens(block.tokens),
  });
  const dropped = block.droppedFiles + block.droppedSummaries;
  return dropped > 0 ? head + t('statusDropped', { count: dropped }) : head;
}
