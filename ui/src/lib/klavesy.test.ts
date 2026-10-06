// Stav kláves okna a pořadí událostí z backendu (bun test).
//
// Skutečný modul `klavesy.svelte.ts` (runy přeloží src/bun-svelte.ts),
// backend je podvržený: `./tauri` zachytí posluchače událostí a příkazy
// obslouží jako backend podle smlouvy (spec B4). Události chodí v pořadí
// vlákna `keypad-okno`: nejdřív `rezim`, pak `oznameni`.
import { describe, expect, mock, test } from 'bun:test';
import klavesyJson from './testdata/klavesy.json';
import type { Cil, KlavesyInfo, Rezim, Vazba, ZmenaKlaves } from './smlouva';
import { KRATKE, viditelneKarty } from './vstupy';

type Obsluha = (data: unknown) => void;

const posluchaci = new Map<string, Obsluha>();
const prikazy: string[] = [];

const zlate = klavesyJson as unknown as KlavesyInfo;
/** Backend po restartu: ovladač 2 má jedinou klávesu z config.json. */
const backend = {
	klavesy: {
		...structuredClone(zlate),
		vazby: [
			...zlate.vazby.filter((v) => v.pad === 0),
			...zlate.vazby.filter((v) => v.pad === 1).slice(0, 1)
		] as Vazba[],
		zpet: false
	} as KlavesyInfo
};
const KLAVESA_2 = backend.klavesy.vazby.find((v) => v.pad === 1)!;

function uprav(z: ZmenaKlaves): void {
	if (z.typ !== 'vyprazdnit') throw new Error(`test neumí ${z.typ}`);
	const k = backend.klavesy;
	k.vazby = k.vazby.filter((v) => !(v.pad === z.pad && v.vstup === z.vstup));
	k.rev++;
	k.zpet = true;
	// Jako vlákno okna: revize jde událostí, klávesy si okno načte samo.
	const rev = k.rev;
	setTimeout(() => posli('klavesy-zmena', { rev }), 0);
}

mock.module('./tauri', () => ({
	vAplikaci: true,
	maBackend: true,
	hlavniOkno: () => null,
	textChyby: (e: unknown) => String(e),
	poslouchej: async (udalost: string, obsluha: Obsluha) => {
		posluchaci.set(udalost, obsluha);
		return () => posluchaci.delete(udalost);
	},
	zavolej: async (prikaz: string, args?: Record<string, unknown>) => {
		prikazy.push(prikaz);
		switch (prikaz) {
			case 'klavesy':
				return structuredClone(backend.klavesy);
			case 'uprav_klavesy':
				return uprav(args!.zmena as ZmenaKlaves);
			default:
				return null;
		}
	}
}));

const k = await import('./klavesy.svelte');
const p = await import('./pady.svelte');
// Jako okno: nejdřív poslouchat, pak načíst.
await k.prihlasKlavesy();
await k.nactiKlavesy();

function posli(udalost: string, data: unknown): void {
	const o = posluchaci.get(udalost);
	if (!o) throw new Error(`nikdo neposlouchá ${udalost}`);
	o(structuredClone(data));
}

let seqRezimu = 0;
let seqOznameni = 0;

function rezim(r: Rezim, cil: Cil | null = null): void {
	posli('rezim', { rezim: r, seq: ++seqRezimu, cil, hook_chyba: false });
}

function oznam(o: Record<string, unknown>): void {
	posli('oznameni', { seq: ++seqOznameni, mezera: false, ...o });
}

/** Doběhnou čekající sliby a události poslané `setTimeout(…, 0)`. */
const tik = () => new Promise<void>((hotovo) => setTimeout(hotovo, 5));

const WIN = { scan: 0x5b, e0: true };
const VIC_KLAVES = 5;

describe('odmítnutá klávesa a konec přiřazování', () => {
	test('„Win patří Windows" přežije zrušení přiřazování ztrátou popředí (Start)', () => {
		rezim('binding', { pad: 0, vstup: 'y' });
		expect(k.napovedaKarty(0, VIC_KLAVES)?.druh).toBe('prirazovani');
		oznam({ typ: 'odmitnuto', duvod: 'win', klavesa: WIN });
		expect(k.napovedaKarty(0, VIC_KLAVES)).toEqual({ text: 'Win patří Windows', druh: 'odmitnuto' });
		// Puštěná Win otevře Start, okno ztratí popředí a backend
		// přiřazování zruší: nejdřív nový režim, pak `zruseno okno`.
		rezim('disabled');
		expect(k.napovedaKarty(0, VIC_KLAVES)?.text).toBe('Win patří Windows');
		oznam({ typ: 'zruseno', duvod: 'okno' });
		expect(k.napovedaKarty(0, VIC_KLAVES)).toEqual({
			text: 'Win patří Windows',
			druh: 'odmitnuto',
			titulek: undefined,
			zpet: false
		});
		expect(k.cilPrirazeni()).toBeNull();
	});

	test('totéž s klávesou, která spustí aplikaci, i po 10 s', () => {
		rezim('binding', { pad: 0, vstup: 'x' });
		oznam({ typ: 'odmitnuto', duvod: 'nejde', klavesa: { scan: 0, e0: false } });
		rezim('disabled');
		oznam({ typ: 'zruseno', duvod: 'cas' });
		expect(k.napovedaKarty(0, VIC_KLAVES)?.text).toBe('Tuhle klávesu nejde použít');
	});

	test('Esc vrátí do klidu i bez dožívajícího odmítnutí', () => {
		rezim('binding', { pad: 0, vstup: 'y' });
		oznam({ typ: 'odmitnuto', duvod: 'zkratka', klavesa: { scan: 70, e0: false } });
		expect(k.napovedaKarty(0, VIC_KLAVES)?.text).toBe('Scroll Lock je pauza');
		rezim('disabled');
		oznam({ typ: 'zruseno', duvod: 'esc' });
		expect(k.napovedaKarty(0, VIC_KLAVES)).toBeNull();
	});

	test('klik jinam vrátí do klidu hned, i když backend ohlásí „okno"', () => {
		rezim('binding', { pad: 0, vstup: 'y' });
		oznam({ typ: 'odmitnuto', duvod: 'win', klavesa: WIN });
		k.zrusPrirazeni();
		expect(prikazy).toContain('zrus_prirazeni');
		rezim('disabled');
		oznam({ typ: 'zruseno', duvod: 'okno' });
		expect(k.napovedaKarty(0, VIC_KLAVES)).toBeNull();
	});

	test('uložená klávesa odmítnutí schová', () => {
		rezim('binding', { pad: 0, vstup: 'y' });
		oznam({ typ: 'odmitnuto', duvod: 'win', klavesa: WIN });
		rezim('disabled');
		oznam({ typ: 'ulozeno', pad: 0, vstup: 'y', klavesa: { scan: 0x13, e0: false }, odkud: null });
		expect(k.napovedaKarty(0, VIC_KLAVES)).toBeNull();
	});

	test('odmítnutí se během přiřazování neukáže jako obyčejná zpráva jiné karty', () => {
		rezim('binding', { pad: 1, vstup: 'a' });
		oznam({ typ: 'odmitnuto', duvod: 'win', klavesa: WIN });
		expect(k.napovedaKarty(1, VIC_KLAVES)?.druh).toBe('odmitnuto');
		expect(k.napovedaKarty(0, VIC_KLAVES)).toBeNull();
		rezim('disabled');
		oznam({ typ: 'zruseno', duvod: 'esc' });
	});
});

describe('karta ovladače při vyprázdnění poslední klávesy', () => {
	test('karta ovladače s klávesami z config.json zůstane i se „Zpět"', async () => {
		const stavy = () => p.pady.map((x) => x.state);
		// Po restartu: karta 2 je vidět jen díky klávese, přidaná není.
		expect(p.pridane).not.toContain(1);
		expect(viditelneKarty(k.klavesy.vazby, stavy(), p.pridane)).toEqual([0, 1]);

		await k.vyprazdni(1, KLAVESA_2.vstup);
		await tik();

		expect(k.klavesy.vazby.some((v) => v.pad === 1)).toBe(false);
		expect(viditelneKarty(k.klavesy.vazby, stavy(), p.pridane)).toEqual([0, 1]);
		expect(k.napovedaKarty(1, 0)).toEqual({
			text: `Vyprázdněno: ${KRATKE[KLAVESA_2.vstup]}`,
			druh: 'zprava',
			titulek: undefined,
			zpet: true
		});
		// Kartu pak schová jen 🗑 (po úspěšném odebrání ovladače).
		p.zapomenPridany(1);
		expect(viditelneKarty(k.klavesy.vazby, stavy(), p.pridane)).toEqual([0]);
	});
});

describe('„Nejdřív zapni ovladač"', () => {
	test('zmizí, jakmile ovladač zapne přepínač (hra i hned pauza)', () => {
		oznam({ typ: 'zapni_ovladac' });
		expect(k.napovedaKarty(0, VIC_KLAVES)?.text).toBe('Nejdřív zapni ovladač');
		rezim('capturing');
		expect(k.napovedaKarty(0, VIC_KLAVES)).toBeNull();
		rezim('disabled');

		oznam({ typ: 'zapni_ovladac' });
		rezim('paused');
		expect(k.napovedaKarty(0, VIC_KLAVES)).toBeNull();
		rezim('disabled');
	});

	test('jiná zpráva návratem do hry nezmizí („Win patří Windows" po přiřazování ze hry)', () => {
		rezim('binding', { pad: 0, vstup: 'y' });
		oznam({ typ: 'odmitnuto', duvod: 'win', klavesa: WIN });
		// Přiřazování začaté za hry se po ztrátě popředí vrací do hry.
		rezim('capturing');
		oznam({ typ: 'zruseno', duvod: 'okno' });
		expect(k.napovedaKarty(0, VIC_KLAVES)?.text).toBe('Win patří Windows');
		rezim('disabled');
		k.skryjZpravu();
	});
});
