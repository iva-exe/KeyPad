// Živý stav vstupů: co je na klávesnici dole a co z toho hra dostává.
//
// Backend ho posílá událostí `zive` jen s viditelným oknem a nejvýš
// jednou za 16 ms; po návratu okna se načte příkazem `zive`. Okno nikdy
// nedostane identitu stisknuté klávesy, jen bity vstupů — nenamapovaná
// klávesa nerozsvítí nic. Starší snímek (odpověď po novější události)
// zahodí `seq`.
import { MAX_PADU } from './vstupy';
import type { ZivaInfo, ZivyPad } from './smlouva';
import { poslouchej, zavolej } from './tauri';

function prazdny(): ZivyPad {
	return { drzi: 0, hra: 0, l: [0, 0], p: [0, 0] };
}

export const zive = $state({
	/** Pořadí posledního převzatého snímku; −1 = zatím nic. */
	seq: -1,
	pady: Array.from({ length: MAX_PADU }, prazdny)
});

function stejnaOsa(a: [number, number], b: readonly number[]): boolean {
	return a[0] === b[0] && a[1] === b[1];
}

function prevezmi(z: ZivaInfo): void {
	if (z.seq < zive.seq) return;
	zive.seq = z.seq;
	// Zapisovat jen změny: událost chodí až 60× za sekundu a každý zápis
	// do $state překreslí čepičky, které na něm závisí.
	for (let i = 0; i < MAX_PADU; i++) {
		const n = z.pady[i] ?? prazdny();
		const s = zive.pady[i]!;
		if (s.drzi !== n.drzi) s.drzi = n.drzi;
		if (s.hra !== n.hra) s.hra = n.hra;
		if (!stejnaOsa(s.l, n.l)) s.l = [n.l[0] ?? 0, n.l[1] ?? 0];
		if (!stejnaOsa(s.p, n.p)) s.p = [n.p[0] ?? 0, n.p[1] ?? 0];
	}
}

export async function prihlasZive(): Promise<void> {
	await poslouchej<ZivaInfo>('zive', prevezmi);
}

export async function nactiZive(): Promise<void> {
	try {
		prevezmi(await zavolej<ZivaInfo>('zive'));
	} catch {
		// Bez backendu nic nesvítí.
	}
}
