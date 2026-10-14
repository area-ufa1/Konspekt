'use client';

import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import { useTranslations } from 'next-intl';
import { toast } from 'sonner';
import { MoreVertical } from 'lucide-react';
import { Dialog, DialogContent, DialogDescription, DialogTitle } from '@/components/ui/dialog';
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from '@/components/ui/dropdown-menu';
import { Switch } from '@/components/ui/switch';
import { isAudioPath } from '@/constants/audioFormats';
import { SUPPORTED, capState, isSupported, setCourseDropActive, tokensText } from '@/lib/course';
import type { CourseFile, CourseOverview } from '@/lib/course';
import { CourseTextViewer } from './CourseTextViewer';

const errorText = (error: unknown) => (typeof error === 'string' ? error : (error as Error)?.message ?? String(error));

interface Props {
  clientId: string;
  subject: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function CourseMaterialsDialog({ clientId, subject, open, onOpenChange }: Props) {
  const t = useTranslations('course');
  const [overview, setOverview] = useState<CourseOverview | null>(null);
  const [progress, setProgress] = useState<Record<string, { done: number; total: number }>>({});
  const [viewer, setViewer] = useState<{ id: string; name: string } | null>(null);
  const tRef = useRef(t);
  tRef.current = t;

  const load = useCallback(async () => {
    try {
      const next = await invoke<CourseOverview>('course_list', { clientId });
      setOverview(next);
      // Файл больше не читается — его прогресс не нужен.
      setProgress((current) => {
        const reading = new Set(next.files.filter((f) => f.state === 'reading').map((f) => f.id));
        const kept = Object.entries(current).filter(([id]) => reading.has(id));
        return kept.length === Object.keys(current).length ? current : Object.fromEntries(kept);
      });
    } catch (error) {
      toast.error(tRef.current('loadFailed', { message: errorText(error) }));
    }
  }, [clientId]);

  const addPaths = useCallback(async (paths: string[]) => {
    // Аудио — дело импорта записей, не материалов.
    const rest = paths.filter((p) => !isAudioPath(p));
    const good = rest.filter(isSupported);
    const bad = rest.filter((p) => !isSupported(p));
    if (bad.length > 0) {
      const names = bad.map((p) => p.split(/[\\/]/).pop() ?? p).join(', ');
      toast.error(tRef.current('unsupported', { names }));
    }
    if (good.length === 0) return;
    try {
      await invoke('course_add_files', { clientId, paths: good });
    } catch (error) {
      toast.error(errorText(error));
    }
  }, [clientId]);

  // Перетаскивание слушаем, только пока окно открыто.
  useEffect(() => {
    if (!open) return;
    setCourseDropActive(true);
    setOverview(null);
    setProgress({});
    void load();
    const unlisteners: Array<Promise<() => void>> = [
      listen<{ clientId: string }>('course-changed', (event) => {
        if (event.payload.clientId === clientId) void load();
      }),
      listen<{ clientId: string; fileId: string; done: number; total: number }>('course-progress', (event) => {
        const p = event.payload;
        if (p.clientId === clientId) setProgress((current) => ({ ...current, [p.fileId]: { done: p.done, total: p.total } }));
      }),
      listen<{ paths: string[] }>('tauri://drag-drop', (event) => {
        void addPaths(event.payload.paths ?? []);
      }),
    ];
    return () => {
      setCourseDropActive(false);
      unlisteners.forEach((p) => void p.then((stop) => stop()));
    };
  }, [open, clientId, load, addPaths]);

  const run = async (command: string, args: Record<string, unknown>) => {
    try {
      await invoke(command, args);
    } catch (error) {
      toast.error(errorText(error));
    }
  };

  const pickFolder = async () => {
    try {
      const picked = await openDialog({ directory: true, multiple: false });
      if (typeof picked === 'string') await run('course_set_folder', { clientId, path: picked });
    } catch (error) {
      toast.error(errorText(error));
    }
  };

  const pickFiles = async () => {
    try {
      const picked = await openDialog({ multiple: true, filters: [{ name: tRef.current('filterName'), extensions: [...SUPPORTED] }] });
      if (Array.isArray(picked)) await addPaths(picked);
      else if (typeof picked === 'string') await addPaths([picked]);
    } catch (error) {
      toast.error(errorText(error));
    }
  };

  const used = overview ? overview.materialTokens + overview.summaryTokens : 0;
  const bar = overview ? capState(overview.materialTokens, overview.summaryTokens, overview.cap) : null;

  return (
    <>
      <Dialog open={open} onOpenChange={onOpenChange}>
        <DialogContent className="flex max-h-[85vh] max-w-2xl flex-col gap-0 p-0">
          <div className="space-y-1.5 border-b border-[var(--af-border)] py-4 pl-5 pr-12">
            <DialogTitle className="text-base">{t('title', { subject })}</DialogTitle>
            <DialogDescription className="sr-only">{t('title', { subject })}</DialogDescription>
            {overview && (
              <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[13px] text-[var(--af-text-2)]">
                <span>{t('folder')}</span>
                {overview.folder ? (
                  <code className="rounded bg-[var(--af-panel-2)] px-1.5 py-px text-xs text-[var(--af-text)]">{overview.folder}</code>
                ) : (
                  <span className="text-[var(--af-text-3)]">{t('folderNone')}</span>
                )}
                <button type="button" onClick={() => void pickFolder()} className="text-[var(--af-accent)] hover:underline">
                  {overview.folder ? t('folderChange') : t('folderPick')}
                </button>
                {overview.folder && (
                  <>
                    <button type="button" onClick={() => void run('course_set_folder', { clientId, path: null })} className="text-[var(--af-accent)] hover:underline">{t('folderUnlink')}</button>
                    <button type="button" onClick={() => void run('course_refresh', { clientId })} className="text-[var(--af-accent)] hover:underline">{t('refresh')}</button>
                  </>
                )}
              </div>
            )}
            {overview?.folderUnreachable && <p className="text-xs text-amber-500">{t('folderUnreachable')}</p>}
          </div>

          <div className="min-h-0 flex-1 overflow-y-auto">
            {overview && overview.files.length === 0 && (
              <p className="px-5 py-4 text-sm text-[var(--af-text-3)]">{t('empty')}</p>
            )}
            {overview?.files.map((file) => (
              <FileRow key={file.id} file={file} progress={progress[file.id]}
                onUse={(flag) => void run('course_set_use', { fileId: file.id, useFlag: flag })}
                onView={() => setViewer({ id: file.id, name: file.name })}
                onReread={() => void run('course_reread', { fileId: file.id })}
                onRemove={() => void run('course_remove', { fileId: file.id })} />
            ))}
          </div>

          {overview && bar && (
            <div className="border-t border-[var(--af-border)]">
              <div className="px-5 py-2.5 text-[13px] text-[var(--af-text-2)]">
                {t('summaries', { count: overview.summaries, tokens: tokensText(overview.summaryTokens, t) })}
              </div>
              <div className="space-y-2.5 px-5 pb-4 pt-1">
                <div className="flex h-2 overflow-hidden rounded-full bg-[var(--af-panel-2)]">
                  <i className={`block h-full ${bar.over ? 'bg-red-500' : 'bg-[var(--af-accent)]'}`} style={{ width: `${bar.percentMaterials}%` }} />
                  <i className={`block h-full ${bar.over ? 'bg-red-500' : 'bg-violet-400'}`} style={{ width: `${bar.percentSummaries}%` }} />
                </div>
                <div className="flex flex-wrap justify-between gap-x-3 gap-y-1 text-[13px] tabular-nums text-[var(--af-text-2)]">
                  {bar.over ? (
                    <span className="text-red-500">{t('over', { used: tokensText(used, t), cap: tokensText(overview.cap, t, false) })}</span>
                  ) : (
                    <span>{t('total', { used: tokensText(used, t), cap: tokensText(overview.cap, t, false) })}</span>
                  )}
                  <span>{t('split', { materials: tokensText(overview.materialTokens, t), summaries: tokensText(overview.summaryTokens, t) })}</span>
                </div>
                <div className="flex flex-wrap items-center gap-3">
                  <button type="button" onClick={() => void pickFiles()}
                    className="rounded-lg bg-[var(--af-accent)] px-3 py-1.5 text-[13px] text-white hover:opacity-90">
                    {t('add')}
                  </button>
                  <span className="text-xs text-[var(--af-text-3)]">{t('dropHint')}</span>
                </div>
              </div>
            </div>
          )}
        </DialogContent>
      </Dialog>
      <CourseTextViewer fileId={viewer?.id ?? null} name={viewer?.name ?? ''} onClose={() => setViewer(null)} />
    </>
  );
}

function FileRow({ file, progress, onUse, onView, onReread, onRemove }: {
  file: CourseFile;
  progress?: { done: number; total: number };
  onUse: (flag: boolean) => void;
  onView: () => void;
  onReread: () => void;
  onRemove: () => void;
}) {
  const t = useTranslations('course');
  const unit = file.kind === 'pptx' ? t('slides', { count: file.pages }) : t('pages', { count: file.pages });

  let badge: { text: string; cls: string };
  if (file.state === 'ready') badge = { text: t('stateReady'), cls: 'text-emerald-500' };
  else if (file.state === 'waits-claude') badge = { text: t('stateWaits', { count: file.pagesWaiting }), cls: 'text-amber-500' };
  else if (file.state === 'error') badge = { text: t('stateError', { message: file.error ?? '' }), cls: 'text-red-500' };
  else badge = { text: progress && progress.total > 0 ? t('stateReading', { done: progress.done, total: progress.total }) : t('stateReadingPlain'), cls: 'text-[var(--af-accent)]' };

  return (
    <div className="grid grid-cols-[auto_minmax(0,1fr)_auto_auto] items-center gap-3 border-b border-[var(--af-border)] px-5 py-2.5">
      <Switch checked={file.useFlag} onCheckedChange={onUse} aria-label={t('useAria', { name: file.name })} />
      <div className="min-w-0">
        <div className={`break-words text-sm ${file.useFlag ? 'text-[var(--af-text)]' : 'text-[var(--af-text-3)] line-through'}`}>{file.name}</div>
        <div className="mt-0.5 flex flex-wrap gap-x-2.5 gap-y-0.5 text-xs text-[var(--af-text-3)]">
          <span>{file.kind.toUpperCase()}{file.pages > 0 && ` · ${unit}`}</span>
          {file.tokens > 0 && <span>{t('tokensOf', { value: tokensText(file.tokens, t) })}</span>}
          <span className="rounded border border-[var(--af-border)] px-1.5 text-[11px]">{file.source === 'folder' ? t('sourceFolder') : t('sourceAdded')}</span>
          {file.pagesByClaude > 0 && <span>{t('byClaude', { count: file.pagesByClaude })}</span>}
          {!file.useFlag && <span>{t('notUsed')}</span>}
        </div>
      </div>
      <span className={`whitespace-nowrap rounded-full border border-[var(--af-border)] px-2 py-0.5 text-xs ${badge.cls}`}>{badge.text}</span>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <button type="button" aria-label={t('actionsAria', { name: file.name })} className="rounded-md p-1 text-[var(--af-text-3)] hover:bg-[var(--af-hover)]">
            <MoreVertical className="h-4 w-4" />
          </button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuItem onSelect={onView}>{t('view')}</DropdownMenuItem>
          <DropdownMenuItem onSelect={onReread}>{t('reread')}</DropdownMenuItem>
          <DropdownMenuItem onSelect={onRemove} className="text-red-500">{t('remove')}</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}
