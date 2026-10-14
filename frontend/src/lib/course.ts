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
  modified: string;
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

/** 9800 → «~9,8 тыс.», 21000 → «~21 тыс.», 640 → «~640»; единица и десятичный знак приходят из словаря. */
export function formatTokens(n: number, unit: string, decimal: string, approx = true): string {
  const mark = approx ? '~' : '';
  if (n < 1000) return `${mark}${Math.round(n)}`;
  const thousands = n / 1000;
  const text = thousands >= 10 ? String(Math.round(thousands)) : thousands.toFixed(1).replace('.0', '').replace('.', decimal);
  return `${mark}${text} ${unit}`;
}

/** То же, но единицу и разделитель берёт из словаря (`course.thousands`, `course.decimal`). */
export function tokensText(n: number, t: Translate, approx = true): string {
  return formatTokens(n, t('thousands'), t('decimal'), approx);
}

// Пока открыто окно материалов, документы, брошенные в приложение, — его дело, а не импорта аудио.
let courseDropActive = false;
export function setCourseDropActive(active: boolean): void { courseDropActive = active; }
export function isCourseDropActive(): boolean { return courseDropActive; }

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

export interface FileBadge {
  key: 'stateReady' | 'stateWaits' | 'stateError' | 'stateReading' | 'stateReadingPlain';
  values?: Record<string, string | number>;
  tone: 'ready' | 'waits' | 'error' | 'reading';
}

/**
 * Значок строки файла. `progress` — последнее событие `course-progress` для файла:
 * при извлечении (`reading`) и при чтении картинок Claude (`waits-claude`, пока не всё
 * дочитано) показывает «читается N из M».
 */
export function fileBadge(
  file: Pick<CourseFile, 'state' | 'pagesWaiting' | 'error'>,
  progress?: { done: number; total: number },
): FileBadge {
  const counting = progress && progress.total > 0 ? progress : null;
  switch (file.state) {
    case 'ready':
      return { key: 'stateReady', tone: 'ready' };
    case 'error':
      return { key: 'stateError', values: { message: file.error ?? '' }, tone: 'error' };
    case 'waits-claude':
      return counting && counting.done < counting.total
        ? { key: 'stateReading', values: { done: counting.done, total: counting.total }, tone: 'reading' }
        : { key: 'stateWaits', values: { count: file.pagesWaiting }, tone: 'waits' };
    default:
      return counting
        ? { key: 'stateReading', values: { done: counting.done, total: counting.total }, tone: 'reading' }
        : { key: 'stateReadingPlain', tone: 'reading' };
  }
}

/** Предупреждение у строки, которая не ошибка: устаревший текст после неудачного перечитывания, причина ожидания. */
export function fileWarning(file: Pick<CourseFile, 'state' | 'error'>): string | null {
  return file.state !== 'error' && file.error ? file.error : null;
}

export function statusLine(
  block: Pick<CourseBlock, 'subject' | 'files' | 'summaries' | 'tokens' | 'droppedFiles' | 'droppedSummaries'>,
  t: Translate,
): string {
  const head = t('status', {
    subject: block.subject,
    files: block.files,
    summaries: block.summaries,
    tokens: tokensText(block.tokens, t),
  });
  const dropped = block.droppedFiles + block.droppedSummaries;
  return dropped > 0 ? head + t('statusDropped', { count: dropped }) : head;
}
