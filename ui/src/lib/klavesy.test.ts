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
/**
 * Backend po restartu: ovladač 2 má jedinou klávesu z config.json (a tím
 * i kartu). `zastarale` = odpověď `klavesy`, která se zpozdila za
 * novější změnou karet.
 */
const backend = {
	klavesy: {
		...structuredClone(zlate),
		vazby: [
			...zlate.vazby.filter((v) => v.pad === 0),
			...zlate.vazby.filter((v) => v.pad === 1).slice(0, 1)
		] as Vazba[],
		zpet: false,
		karty: { rev: 0, pady: [0, 1], rozbalene: [0] },
		nastaveni: { rev: 0, zvuk: true, sdilene_klavesy: false }
	} as KlavesyInfo,
	zastarale: null as KlavesyInfo | null
};
const KLAVESA_2 = backend.klavesy.vazby.find((v) => v.pad === 1)!;

/** Jako backend: karta přibude (uloží se) a roste pořadí. */
function pridejKartu(pad: number): void {
	const k = backend.klavesy.karty;
	if (k.pady.includes(pad)) return;
	k.pady = [...k.pady, pad].sort((a, b) => a - b);
	k.rev++;
}

/** Revize, kterou okno poslalo s posledním „Zpět" (−1 = žádné). */
let revZpet = -1;

/** Jako hook: změna mapování zvedne revizi právě o 1, revize jde událostí. */
function zmenMapovani(): void {
	const k = backend.klavesy;
	k.rev++;
	k.zpet = true;
	const rev = k.rev;
	setTimeout(() => posli('klavesy-zmena', { rev }), 0);
}

function uprav(z: ZmenaKlaves): void {
	if (z.typ === 'zpet') {
		revZpet = z.rev;
		return;
	}
	if (z.typ !== 'vyprazdnit') throw new Error(`test neumí ${z.typ}`);
	const k = backend.klavesy;
	// Karta s poslední klávesou nezmizí (OQ 52) — backend ji uloží předem.
	pridejKartu(z.pad);
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
			case 'klavesy': {
				const z = backend.zastarale;
				backend.zastarale = null;
				return structuredClone(z ?? backend.klavesy);
			}
			case 'uprav_klavesy':
				return uprav(args!.zmena as ZmenaKlaves);
			case 'pridej_kartu': {
				const pad = args!.pad as number;
				const k = backend.klavesy.karty;
				const nova = !k.pady.includes(pad);
				pridejKartu(pad);
				// Nová karta přijde rozbalená (Fáze 7, Z2).
				if (nova) k.rozbalene = [...k.rozbalene, pad].sort((a, b) => a - b);
				return structuredClone(k);
			}
			case 'rozbal_kartu': {
				const k = backend.klavesy.karty;
				const pad = args!.pad as number;
				const pred = k.rozbalene.includes(pad);
				k.rozbalene = args!.rozbalena
					? [...new Set([...k.rozbalene, pad])].sort((a, b) => a - b)
					: k.rozbalene.filter((p) => p !== pad);
				if (pred !== args!.rozbalena) k.rev++;
				return structuredClone(k);
			}
			case 'nastav': {
				const n = backend.klavesy.nastaveni;
				n[args!.volba as 'zvuk' | 'sdilene_klavesy'] = !!args!.zapnuto;
				n.rev++;
				return structuredClone(n);
			}
			case 'odeber_ovladac': {
				const k = backend.klavesy;
				// Smazané klávesy jsou změna mapování (`VymazOvladac`).
				if (k.vazby.some((v) => v.pad === args!.pad)) zmenMapovani();
				k.vazby = k.vazby.filter((v) => v.pad !== args!.pad);
				k.karty.pady = k.karty.pady.filter((p) => p !== args!.pad);
				k.karty.rozbalene = k.karty.rozbalene.filter((p) => p !== args!.pad);
				k.karty.rev++;
				return structuredClone(k.karty);
			}
			default:
				return null;
		}
	}
}));

const k = await import('./klavesy.svelte');
const p = await import('./pady.svelte');
/** Karty, jak je okno ukáže. */
const videt = () =>
	viditelneKarty(
		k.klavesy.vazby,
		p.pady.map((x) => x.state),
		k.klavesy.karty
	);
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

function rezim(r: Rezim, cil: Cil | { zkratka: true } | null = null): void {
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

describe('sdílená klávesa (Fáze 7, Z4)', () => {
	test('uloženo se sdílením: „Sdíleno s ovladačem 2 · B" a „Zpět", ostatní čepička blikne', () => {
		// F patří X ovladače 1 i B ovladače 2 (zlatý soubor).
		const f = { scan: 0x21, e0: false };
		rezim('binding', { pad: 0, vstup: 'x' });
		rezim('disabled');
		oznam({ typ: 'ulozeno', pad: 0, vstup: 'x', klavesa: f, odkud: null, odkud_dalsi: 0, sdileno: 1 });
		expect(k.napovedaKarty(0, VIC_KLAVES)?.text).toBe('Sdíleno s ovladačem 2 · B');
		expect(k.efekty['1:b']?.druh).toBe('sdileno');
		k.skryjZpravu();
	});

	test('bez volby ze sdílené klávesy: „Přesunuto z … a 1 dalšího"', () => {
		oznam({
			typ: 'ulozeno',
			pad: 2,
			vstup: 'a',
			klavesa: { scan: 0x21, e0: false },
			odkud: { pad: 0, vstup: 'x' },
			odkud_dalsi: 1,
			sdileno: 0
		});
		expect(k.napovedaKarty(2, VIC_KLAVES)?.text).toBe('Přesunuto z ovladače 1 · X a 1 dalšího');
		k.skryjZpravu();
	});

	test('pátý vstup: „F už ovládá 4 vstupy" u přiřazované čepičky', () => {
		rezim('binding', { pad: 2, vstup: 'y' });
		oznam({ typ: 'odmitnuto', duvod: 'plno', klavesa: { scan: 0x21, e0: false } });
		expect(k.napovedaKarty(2, VIC_KLAVES)).toEqual({ text: 'F už ovládá 4 vstupy', druh: 'odmitnuto' });
		rezim('disabled');
		oznam({ typ: 'zruseno', duvod: 'esc' });
	});
});

describe('zkratka pozastavení z ⓘ (Fáze 7, Z6)', () => {
	test('přiřazování zkratky: čepičky vstupů nepulzují, odmítnutí jde do ⓘ, ne do karty', async () => {
		await k.priradZkratku();
		expect(prikazy).toContain('prirad_zkratku');
		expect(k.prirazujeZkratku()).toBe(true);
		expect(k.cilPrirazeni()).toBeNull();
		expect(k.prirazuje()).toBe(true);
		rezim('binding', { zkratka: true });
		expect(k.prirazujeZkratku()).toBe(true);
		expect(k.rezim.cil).toBeNull();
		expect(k.napovedaZkratky()?.druh).toBe('prirazovani');
		oznam({ typ: 'odmitnuto', duvod: 'nevhodna', klavesa: { scan: 0x0f, e0: false } });
		expect(k.napovedaZkratky()).toEqual({ text: 'Pauza jde jen na F1–F24 (ne F4), Scroll Lock nebo Pause', druh: 'odmitnuto' });
		expect(k.napovedaKarty(0, VIC_KLAVES)).toBeNull();
		// Namapovaná F-klávesa: kam patří, najde okno ve vazbách.
		oznam({ typ: 'odmitnuto', duvod: 'namapovana', klavesa: { scan: 0x21, e0: false } });
		expect(k.napovedaZkratky()?.text).toBe('F patří ovladači 1 · X');
		rezim('paused');
		oznam({ typ: 'ulozeno', zkratka: true, klavesa: { scan: 0x43, e0: false } });
		expect(k.prirazujeZkratku()).toBe(false);
		expect(k.napovedaZkratky()).toEqual({ text: 'Uloženo', druh: 'zprava' });
		k.skryjZpravuZkratky();
		rezim('disabled');
	});

	test('Esc zkratku zruší jako přiřazování vstupu', async () => {
		await k.priradZkratku();
		rezim('binding', { zkratka: true });
		prikazy.length = 0;
		k.zrusPrirazeni();
		expect(prikazy).toContain('zrus_prirazeni');
		rezim('disabled');
		expect(k.prirazuje()).toBe(false);
		oznam({ typ: 'zruseno', duvod: 'okno' });
		expect(k.napovedaZkratky()).toBeNull();
	});
});

describe('volby z ⓘ (Fáze 7, Z6)', () => {
	test('přepnutí platí hned a backend ho potvrdí; starší událost ho nepřebije', async () => {
		expect(k.nastaveni.zvuk).toBe(true);
		const r = k.nastav('zvuk', false);
		expect(k.nastaveni.zvuk).toBe(false);
		expect(await r).toBe('');
		expect(prikazy).toContain('nastav');
		const rev = k.nastaveni.rev;
		posli('nastaveni', { rev: rev - 1, zvuk: true, sdilene_klavesy: false });
		expect(k.nastaveni.zvuk).toBe(false);
		// „✓ Zvuk" z nabídky ikony: novější událost.
		posli('nastaveni', { rev: rev + 1, zvuk: true, sdilene_klavesy: false });
		expect(k.nastaveni.zvuk).toBe(true);
	});
});

describe('rozbalení karet (Fáze 7, Z2)', () => {
	test('každá karta zvlášť, okno přepne hned a starší seznam to nevrátí', async () => {
		expect(k.klavesy.rozbalene).toEqual([0]);
		backend.zastarale = structuredClone(backend.klavesy);
		const r = k.rozbalKartu(1, true);
		expect(k.klavesy.rozbalene).toEqual([0, 1]);
		await k.nactiKlavesy();
		expect(k.klavesy.rozbalene).toEqual([0, 1]);
		await r;
		expect(prikazy).toContain('rozbal_kartu');
		expect(k.klavesy.rozbalene).toEqual([0, 1]);
		// Sbalit jde i ovladač 1.
		await k.rozbalKartu(0, false);
		expect(k.klavesy.rozbalene).toEqual([1]);
		await k.nactiKlavesy();
		expect(k.klavesy.rozbalene).toEqual([1]);
		await k.rozbalKartu(0, true);
		await k.rozbalKartu(1, false);
		expect(k.klavesy.rozbalene).toEqual([0]);
	});

	test('sbalení karty s přiřazovanou čepičkou přiřazování zruší, jiné karty ne', async () => {
		await k.rozbalKartu(1, true);
		rezim('binding', { pad: 1, vstup: 'a' });
		prikazy.length = 0;
		await k.rozbalKartu(0, false);
		expect(prikazy).not.toContain('zrus_prirazeni');
		await k.rozbalKartu(1, false);
		expect(prikazy).toContain('zrus_prirazeni');
		rezim('disabled');
		oznam({ typ: 'zruseno', duvod: 'okno' });
		await k.rozbalKartu(0, true);
	});
});

describe('karty ovladačů (ukládají se, OQ 52)', () => {
	test('karta ovladače s klávesami z config.json zůstane i po vyprázdnění, se „Zpět"', async () => {
		// Po restartu: karta 2 uložená (má klávesu).
		expect(k.klavesy.karty).toEqual([0, 1]);
		expect(videt()).toEqual([0, 1]);

		await k.vyprazdni(1, KLAVESA_2.vstup);
		await tik();

		expect(k.klavesy.vazby.some((v) => v.pad === 1)).toBe(false);
		expect(videt()).toEqual([0, 1]);
		expect(k.napovedaKarty(1, 0)).toEqual({
			text: `Vyprázdněno: ${KRATKE[KLAVESA_2.vstup]}`,
			druh: 'zprava',
			titulek: undefined,
			zpet: true
		});
		k.skryjZpravu();
	});

	test('„+ Ovladač" kartu ukáže hned a starší odpověď ji neschová', async () => {
		// Načtení kláves, které se zpozdilo za přidáním karty.
		backend.zastarale = structuredClone(backend.klavesy);
		const pridani = k.pridejKartu(3);
		expect(videt()).toEqual([0, 1, 3]);
		// Nová karta vzniká rozbalená bez přechodu — do zorného pole ji
		// posune karta sama podle příznaku (revize; shodí ho po posunu).
		expect(k.novaKarta.pad).toBe(3);
		await pridani;
		expect(prikazy).toContain('pridej_kartu');
		await k.nactiKlavesy();
		expect(k.klavesy.karty).toEqual([0, 1, 3]);
		expect(videt()).toEqual([0, 1, 3]);
	});

	test('kartu schová jen 🗑 (backend ji odebere i z uložených)', async () => {
		expect(await k.odeberOvladac(3)).toBe(true);
		expect(videt()).toEqual([0, 1]);
		expect(await k.odeberOvladac(1)).toBe(true);
		expect(videt()).toEqual([0]);
		// Po „restartu" (novém načtení) se nevrátí.
		await k.nactiKlavesy();
		expect(videt()).toEqual([0]);
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

describe('„Zpět" vrací jen změnu, o které mluví zpráva (revize)', () => {
	const F = { scan: 0x21, e0: false };

	/** Přesun F z X na Y ovladače 1 (backend zvedne revizi o 1). Vrací revizi před ním. */
	async function presun(): Promise<number> {
		await k.prirad(0, 'y', false);
		rezim('binding', { pad: 0, vstup: 'y' });
		const pred = k.klavesy.rev;
		zmenMapovani();
		await tik();
		rezim('disabled');
		oznam({ typ: 'ulozeno', pad: 0, vstup: 'y', klavesa: F, odkud: { pad: 0, vstup: 'x' }, odkud_dalsi: 0, sdileno: 0 });
		expect(k.klavesy.rev).toBe(pred + 1);
		expect(k.napovedaKarty(0, VIC_KLAVES)?.zpet).toBe(true);
		return pred;
	}

	test('Zpět pošle revizi hned po přesunu, ne tu, kterou okno zná teď', async () => {
		const pred = await presun();
		// Klik na „Zpět" ve chvíli, kdy okno už zná novější změnu (tlačítko
		// ještě nezmizelo): backend musí dostat revizi přesunu a odmítnout.
		zmenMapovani();
		await tik();
		revZpet = -1;
		await k.zpet();
		expect(revZpet).toBe(pred + 1);
	});

	test('přesun → jiná změna mapování: Zpět se nenabídne (vrátil by tu druhou)', async () => {
		await presun();
		zmenMapovani();
		await tik();
		const n = k.napovedaKarty(0, VIC_KLAVES);
		expect(n?.text).toBe('Přesunuto z X');
		expect(n?.zpet).toBe(false);
		k.skryjZpravu();
	});

	test('přesun → zkratka pozastavení: Zpět se nenabídne', async () => {
		await presun();
		await k.priradZkratku();
		rezim('binding', { zkratka: true });
		zmenMapovani();
		await tik();
		rezim('disabled');
		oznam({ typ: 'ulozeno', zkratka: true, klavesa: { scan: 0x43, e0: false } });
		expect(k.napovedaKarty(0, VIC_KLAVES)?.zpet ?? false).toBe(false);
		expect(k.lzeVratit()).toBe(false);
		k.skryjZpravuZkratky();
	});

	test('přesun → 🗑 jiného ovladače s klávesami: Zpět se nenabídne', async () => {
		backend.klavesy.vazby = [...backend.klavesy.vazby, { ...structuredClone(KLAVESA_2), pad: 2 }];
		await k.pridejKartu(2);
		await presun();
		expect(await k.odeberOvladac(2)).toBe(true);
		await tik();
		expect(k.napovedaKarty(0, VIC_KLAVES)?.zpet ?? false).toBe(false);
		expect(k.lzeVratit()).toBe(false);
	});
});
