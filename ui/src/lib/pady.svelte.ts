// Stav čtyř virtuálních ovladačů pro celé okno.
//
// Zdroj pravdy je backend (pad vlákna, src-tauri/src/platform/windows/
// pad.rs). Sem teče událostí `pad-stav` při každé změně a příkazem
// `pady` při startu a po návratu okna. Každý ovladač má vlastní `seq`:
// odpovědi a události se můžou předběhnout a starší změna se zahodí.
//
// Ovladač se připojuje JEN přepínačem (`pad_on`) — po startu, po
// probuzení i po aktualizaci je vypnutý (princip 11). Stav sběrnice
// (chybí / neběží / starší) hlásí vlákno ovladače 1, které běží vždy;
// akce sběrnice proto míří na ovladač 0.
import { MAX_PADU } from './vstupy';
import type { Odkaz, PadInfo, PadStav } from './smlouva';
import { poslouchej, textChyby, zavolej } from './tauri';

interface PadOkno {
	state: PadStav;
	/** Podrobnost od backendu (proč chyba, rada) — do bubliny. */
	detail: string;
	/** ViGEmBus je starší než z posledního vydání. */
	stary: boolean;
	/** Instalátor ViGEmBus běží — hlásí backend, dokud neskončí. */
	installer: boolean;
	/** Pořadí poslední převzaté změny; −1 = zatím nic. */
	seq: number;
	/** Přepínač čeká na odpověď backendu — ukazuje, co uživatel chce. */
	prepina: boolean;
	/** Kam uživatel přepínač přepnul (platí jen s `prepina`). */
	chce: boolean;
	/** Páčka zrovna opisuje kruh — tlačítko je chvíli zablokované. */
	testuje: boolean;
	/** Chyba posledního kliknutí na kartě (přepínač, Vyzkoušet). */
	chybaAkce: string;
}

function novyPad(): PadOkno {
	return {
		state: 'off',
		detail: '',
		stary: false,
		installer: false,
		seq: -1,
		prepina: false,
		chce: false,
		testuje: false,
		chybaAkce: ''
	};
}

export const pady: PadOkno[] = $state(Array.from({ length: MAX_PADU }, novyPad));

/** Akce sběrnice (instalace, „Zkusit znovu", stažení) — mluví pruh nad kartami. */
export const sbernice = $state({
	/** Spouští se instalátor ViGEmBus (příkaz ještě neodpověděl). */
	spoustiInstalator: false,
	chyba: ''
});

/** Je ovladač zapnutý (nebo se právě zapíná)? */
export function zapnuto(pad: number): boolean {
	const s = pady[pad]?.state;
	return s === 'on' || s === 'connecting';
}

/** Je ViGEmBus k použití (podle ovladače 1)? */
export function sbernicePripravena(): boolean {
	const s = pady[0]?.state;
	return s !== 'bus_missing' && s !== 'bus_not_running';
}

/** Běží (nebo se spouští) instalátor ViGEmBus? */
export function instalatorBezi(): boolean {
	return sbernice.spoustiInstalator || pady.some((p) => p.installer);
}

const casovacePrepnuti: (ReturnType<typeof setTimeout> | undefined)[] = [];

function prevezmi(i: PadInfo): void {
	const p = pady[i.pad];
	if (!p) return;
	// Stejný `seq` projde: backend zapisuje a čte celý stav najednou,
	// takže stejný `seq` = stejný obsah.
	if (i.seq < p.seq) return;
	const nova = i.seq > p.seq;
	p.state = i.state;
	p.detail = i.detail ?? '';
	p.stary = !!i.needs_update;
	// Z backendu, ne odhadem okna: vypnutí padu před instalátorem se
	// může ohlásit dřív i později než odpověď `install_vigembus`.
	p.installer = !!i.installer;
	p.seq = i.seq;
	if (nova) {
		// Backend odpověděl — přepínač zase ukazuje skutečnost.
		p.prepina = false;
		clearTimeout(casovacePrepnuti[i.pad]);
	}
	if (i.state !== 'on') p.testuje = false;
}

/** Přihlásí se k událostem — vždy dřív, než se stav načte. */
export async function prihlasPady(): Promise<void> {
	await poslouchej<PadInfo>('pad-stav', prevezmi);
}

export async function nactiPady(): Promise<void> {
	try {
		for (const i of await zavolej<PadInfo[]>('pady')) prevezmi(i);
	} catch {
		// Backend neodpověděl — zůstane „vypnuto".
	}
}

/**
 * Jak dlouho přepínač ukazuje přání uživatele, když backend mlčí.
 * Normálně odpoví do milisekund („zapínám…"); kdyby příkaz nic
 * nezměnil, přepínač se po chvíli vrátí ke skutečnosti.
 */
const PREPNUTI_MS = 2000;

/** Přepínač: zapnout / vypnout virtuální ovladač. */
export async function prepni(pad: number): Promise<void> {
	const p = pady[pad];
	if (!p) return;
	// Podle toho, co přepínač UKAZUJE (dvojklik před odpovědí backendu
	// je „zapnout a zase vypnout", ne dvakrát „zapnout").
	const chce = !(p.prepina ? p.chce : zapnuto(pad));
	p.chybaAkce = '';
	p.prepina = true;
	p.chce = chce;
	clearTimeout(casovacePrepnuti[pad]);
	casovacePrepnuti[pad] = setTimeout(() => (p.prepina = false), PREPNUTI_MS);
	try {
		await zavolej(chce ? 'pad_on' : 'pad_off', { pad });
	} catch (e) {
		p.chybaAkce = textChyby(e);
		p.prepina = false;
	}
}

/** Jak dlouho blokovat „Vyzkoušet" — kruh trvá 1,2 s. */
const TEST_MS = 1300;

export async function vyzkousej(pad: number): Promise<void> {
	const p = pady[pad];
	if (!p || p.testuje) return;
	p.chybaAkce = '';
	p.testuje = true;
	try {
		await zavolej('pad_test', { pad });
		setTimeout(() => (p.testuje = false), TEST_MS);
	} catch (e) {
		p.chybaAkce = textChyby(e);
		p.testuje = false;
	}
}

/** „Zkusit znovu" — sběrnici znovu ověří všechny ovladače. Nic nepřipojí. */
export async function zkusZnovu(): Promise<void> {
	sbernice.chyba = '';
	try {
		await zavolej('pad_retry');
	} catch (e) {
		sbernice.chyba = textChyby(e);
	}
}

/**
 * Instalace chybějícího nebo aktualizace staršího ViGEmBus. Zapnuté
 * ovladače backend před spuštěním vypne; „instalátor běží" pak hlásí
 * událostí `pad-stav` (`installer`), až do konce instalátoru.
 */
export async function instalujOvladac(): Promise<void> {
	if (instalatorBezi()) return;
	sbernice.chyba = '';
	sbernice.spoustiInstalator = true;
	try {
		await zavolej('install_vigembus');
	} catch (e) {
		sbernice.chyba = textChyby(e);
	}
	sbernice.spoustiInstalator = false;
}

/**
 * Otevře pevnou stránku v prohlížeči (adresy drží backend). Vrací
 * text chyby, nebo prázdný řetězec — kam ji ukázat, rozhodne volající.
 */
export async function otevri(link: Odkaz): Promise<string> {
	try {
		await zavolej('open_link', { link });
		return '';
	} catch (e) {
		return textChyby(e);
	}
}
