'use client';

import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslations } from 'next-intl';
import { toast } from 'sonner';
import { Dialog, DialogContent, DialogDescription, DialogTitle } from '@/components/ui/dialog';
import type { StoredPage } from '@/lib/course';

const errorText = (error: unknown) => (typeof error === 'string' ? error : (error as Error)?.message ?? String(error));

interface Props {
  fileId: string | null;
  name: string;
  onClose: () => void;
}

/** Текст, который Claude получит из файла, постранично. */
export function CourseTextViewer({ fileId, name, onClose }: Props) {
  const t = useTranslations('course');
  const [pages, setPages] = useState<StoredPage[] | null>(null);

  const load = useCallback(async () => {
    if (!fileId) return;
    try {
      setPages(await invoke<StoredPage[]>('course_pages', { fileId }));
    } catch (error) {
      toast.error(errorText(error));
    }
  }, [fileId]);

  useEffect(() => {
    setPages(null);
    void load();
  }, [load]);

  const reread = async () => {
    if (!fileId) return;
    try {
      await invoke('course_reread', { fileId });
      void load();
    } catch (error) {
      toast.error(errorText(error));
    }
  };

  return (
    <Dialog open={fileId !== null} onOpenChange={(open) => { if (!open) onClose(); }}>
      <DialogContent className="flex max-h-[80vh] max-w-2xl flex-col gap-3">
        <div className="flex items-center gap-3 pr-6">
          <DialogTitle className="min-w-0 flex-1 truncate text-base">{name}</DialogTitle>
          <button type="button" onClick={() => void reread()} className="shrink-0 text-sm text-[var(--af-accent)] hover:underline">
            {t('reread')}
          </button>
        </div>
        <DialogDescription className="sr-only">{name}</DialogDescription>
        <div className="min-h-0 flex-1 space-y-3 overflow-y-auto">
          {pages !== null && pages.length === 0 && <p className="text-sm text-[var(--af-text-3)]">{t('noPages')}</p>}
          {pages?.map((page) => (
            <div key={page.number} className="space-y-1">
              <div className="flex items-center gap-2 text-xs font-medium text-[var(--af-text-3)]">
                {t('page', { number: page.number })}
                {page.byClaude && <span className="rounded border border-violet-400/50 px-1.5 text-[11px] text-violet-400">{t('readByClaude')}</span>}
                {page.waitsClaude && <span className="rounded border border-amber-400/50 px-1.5 text-[11px] text-amber-400">{t('waiting')}</span>}
              </div>
              <p className="whitespace-pre-wrap text-sm text-[var(--af-text)]">{page.text}</p>
            </div>
          ))}
        </div>
      </DialogContent>
    </Dialog>
  );
}
