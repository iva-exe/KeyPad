// Schéma ovladače a čisté výpočty okna — bez Svelte a bez Tauri, aby
// je šlo testovat holým `bun test` (vstupy.test.ts).
import type { Cil, Klavesa, PadStav, Vazba, Vstup } from './smlouva';

/**
 * Všech 24 vstupů v pořadí jádra (`Action::ALL`): páčky, tlačítka, D-pad,
 * triggery. Bity živého stavu okno dekóduje podle `KlavesyInfo.vstupy`
 * od backendu, ne podle téhle kopie — ta jen říká, co schéma kreslí.
 * Že sedí se zlatým souborem smlouvy, hlídá test.
 */
export const VSTUPY = [
	'ls_up',
	'ls_down',
	'ls_left',
	'ls_right',
	'rs_up',
	'rs_down',
	'rs_left',
	'rs_right',
	'a',
	'b',
	'x',
	'y',
	'lb',
	'rb',
	'l3',
	'r3',
	'start',
	'back',
	'dpad_up',
	'dpad_down',
	'dpad_left',
	'dpad_right',
	'lt',
	'rt'
] as const satisfies readonly Vstup[];

export const MAX_PADU = 4;

/** Mřížka schématu: 9 sloupců × 7 řádků čepiček. */
export const SLOUPCU = 9;
export const RADKU = 7;

export interface Poloha {
	/** Řádek a sloupec mřížky (od 1, jako CSS grid). */
	r: number;
	s: number;
	/** Popisek v rohu čepičky (A, LB, ◀ …). Páčky a D-pad žádný nemají — význam dává poloha. */
	popisek?: string;
	/** Kulatá hlavička páčky (L3/R3), v náhledu se posouvá. */
	hlavicka?: 'l' | 'p';
}

/**
 * Rozložení jako Xbox: levá páčka vlevo nahoře, tlačítka vpravo nahoře,
 * D-pad a pravá páčka níž a blíž ke středu. Páčka je kříž čtyř čepiček
 * kolem kulaté hlavičky (L3/R3). Sloupec 5 zůstává prázdný — střed
 * ovladače, kde by bylo logo Xboxu (to se nemapuje).
 */
export const POLOHY: Record<Vstup, Poloha> = {
	lt: { r: 1, s: 1, popisek: 'LT' },
	lb: { r: 1, s: 2, popisek: 'LB' },
	rb: { r: 1, s: 8, popisek: 'RB' },
	rt: { r: 1, s: 9, popisek: 'RT' },

	ls_up: { r: 2, s: 2 },
	ls_left: { r: 3, s: 1 },
	l3: { r: 3, s: 2, hlavicka: 'l' },
	ls_right: { r: 3, s: 3 },
	ls_down: { r: 4, s: 2 },

	back: { r: 2, s: 4, popisek: '◀' },
	start: { r: 2, s: 6, popisek: '▶' },

	y: { r: 2, s: 8, popisek: 'Y' },
	x: { r: 3, s: 7, popisek: 'X' },
	b: { r: 3, s: 9, popisek: 'B' },
	a: { r: 4, s: 8, popisek: 'A' },

	dpad_up: { r: 5, s: 3 },
	dpad_left: { r: 6, s: 2 },
	dpad_right: { r: 6, s: 4 },
	dpad_down: { r: 7, s: 3 },

	rs_up: { r: 5, s: 7 },
	rs_left: { r: 6, s: 6 },
	r3: { r: 6, s: 7, hlavicka: 'p' },
	rs_right: { r: 6, s: 8 },
	rs_down: { r: 7, s: 7 }
};

/** Podklad pod skupinami čepiček (oblast mřížky jako CSS `grid-area`). */
export interface Podklad {
	druh: 'jamka' | 'kriz';
	/** řádek od, sloupec od, řádek do, sloupec do (konec bez sebe). */
	oblast: [number, number, number, number];
}

export const PODKLADY: readonly Podklad[] = [
	{ druh: 'jamka', oblast: [2, 1, 5, 4] }, // levá páčka
	{ druh: 'jamka', oblast: [2, 7, 5, 10] }, // A B X Y
	{ druh: 'kriz', oblast: [5, 2, 8, 5] }, // D-pad (✚ uprostřed)
	{ druh: 'jamka', oblast: [5, 6, 8, 9] } // pravá páčka
];

/** České názvy do bublin. */
export const NAZVY: Record<Vstup, string> = {
	ls_up: 'Levá páčka ↑',
	ls_down: 'Levá páčka ↓',
	ls_left: 'Levá páčka ←',
	ls_right: 'Levá páčka →',
	rs_up: 'Pravá páčka ↑',
	rs_down: 'Pravá páčka ↓',
	rs_left: 'Pravá páčka ←',
	rs_right: 'Pravá páčka →',
	a: 'A',
	b: 'B',
	x: 'X',
	y: 'Y',
	lb: 'LB',
	rb: 'RB',
	l3: 'Stisk levé páčky (L3)',
	r3: 'Stisk pravé páčky (R3)',
	start: 'Start',
	back: 'Back',
	dpad_up: 'D-pad ↑',
	dpad_down: 'D-pad ↓',
	dpad_left: 'D-pad ←',
	dpad_right: 'D-pad →',
	lt: 'LT',
	rt: 'RT'
};

/** Krátké názvy do jednořádkové nápovědy („Přesunuto z ovladače 2 · LB"). */
export const KRATKE: Record<Vstup, string> = {
	ls_up: 'levá ↑',
	ls_down: 'levá ↓',
	ls_left: 'levá ←',
	ls_right: 'levá →',
	rs_up: 'pravá ↑',
	rs_down: 'pravá ↓',
	rs_left: 'pravá ←',
	rs_right: 'pravá →',
	a: 'A',
	b: 'B',
	x: 'X',
	y: 'Y',
	lb: 'LB',
	rb: 'RB',
	l3: 'L3',
	r3: 'R3',
	start: 'Start',
	back: 'Back',
	dpad_up: 'D-pad ↑',
	dpad_down: 'D-pad ↓',
	dpad_left: 'D-pad ←',
	dpad_right: 'D-pad →',
	lt: 'LT',
	rt: 'RT'
};

export function jeVstup(v: string): v is Vstup {
	return Object.hasOwn(POLOHY, v);
}

/** Jak vstup svítí: plně (hra ho dostává), obrysem (klávesa je dole, ale hra ho nedostává), vůbec. */
export type Sviti = 'hra' | 'nahled' | null;

/**
 * Svícení vstupu s bitem `i`. Hra má přednost: u A+D svítí plně jen
 * vítěz SOCD, poražený obrysem. Bit `hra` bez `drzi` (páčka při
 * „Vyzkoušet") svítí taky — je to skutečný stav ovladače.
 */
export function sviti(drzi: number, hra: number, i: number | undefined): Sviti {
	if (i === undefined || !Number.isInteger(i) || i < 0 || i > 31) return null;
	if ((hra >>> i) & 1) return 'hra';
	if ((drzi >>> i) & 1) return 'nahled';
	return null;
}

/** Index bitu každého vstupu podle pořadí od backendu (neznámé kódy přeskočí). */
export function indexy(vstupy: readonly string[]): Partial<Record<Vstup, number>> {
	const m: Partial<Record<Vstup, number>> = {};
	vstupy.forEach((v, i) => {
		if (jeVstup(v) && i < 32) m[v] = i;
	});
	return m;
}

/** Vstupy s nastaveným bitem, v pořadí bitů. */
export function dekoduj(bity: number, vstupy: readonly string[]): Vstup[] {
	const ven: Vstup[] = [];
	vstupy.forEach((v, i) => {
		if (i < 32 && jeVstup(v) && (bity >>> i) & 1) ven.push(v);
	});
	return ven;
}

/** O kolik px se v náhledu posune hlavička páčky. */
export const POSUN_HLAVICKY = 6;

/**
 * Posun hlavičky páčky v px (x doprava, y dolů — obrazovka) z výchylky
 * po SOCD (+y = nahoru jako XInput). Po diagonále je posun stejně
 * dlouhý jako rovně, jen šikmo.
 */
export function posunHlavicky(x: number, y: number, krok = POSUN_HLAVICKY): [number, number] {
	const sx = Math.sign(x);
	const sy = Math.sign(y);
	const k = sx !== 0 && sy !== 0 ? krok * Math.SQRT1_2 : krok;
	// `|| 0`: −0 by v CSS nevadil, ale v porovnání (testy, $derived) ano.
	return [sx * k || 0, -sy * k || 0];
}

/** Klávesy každého vstupu jednoho ovladače, v pořadí od backendu. */
export function seskup(vazby: readonly Vazba[], pad: number): Partial<Record<Vstup, Klavesa[]>> {
	const m: Partial<Record<Vstup, Klavesa[]>> = {};
	for (const v of vazby) {
		if (v.pad !== pad) continue;
		(m[v.vstup] ??= []).push(v.klavesa);
	}
	return m;
}

/** Alt (levý i pravý): na vstupu ovladače při hraní spolkne Alt+Tab. */
export function jeAlt(k: { scan: number }): boolean {
	return k.scan === 0x38;
}

/**
 * Vstup, na kterém leží úplně všechny klávesy mapování, nebo `null`.
 * Ten nejde vyprázdnit — prázdné mapování jádro nedovolí (OQ 6).
 */
export function jedinyVstup(vazby: readonly Vazba[]): Cil | null {
	const prvni = vazby[0];
	if (!prvni) return null;
	return vazby.every((v) => v.pad === prvni.pad && v.vstup === prvni.vstup)
		? { pad: prvni.pad, vstup: prvni.vstup }
		: null;
}

export function pocetKlaves(vazby: readonly Vazba[], pad: number): number {
	let n = 0;
	for (const v of vazby) if (v.pad === pad) n++;
	return n;
}

/** Má ovladač všechny klávesy mapování? Pak ho nejde odebrat. */
export function maVsechnyKlavesy(vazby: readonly Vazba[], pad: number): boolean {
	return vazby.length > 0 && vazby.every((v) => v.pad === pad);
}

/**
 * Proč 🗑 nejde (bublina), nebo '' = jde. Backend odebere jen vypnutý
 * ovladač (`off`, spec B4) — okno proto zablokuje každý jiný stav
 * a řekne, co s tím. Jinak by dva kliky skončily radou „Nejdřív ho
 * vypni", kterou bez ViGEmBus splnit nejde (přepínač je zašedlý).
 */
export function procNeodebrat(stav: PadStav, maVsechny: boolean): string {
	switch (stav) {
		case 'off':
			return maVsechny ? 'Nejdřív dej klávesy jinému ovladači' : '';
		case 'on':
		case 'connecting':
			return 'Nejdřív ho vypni';
		case 'error':
			// Porouchaný vypnout nejde — do `off` ho vrátí „Zkusit znovu".
			return 'Nejdřív Zkusit znovu';
		case 'bus_missing':
		case 'bus_not_running':
			return 'Nejdřív musí běžet ViGEmBus';
	}
}

/** Stavy padu, kdy ovladač běží nebo má běžet — jeho karta nesmí zmizet. */
const ZAPNUTE = new Set(['on', 'connecting', 'error']);

/**
 * Které karty ukázat (vzestupně): uložené karty od backendu (OQ 52 —
 * karta zůstane i bez kláves a po restartu, zmizí jen 🗑), ovladač 1
 * vždy, a navíc ovladač s klávesami a zapnutý ovladač — ty jsou vidět
 * vždycky, takže nic nemůže schovat ovladač, který ještě běží nebo hraje.
 */
export function viditelneKarty(
	vazby: readonly { pad: number }[],
	stavy: readonly (string | undefined)[],
	karty: readonly number[]
): number[] {
	const videt = new Set<number>([0]);
	for (const v of vazby) videt.add(v.pad);
	stavy.forEach((s, i) => {
		if (s !== undefined && ZAPNUTE.has(s)) videt.add(i);
	});
	for (const p of karty) videt.add(p);
	return [...videt].filter((p) => Number.isInteger(p) && p >= 0 && p < MAX_PADU).sort((a, b) => a - b);
}

/** Ovladač, který přidá „+ Ovladač" (nejnižší neviditelný), nebo `null` při plném počtu. */
export function dalsiKarta(viditelne: readonly number[]): number | null {
	for (let p = 0; p < MAX_PADU; p++) if (!viditelne.includes(p)) return p;
	return null;
}

/** Bublina čepičky: „Levá páčka ↑ · W", u víc kláves „· také Šipka nahoru". */
export function bublinaVstupu(vstup: Vstup, klavesy: readonly Klavesa[]): string {
	const [prvni, ...dalsi] = klavesy;
	if (!prvni) return `${NAZVY[vstup]} · bez klávesy`;
	let t = `${NAZVY[vstup]} · ${prvni.nazev}`;
	if (dalsi.length > 0) t += ` · také ${dalsi.map((k) => k.nazev).join(', ')}`;
	if (klavesy.some(jeAlt)) t += '\nPři hraní nepůjde Alt+Tab';
	return t;
}

/** Nápověda k odmítnuté klávese (přiřazování běží dál). */
export function textOdmitnuti(duvod: 'zkratka' | 'win' | 'nejde', zkratka?: string): string {
	switch (duvod) {
		case 'zkratka':
			return `${zkratka || 'Zkratka'} je pauza`;
		case 'win':
			return 'Win patří Windows';
		case 'nejde':
			return 'Tuhle klávesu nejde použít';
	}
}

/**
 * Konec bubliny pruhu konfigurace: co je v nevalidním souboru špatně, jak
 * to poslal backend (nejvýš pár vět, spec 1.7), a že zbytek je v logu.
 */
export function bublinaChyb(chyby: readonly string[]): string {
	if (chyby.length === 0) return 'Podrobnosti jsou v logu.';
	// Ne „všechno je v logu": i log má strop (MAX_CHYB_V_LOGU, zbytek počtem).
	return ['Co je v souboru špatně:', ...chyby.map((c) => `· ${c}`), 'Podrobnosti jsou v logu.'].join('\n');
}

/** Nápověda k přesunuté klávese: „Přesunuto z LB", z jiného ovladače i s jeho číslem. */
export function textPresunu(odkud: Cil, pad: number): string {
	const kde = jeVstup(odkud.vstup) ? KRATKE[odkud.vstup] : odkud.vstup;
	return odkud.pad === pad ? `Přesunuto z ${kde}` : `Přesunuto z ovladače ${odkud.pad + 1} · ${kde}`;
}
