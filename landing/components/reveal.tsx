'use client';

import { usePathname } from 'next/navigation';
import { useEffect } from 'react';

// Marks each `.reveal` element with `.in` once it scrolls into view, which starts its chart race
// (CSS in globals.css). The `io` class on <html>, set by an inline script in the layout before
// first paint, is what holds the bars at zero until then; without JavaScript, or under reduced
// motion, it is never set and every chart is drawn complete. Re-runs on client-side navigation.
export function Reveal() {
  const path = usePathname();
  useEffect(() => {
    const els = document.querySelectorAll<HTMLElement>('.reveal:not(.in)');
    if (!document.documentElement.classList.contains('io')) return;
    const io = new IntersectionObserver(
      entries => {
        for (const e of entries) {
          if (!e.isIntersecting) continue;
          e.target.classList.add('in');
          io.unobserve(e.target);
        }
      },
      { rootMargin: '0px 0px -10% 0px' },
    );
    els.forEach(el => io.observe(el));
    return () => io.disconnect();
  }, [path]);
  return null;
}

