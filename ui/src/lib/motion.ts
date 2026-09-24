// Délky přechodů Svelte s ohledem na „omezit pohyb" ve Windows.
//
// CSS pravidlo v app.css vypíná jen CSS přechody a animace. Svelte 5
// ale `transition:` přehrává přes Web Animations API (element.animate),
// na které `animation: none !important` nemá vliv — bez tohohle by
// banner s aktualizací vyjížděl i u uživatele, který si pohyb vypnul.
import { cubicOut } from 'svelte/easing';

const dotaz =
	typeof window !== 'undefined' && typeof window.matchMedia === 'function'
		? window.matchMedia('(prefers-reduced-motion: reduce)')
		: null;

/** Délka přechodu v ms, nebo 0, když si uživatel pohyb nepřeje. */
export function trvani(ms: number): number {
	return dotaz?.matches ? 0 : ms;
}

/** Jednotné zpomalení pro všechny přechody ve Svelte. */
export const zpomaleni = cubicOut;
