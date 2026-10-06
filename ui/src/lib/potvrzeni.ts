// Potvrzovací dialog ↺ a 🗑 (Fáze 7, Z3) — čisté části, ať je zkouší
// holý `bun test` (potvrzeni.test.ts): texty a co udělá klávesa.
//
// Dialog je vrstva ve WebView, ne okno Windows: nesmí vyskočit přes hru
// (žádné okno přes hru v popředí) a nic se kvůli němu neinstaluje.

export type DruhPotvrzeni = 'vychozi' | 'odebrat';

export interface TextyPotvrzeni {
	nadpis: string;
	/** Druhý řádek, nebo '' (ovladač bez kláves nemá co ztratit). */
	popis: string;
	/** Tlačítko, které akci provede. */
	akce: string;
	/** Akce maže (🗑) — tlačítko červeně. */
	nebezpecne: boolean;
}

/** Texty dialogu: krátké, jedna otázka a co se stane. */
export function textyPotvrzeni(druh: DruhPotvrzeni, pad: number, maKlavesy: boolean): TextyPotvrzeni {
	if (druh === 'vychozi') {
		return {
			nadpis: `Vrátit výchozí klávesy ovladače ${pad + 1}?`,
			popis: 'Klávesy jiných ovladačů zůstanou.',
			akce: 'Vrátit',
			nebezpecne: false
		};
	}
	return {
		nadpis: `Odebrat ovladač ${pad + 1}?`,
		popis: maKlavesy ? 'Jeho klávesy se smažou.' : '',
		akce: 'Odebrat',
		nebezpecne: true
	};
}

/** Stisk, jak ho dialog potřebuje — bez DOM. */
export interface StiskDialogu {
	key: string;
	shiftKey: boolean;
	ctrlKey: boolean;
	altKey: boolean;
	metaKey: boolean;
}

export type AkceDialogu = { akce: 'zrusit' } | { akce: 'fokus'; index: number } | null;

/**
 * Co udělá klávesa v otevřeném dialogu: Esc = Zrušit; Tab / Shift+Tab
 * dokola jen mezi tlačítky dialogu (`fokus` = index tlačítka s fokusem, −1
 * = žádné z nich, `pocet` = kolik jich je). Enter a mezerník řeší stráž
 * kláves okna (klavesnice.ts): projdou jen s fokusem z Tabu, takže dialog
 * otevřený myší Enter nepotvrdí — Enter je Start ovladače 1.
 */
export function klavesaDialogu(e: StiskDialogu, fokus: number, pocet: number): AkceDialogu {
	if (e.key === 'Escape' && !e.ctrlKey && !e.altKey && !e.metaKey) return { akce: 'zrusit' };
	if (e.key !== 'Tab' || e.ctrlKey || e.altKey || e.metaKey || pocet <= 0) return null;
	if (fokus < 0 || fokus >= pocet) return { akce: 'fokus', index: e.shiftKey ? pocet - 1 : 0 };
	const krok = e.shiftKey ? -1 : 1;
	return { akce: 'fokus', index: (fokus + krok + pocet) % pocet };
}
