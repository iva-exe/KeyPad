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

/** Která jednorázová animace se má přehrát a její pořadí. */
export interface Prehrani {
	/** Třída s CSS animací z app.css (kp-zablesk, kp-zatreseni …); `''` = nic. */
	trida: string;
	/** Každá nová hodnota přehraje animaci znovu od začátku. */
	id: number;
}

/**
 * Akce Svelte: přehraje jednorázovou CSS animaci pokaždé, když se změní
 * `id` (i stejnou několikrát za sebou). Animace jsou v app.css jako
 * třídy, takže je „Omezit pohyb" vypne týmž pravidlem jako ostatní.
 * Animace přehraná před připojením prvku se nepřehrává — nikdo ji neviděl.
 */
export function prehraj(uzel: HTMLElement, p: Prehrani) {
	let id = p.id;
	let trida = '';
	const konec = (e: AnimationEvent) => {
		if (e.target === uzel && trida) {
			uzel.classList.remove(trida);
			trida = '';
		}
	};
	uzel.addEventListener('animationend', konec);
	return {
		update(n: Prehrani) {
			if (n.id === id) return;
			id = n.id;
			if (trida) uzel.classList.remove(trida);
			trida = n.trida;
			if (!trida) return;
			// Přečtení rozměru vynutí přepočet stylu — bez něj by prohlížeč
			// odebrání a přidání téže třídy slil do jednoho kroku a animace
			// by se znovu nespustila.
			void uzel.offsetWidth;
			uzel.classList.add(trida);
		},
		destroy() {
			uzel.removeEventListener('animationend', konec);
		}
	};
}
