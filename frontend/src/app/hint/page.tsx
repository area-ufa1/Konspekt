'use client';

/**
 * Окно подсказки (метка `hint`): карточка поверх всех окон, без рамки.
 * Rust создаёт окно при первой подсказке и показывает его без активации;
 * здесь — последняя подсказка, Esc/✕ прячут окно, высота подгоняется под карточку.
 * Сама страница окно никогда не показывает — только прячет.
 */

import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { HintCard } from '@/components/Hints/HintCard';
import { applyAppTheme, getSavedAppTheme } from '@/lib/app-theme';
import type { HintEvent } from '@/lib/hints';

export default function HintWindow() {
  const [hint, setHint] = useState<HintEvent | null>(null);
  const box = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    applyAppTheme(getSavedAppTheme());
    const receive = (event: HintEvent) => {
      // Кандидат, который в итоге оказался «НЕТ»: показывать нечего, окно прячем.
      // «Вопросов не было» — только ответ на горячую клавишу.
      if (event.state === 'empty' && event.kind !== 'hint-hotkey') {
        void invoke('hint_window_hide');
        return;
      }
      setHint(event);
    };
    // Событие могло прийти раньше, чем окно загрузилось.
    invoke<HintEvent | null>('hints_current').then((current) => { if (current) receive(current); }).catch(() => {});
    const unlisten = listen<HintEvent>('hint-update', (event) => receive(event.payload));
    const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') void invoke('hint_window_hide'); };
    window.addEventListener('keydown', onKey);
    return () => { void unlisten.then((stop) => stop()); window.removeEventListener('keydown', onKey); };
  }, []);

  useEffect(() => {
    if (!box.current) return;
    const observer = new ResizeObserver(() => {
      const height = Math.ceil(box.current?.getBoundingClientRect().height ?? 0) + 8;
      void invoke('hint_window_fit', { height });
    });
    observer.observe(box.current);
    return () => observer.disconnect();
  }, [hint?.id]);

  if (!hint) return null;
  // "deep": окно тащится за любое место карточки; кнопки и ссылки drag.js
  // Tauri пропускает сам, они остаются кликабельными.
  return (
    <div className="p-1" data-tauri-drag-region="deep">
      <div ref={box}>
        <HintCard hint={hint} onClose={() => void invoke('hint_window_hide')} />
      </div>
    </div>
  );
}
