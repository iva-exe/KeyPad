// Maketa backendu — JEN `bun run dev` v obyčejném prohlížeči.
//
// Okno se tak dá ladit s daty a se skutečnou klávesnicí bez buildu Rustu
// (přiřazování, přesun, „Zpět", svícení, SOCD). Do buildu se nedostane:
// tauri.ts ji načítá jen ve větvi `import.meta.env.DEV`, kterou Vite
// v produkci nahradí `false` a Rollup zahodí i s tímhle modulem.
//
// Chová se podle smlouvy (smlouva.ts, spec B4), ne podle skutečného
// enginu — co tu sedí, nemusí sedět v Rustu. Výchozí klávesy bere ze
// zlatého souboru smlouvy, aby maketa a test okna mluvily o tomtéž.
//
// Scénáře pro snímky rozvržení (parametry adresy, lze kombinovat):
//   ?sbernice=chybi|nebezi|stary|instalator   stav ViGEmBus
//   ?konfigurace=obnovena|novejsi|necitelna|neulozena
//   ?karty=4          klávesy i pro ovladače 3 a 4
//   ?zapnuto=1        ovladač 1 zapnutý (hraje)
//   ?zive=zlaty       živý stav ze zlatého souboru (A+D, šipka nahoru…)
//   ?prirazovani=a    přiřazuje se vstup ovladače 1
//   ?chyba=2          ovladač 2 v chybě
//   ?hook=chyba       Windows nedovolily hook
import klavesyJson from './testdata/klavesy.json';
import ziveJson from './testdata/zive.json';
import type {
	Cil,
	KartyInfo,
	Klavesa,
	KlavesyInfo,
	Oznameni,
	PadInfo,
	PadStav,
	RezimInfo,
	StavKonfigurace,
	Vazba,
	ZivaInfo,
	ZmenaKlaves
} from './smlouva';
import { jeVstup, MAX_PADU, VSTUPY } from './vstupy';

type Obsluha = (data: unknown) => void;

const posluchaci = new Map<string, Set<Obsluha>>();

/** Jako `listen` z Tauri: obsluha dostane rovnou obsah události. */
export function poslouchej(udalost: string, obsluha: Obsluha): () => void {
	const mnozina = posluchaci.get(udalost) ?? new Set<Obsluha>();
	posluchaci.set(udalost, mnozina);
	mnozina.add(obsluha);
	return () => void mnozina.delete(obsluha);
}

/** Události chodí asynchronně jako z backendu (a po odpovědi příkazu). */
function vydej(udalost: string, data: unknown): void {
	setTimeout(() => posluchaci.get(udalost)?.forEach((o) => o(structuredClone(data))), 0);
}

// ── klávesy podle KeyboardEvent.code (scan kód sady 1, jako hook) ──

/** code → [scan, e0, název, krátký název]; písmena a čísla bere název z rozložení. */
const KLAVESY: Record<string, [number, boolean, string, string?]> = {
	Escape: [0x01, false, 'Esc'],
	Minus: [0x0c, false, ''],
	Equal: [0x0d, false, ''],
	Backspace: [0x0e, false, 'Backspace', '⌫'],
	Tab: [0x0f, false, 'Tab', '⇥'],
	BracketLeft: [0x1a, false, ''],
	BracketRight: [0x1b, false, ''],
	Enter: [0x1c, false, 'Enter', '↵'],
	ControlLeft: [0x1d, false, 'Ctrl'],
	Semicolon: [0x27, false, ''],
	Quote: [0x28, false, ''],
	Backquote: [0x29, false, ''],
	ShiftLeft: [0x2a, false, 'Shift', '⇧'],
	Backslash: [0x2b, false, ''],
	Comma: [0x33, false, ''],
	Period: [0x34, false, ''],
	Slash: [0x35, false, ''],
	ShiftRight: [0x36, false, 'Pravý Shift', 'P⇧'],
	NumpadMultiply: [0x37, false, 'Num *'],
	AltLeft: [0x38, false, 'Alt'],
	Space: [0x39, false, 'Mezerník', '␣'],
	CapsLock: [0x3a, false, 'Caps Lock', 'Caps'],
	ScrollLock: [0x46, false, 'Scroll Lock'],
	Numpad7: [0x47, false, 'Num 7'],
	Numpad8: [0x48, false, 'Num 8'],
	Numpad9: [0x49, false, 'Num 9'],
	NumpadSubtract: [0x4a, false, 'Num -'],
	Numpad4: [0x4b, false, 'Num 4'],
	Numpad5: [0x4c, false, 'Num 5'],
	Numpad6: [0x4d, false, 'Num 6'],
	NumpadAdd: [0x4e, false, 'Num +'],
	Numpad1: [0x4f, false, 'Num 1'],
	Numpad2: [0x50, false, 'Num 2'],
	Numpad3: [0x51, false, 'Num 3'],
	Numpad0: [0x52, false, 'Num 0'],
	NumpadDecimal: [0x53, false, 'Num Del'],
	IntlBackslash: [0x56, false, ''],
	F11: [0x57, false, 'F11'],
	F12: [0x58, false, 'F12'],
	NumpadEnter: [0x1c, true, 'Num Enter'],
	ControlRight: [0x1d, true, 'Pravý Ctrl', 'P Ctrl'],
	NumpadDivide: [0x35, true, 'Num /'],
	AltRight: [0x38, true, 'Pravý Alt', 'P Alt'],
	Home: [0x47, true, 'Home'],
	ArrowUp: [0x48, true, 'Šipka nahoru', '↑'],
	PageUp: [0x49, true, 'Page Up'],
	ArrowLeft: [0x4b, true, 'Šipka vlevo', '←'],
	ArrowRight: [0x4d, true, 'Šipka vpravo', '→'],
	End: [0x4f, true, 'End'],
	ArrowDown: [0x50, true, 'Šipka dolů', '↓'],
	PageDown: [0x51, true, 'Page Down'],
	Insert: [0x52, true, 'Insert'],
	Delete: [0x53, true, 'Delete'],
	MetaLeft: [0x5b, true, 'Levá Win'],
	MetaRight: [0x5c, true, 'Pravá Win'],
	ContextMenu: [0x5d, true, 'Aplikace']
};
'QWERTYUIOP'.split('').forEach((p, i) => (KLAVESY[`Key${p}`] = [0x10 + i, false, '']));
'ASDFGHJKL'.split('').forEach((p, i) => (KLAVESY[`Key${p}`] = [0x1e + i, false, '']));
'ZXCVBNM'.split('').forEach((p, i) => (KLAVESY[`Key${p}`] = [0x2c + i, false, '']));
'1234567890'.split('').forEach((p, i) => (KLAVESY[`Digit${p}`] = [0x02 + i, false, '']));
for (let i = 1; i <= 10; i++) KLAVESY[`F${i}`] = [0x3a + i, false, `F${i}`];

function klavesaZUdalosti(e: KeyboardEvent): Klavesa | null {
	const k = KLAVESY[e.code];
	if (!k) return null;
	// Prázdný název = podle rozložení (na české klávesnici „ě", „ů"…),
	// jako GetKeyNameTextW v backendu.
	const nazev = k[2] || (e.key.length === 1 ? e.key.toUpperCase() : e.code);
	return { scan: k[0], e0: k[1], nazev, kratky: k[3] ?? nazev.slice(0, 5) };
}

const ZKRATKA = klavesyJson.zkratka as Klavesa;
const jeWin = (k: { scan: number; e0: boolean }) => k.e0 && (k.scan === 0x5b || k.scan === 0x5c);
const stejna = (a: { scan: number; e0: boolean }, b: { scan: number; e0: boolean }) =>
	a.scan === b.scan && a.e0 === b.e0;
/** Pořadí `Mapping::bindings()`: běžné klávesy podle scan kódu, pak E0. */
const poradi = (v: Vazba) => (v.klavesa.e0 ? 0x80 : 0) + v.klavesa.scan;

// ── stav ──

const adresa = new URLSearchParams(location.search);

const VYCHOZI: Vazba[] = (klavesyJson.vazby as Vazba[]).filter((v) => v.pad === 0 && v.klavesa.scan !== 0x38);

let vazby: Vazba[] = structuredClone(klavesyJson.vazby as Vazba[]);
if (adresa.get('karty') === '4') {
	vazby.push(
		{ pad: 2, vstup: 'a', klavesa: { scan: 0x3b, e0: false, nazev: 'F1', kratky: 'F1' } },
		{ pad: 3, vstup: 'a', klavesa: { scan: 0x3c, e0: false, nazev: 'F2', kratky: 'F2' } }
	);
	vazby.sort((a, b) => poradi(a) - poradi(b));
}
let rev = klavesyJson.rev;
/** Ovladače s kartou (OQ 52) — jako backend: 0 vždy, ovladač s klávesami taky. */
const karty = new Set<number>([0, ...vazby.map((v) => v.pad)]);
let kartyRev = 0;

function kartyInfo(): KartyInfo {
	return { rev: kartyRev, pady: [...karty].sort((a, b) => a - b) };
}

function pridejKartu(pad: number): void {
	if (karty.has(pad)) return;
	karty.add(pad);
	kartyRev++;
}

/** Mapování před poslední změnou — pro „Zpět" (jen přesný předchůdce). */
let predchozi: { rev: number; vazby: Vazba[] } | null = null;
let konfigurace = (adresa.get('konfigurace') ?? 'ok') as StavKonfigurace;

function padInfo(pad: number): PadInfo {
	const s = adresa.get('sbernice');
	const state: PadStav = s === 'chybi' ? 'bus_missing' : s === 'nebezi' ? 'bus_not_running' : 'off';
	return {
		pad,
		state,
		player: null,
		detail:
			state === 'bus_not_running'
				? 'Zbytek staré instalace — nainstaluj ViGEmBus znovu: https://github.com/nefarius/ViGEmBus/releases'
				: '',
		needs_update: s === 'stary',
		installer: s === 'instalator',
		seq: 1
	};
}

const pady: PadInfo[] = Array.from({ length: MAX_PADU }, (_, i) => padInfo(i));
const chybny = Number(adresa.get('chyba')) - 1;
if (pady[chybny]) {
	pady[chybny].state = 'error';
	pady[chybny].detail = 'Ovladač přestal odpovídat (maketa).';
}
if (adresa.get('zapnuto') === '1' || adresa.get('zive') === 'zlaty') pady[0]!.state = 'on';

const hookChyba = adresa.get('hook') === 'chyba';
let pauza = false;
let rezimSeq = 1;
let prirazovani: { cil: Cil; pridat: boolean; casovac: ReturnType<typeof setTimeout> } | null = null;
let oznameniSeq = 0;
let ziveSeq = 0;

function rezimTed(): RezimInfo['rezim'] {
	if (prirazovani) return 'binding';
	if (!pady.some((p) => p.state === 'on')) return 'disabled';
	if (hookChyba) return 'no_hook';
	return pauza ? 'paused' : 'capturing';
}

function rezimInfo(): RezimInfo {
	return {
		rezim: rezimTed(),
		seq: rezimSeq,
		cil: prirazovani ? { ...prirazovani.cil } : null,
		hook_chyba: hookChyba
	};
}

function zmenRezim(): void {
	rezimSeq++;
	vydej('rezim', rezimInfo());
	vydejZive();
}

function oznam(o: Record<string, unknown>): void {
	oznameniSeq = (oznameniSeq + 1) & 0xffff;
	vydej('oznameni', { seq: oznameniSeq, mezera: false, ...o } as Oznameni);
}

function klavesyInfo(): KlavesyInfo {
	return {
		rev,
		vstupy: [...VSTUPY],
		zkratka: ZKRATKA,
		vazby: structuredClone(vazby),
		zpet: predchozi !== null && predchozi.rev === rev,
		konfigurace,
		zaloha: konfigurace === 'obnovena' ? 'C:\\Users\\hrac\\AppData\\Roaming\\KeyPad\\config.invalid.json' : null,
		chyby:
			konfigurace === 'obnovena' || konfigurace === 'necitelna'
				? ['řádek 7: expected value', 'vazba č. 3: neznámý vstup "skok"']
				: [],
		karty: kartyInfo()
	};
}

function zmenVazby(nove: Vazba[]): void {
	predchozi = { rev: rev + 1, vazby };
	vazby = [...nove].sort((a, b) => poradi(a) - poradi(b));
	rev++;
	// Backend posílá jen revizi; okno si klávesy načte samo (rozložení).
	vydej('klavesy-zmena', { rev });
	if (konfigurace === 'obnovena') {
		konfigurace = 'ok';
		vydej('konfigurace', { stav: konfigurace });
	}
}

// ── živý stav: držené klávesy, SOCD (vyhrává naposledy stisknutá) ──

/** Držené mapované klávesy v pořadí stisku. */
let drzene: Klavesa[] = [];

function osa(minus: number, plus: number, poradi: (b: number) => number): number {
	const m = poradi(minus);
	const p = poradi(plus);
	if (m < 0 && p < 0) return 0;
	return p > m ? 1 : -1;
}

function ziva(): ZivaInfo {
	const vysledek = Array.from({ length: MAX_PADU }, () => ({
		drzi: 0,
		hra: 0,
		l: [0, 0] as [number, number],
		p: [0, 0] as [number, number]
	}));
	const poradi: number[][] = vysledek.map(() => Array(24).fill(-1));
	drzene.forEach((k, n) => {
		for (const v of vazby) {
			if (!stejna(v.klavesa, k)) continue;
			const i = VSTUPY.indexOf(v.vstup);
			vysledek[v.pad]!.drzi |= 1 << i;
			poradi[v.pad]![i] = n;
		}
	});
	const hraje = rezimTed() === 'capturing';
	vysledek.forEach((z, pad) => {
		const por = (b: number) => poradi[pad]![b]!;
		z.l = [osa(2, 3, por), osa(1, 0, por)];
		z.p = [osa(6, 7, por), osa(5, 4, por)];
		if (!hraje || pady[pad]!.state !== 'on') return;
		// Hra dostane tlačítka a jen vítězné směry páček.
		let hra = z.drzi & ~0xff;
		if (z.l[0]) hra |= 1 << (z.l[0] > 0 ? 3 : 2);
		if (z.l[1]) hra |= 1 << (z.l[1] > 0 ? 0 : 1);
		if (z.p[0]) hra |= 1 << (z.p[0] > 0 ? 7 : 6);
		if (z.p[1]) hra |= 1 << (z.p[1] > 0 ? 4 : 5);
		z.hra = hra >>> 0;
	});
	return { seq: ziveSeq, pady: vysledek };
}

let zlata = adresa.get('zive') === 'zlaty';

function vydejZive(): void {
	ziveSeq++;
	vydej('zive', zlata ? { ...structuredClone(ziveJson), seq: ziveSeq } : ziva());
}

// ── přiřazování ──

/** Konec přiřazování: jako backend nejdřív režim, pak oznámení. */
function konecPrirazovani(oznameni?: Record<string, unknown>): void {
	if (prirazovani) clearTimeout(prirazovani.casovac);
	prirazovani = null;
	zmenRezim();
	if (oznameni) oznam(oznameni);
}

function prirad(cil: Cil, pridat: boolean): void {
	if (prirazovani) clearTimeout(prirazovani.casovac);
	const casovac = setTimeout(() => konecPrirazovani({ typ: 'zruseno', duvod: 'cas' }), 10_000);
	prirazovani = { cil, pridat, casovac };
	zmenRezim();
	if (hookChyba) {
		// Hook nejde → přiřazování backend hned zruší (spec 1.4).
		setTimeout(konecPrirazovani, 50);
	}
}

function uloz(k: Klavesa): void {
	const p = prirazovani!;
	const puvodni = vazby.find((v) => stejna(v.klavesa, k));
	const odkud = puvodni && !(puvodni.pad === p.cil.pad && puvodni.vstup === p.cil.vstup) ? puvodni : null;
	let nove = vazby.filter((v) => !stejna(v.klavesa, k));
	if (!p.pridat) nove = nove.filter((v) => !(v.pad === p.cil.pad && v.vstup === p.cil.vstup));
	nove.push({ pad: p.cil.pad, vstup: p.cil.vstup, klavesa: k });
	konecPrirazovani({
		typ: 'ulozeno',
		pad: p.cil.pad,
		vstup: p.cil.vstup,
		klavesa: { scan: k.scan, e0: k.e0 },
		odkud: odkud ? { pad: odkud.pad, vstup: odkud.vstup } : null
	});
	zmenVazby(nove);
}

// ── klávesnice prohlížeče místo hooku ──

function dolu(e: KeyboardEvent): void {
	if (e.repeat) return;
	const k = klavesaZUdalosti(e);
	const pozice = k ? { scan: k.scan, e0: k.e0 } : { scan: 0, e0: false };
	if (prirazovani) {
		if (k?.scan === 0x01) konecPrirazovani({ typ: 'zruseno', duvod: 'esc' });
		else if (k && stejna(k, ZKRATKA)) oznam({ typ: 'odmitnuto', duvod: 'zkratka', klavesa: pozice });
		else if (k && jeWin(k)) oznam({ typ: 'odmitnuto', duvod: 'win', klavesa: pozice });
		else if (!k) oznam({ typ: 'odmitnuto', duvod: 'nejde', klavesa: pozice });
		else uloz(k);
		return;
	}
	if (!k) return;
	if (stejna(k, ZKRATKA)) {
		if (pady.some((p) => p.state === 'on')) {
			pauza = !pauza;
			zmenRezim();
		} else oznam({ typ: 'zapni_ovladac' });
		return;
	}
	if (!vazby.some((v) => stejna(v.klavesa, k))) return;
	zlata = false;
	drzene = [...drzene.filter((d) => !stejna(d, k)), k];
	vydejZive();
}

function nahoru(e: KeyboardEvent): void {
	const k = klavesaZUdalosti(e);
	if (!k || !drzene.some((d) => stejna(d, k))) return;
	drzene = drzene.filter((d) => !stejna(d, k));
	vydejZive();
}

window.addEventListener('keydown', dolu);
window.addEventListener('keyup', nahoru);
// Okno ztratilo popředí: přiřazování se ruší, klávesy nikdo nedrží.
window.addEventListener('blur', () => {
	if (prirazovani) konecPrirazovani({ typ: 'zruseno', duvod: 'okno' });
	if (drzene.length > 0) {
		drzene = [];
		vydejZive();
	}
});

const startovni = adresa.get('prirazovani');
if (startovni && jeVstup(startovni)) prirad({ pad: 0, vstup: startovni }, false);

// ── příkazy ──

function cislo(pad: unknown): number {
	const i = Number(pad ?? 0);
	if (!Number.isInteger(i) || i < 0 || i >= MAX_PADU) throw 'Takový ovladač není.';
	return i;
}

function ohlasPad(pad: number): void {
	pady[pad]!.seq++;
	vydej('pad-stav', { ...pady[pad]! });
}

function uprav(z: ZmenaKlaves): void {
	switch (z.typ) {
		case 'vyprazdnit': {
			// Karta s poslední klávesou nezmizí — schová ji jen 🗑 (OQ 52).
			pridejKartu(z.pad);
			const zbyle = vazby.filter((v) => !(v.pad === z.pad && v.vstup === z.vstup));
			if (zbyle.length === vazby.length) return;
			if (zbyle.length === 0) throw 'Poslední klávesu nejde odebrat.';
			zmenVazby(zbyle);
			return;
		}
		case 'vychozi': {
			// Klávesy jiných ovladačů se nebrání (OQ 49): vstup zůstane prázdný.
			const cizi = vazby.filter((v) => v.pad !== 0);
			const nove = VYCHOZI.filter((d) => !cizi.some((c) => stejna(c.klavesa, d.klavesa)));
			zmenVazby([...structuredClone(nove), ...cizi]);
			return;
		}
		case 'zpet': {
			if (!predchozi || predchozi.rev !== rev || z.rev !== rev) throw 'Vrátit už nejde.';
			const stare = predchozi.vazby;
			zmenVazby(stare);
			predchozi = null;
			return;
		}
	}
}

function obsluz(prikaz: string, a: Record<string, unknown>): unknown {
	switch (prikaz) {
		case 'klavesy':
			return klavesyInfo();
		case 'pady':
			return pady.map((p) => ({ ...p }));
		case 'pad_status':
			return { ...pady[cislo(a.pad)]! };
		case 'rezim':
			return rezimInfo();
		case 'zive':
			return zlata ? { ...structuredClone(ziveJson), seq: ziveSeq } : ziva();
		case 'prirad': {
			const pad = cislo(a.pad);
			if (typeof a.vstup !== 'string' || !jeVstup(a.vstup)) throw 'Takový vstup není.';
			prirad({ pad, vstup: a.vstup }, !!a.pridat);
			return null;
		}
		case 'zrus_prirazeni':
			if (prirazovani) konecPrirazovani({ typ: 'zruseno', duvod: 'okno' });
			return null;
		case 'uprav_klavesy':
			uprav(a.zmena as ZmenaKlaves);
			return null;
		case 'odeber_ovladac': {
			const pad = cislo(a.pad);
			if (pad === 0) throw 'Ovladač 1 nejde odebrat.';
			if (pady[pad]!.state !== 'off') throw 'Nejdřív ho vypni.';
			const zbyle = vazby.filter((v) => v.pad !== pad);
			if (zbyle.length === 0) throw 'Nejdřív dej klávesy jinému ovladači.';
			if (zbyle.length !== vazby.length) zmenVazby(zbyle);
			if (karty.delete(pad)) kartyRev++;
			return kartyInfo();
		}
		case 'pridej_kartu': {
			const pad = cislo(a.pad);
			if (pad === 0) throw 'Ovladač 1 má kartu vždy.';
			pridejKartu(pad);
			return kartyInfo();
		}
		case 'pad_on': {
			const pad = cislo(a.pad);
			const p = pady[pad]!;
			if (p.state === 'bus_missing' || p.state === 'bus_not_running') throw 'ViGEmBus není připravený.';
			p.state = 'connecting';
			p.detail = '';
			ohlasPad(pad);
			setTimeout(() => {
				if (p.state !== 'connecting') return;
				p.state = 'on';
				ohlasPad(pad);
				pauza = false;
				zmenRezim();
			}, 400);
			return null;
		}
		case 'pad_off': {
			const pad = cislo(a.pad);
			if (pady[pad]!.state === 'off') return null;
			pady[pad]!.state = 'off';
			ohlasPad(pad);
			zmenRezim();
			return null;
		}
		case 'pad_test':
			if (pady[cislo(a.pad)]!.state !== 'on') throw 'Ovladač je vypnutý.';
			return null;
		case 'pad_retry':
			pady.forEach((p, i) => {
				if (p.state === 'error') p.state = 'off';
				ohlasPad(i);
			});
			return null;
		case 'install_vigembus':
			throw 'V maketě se nic neinstaluje.';
		case 'open_link':
			console.info('maketa: open_link', a.link);
			return null;
		default:
			throw `Maketa nezná příkaz ${prikaz}.`;
	}
}

/** Jako `invoke` z Tauri: chyba je prostý řetězec, odpověď přijde asynchronně. */
export function zavolej<T>(prikaz: string, args?: Record<string, unknown>): Promise<T> {
	return new Promise<T>((hotovo, chyba) => {
		setTimeout(() => {
			try {
				hotovo(obsluz(prikaz, args ?? {}) as T);
			} catch (e) {
				chyba(e);
			}
		}, 10);
	});
}
