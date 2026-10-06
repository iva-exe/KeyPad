// Testy čistých výpočtů okna a tvaru zlatých souborů smlouvy (bun test).
//
// Zlaté soubory v testdata/ serializuje i Rust (B4) ze stejných vzorových
// hodnot: když se tvar změní na jedné straně, spadne test na té druhé.
import { describe, expect, test } from 'bun:test';
import klavesyJson from './testdata/klavesy.json';
import nastaveniJson from './testdata/nastaveni.json';
import oznameniJson from './testdata/oznameni.json';
import rezimJson from './testdata/rezim.json';
import rezimZkratkaJson from './testdata/rezim_zkratka.json';
import ziveJson from './testdata/zive.json';
import type { KlavesyInfo, PadStav, Vazba } from './smlouva';
import {
	bublinaChyb,
	bublinaVstupu,
	cileKlaves,
	dalsiCile,
	dalsiKarta,
	dekoduj,
	indexy,
	jeAlt,
	jedinyVstup,
	klicKlavesy,
	KRATKE,
	maVsechnyKlavesy,
	NAZVY,
	pocetKlaves,
	PODKLADY,
	POLOHY,
	posunHlavicky,
	procNeodebrat,
	RADKU,
	seskup,
	SLOUPCU,
	sviti,
	textCile,
	textOdmitnuti,
	textPresunu,
	textSdileni,
	varovaniZkratky,
	viditelneKarty,
	VSTUPY
} from './vstupy';

const klavesy = klavesyJson as unknown as KlavesyInfo;

/** Klíče objektu seřazené — smlouva nesmí mít pole navíc ani chybějící. */
function klice(o: unknown): string[] {
	expect(typeof o).toBe('object');
	expect(o).not.toBeNull();
	return Object.keys(o as object).sort();
}

function jeCislo(v: unknown): void {
	expect(typeof v).toBe('number');
	expect(Number.isInteger(v)).toBe(true);
}

function jeKlavesa(k: unknown, sNazvy: boolean): void {
	expect(klice(k)).toEqual(sNazvy ? ['e0', 'kratky', 'nazev', 'scan'] : ['e0', 'scan']);
	const o = k as Record<string, unknown>;
	jeCislo(o.scan);
	expect(typeof o.e0).toBe('boolean');
	if (sNazvy) {
		expect(typeof o.nazev).toBe('string');
		expect(typeof o.kratky).toBe('string');
	}
}

/** Volby z ⓘ (Fáze 7): pořadí změny a dvě volby. */
function jeNastaveni(n: unknown): void {
	expect(klice(n)).toEqual(['rev', 'sdilene_klavesy', 'zvuk']);
	const o = n as Record<string, unknown>;
	jeCislo(o.rev);
	expect(typeof o.zvuk).toBe('boolean');
	expect(typeof o.sdilene_klavesy).toBe('boolean');
}

function jeCil(c: unknown): void {
	expect(klice(c)).toEqual(['pad', 'vstup']);
	const o = c as Record<string, unknown>;
	jeCislo(o.pad);
	expect(VSTUPY as readonly unknown[]).toContain(o.vstup);
}

describe('zlaté soubory smlouvy', () => {
	test('klavesy: pořadí vstupů je pořadí bitů jádra', () => {
		expect(klavesy.vstupy).toEqual([...VSTUPY]);
	});

	test('klavesy: tvar', () => {
		expect(klice(klavesyJson)).toEqual([
			'chyby',
			'karty',
			'konfigurace',
			'nastaveni',
			'rev',
			'vazby',
			'vstupy',
			'zaloha',
			'zkratka',
			'zkratka_mimo',
			'zpet'
		]);
		expect(typeof klavesy.zkratka_mimo).toBe('boolean');
		jeCislo(klavesy.rev);
		expect(klice(klavesy.karty)).toEqual(['pady', 'rev', 'rozbalene']);
		jeCislo(klavesy.karty.rev);
		expect(klavesy.karty.pady[0]).toBe(0);
		for (const p of klavesy.karty.pady) jeCislo(p);
		expect(klavesy.karty.pady).toEqual([...klavesy.karty.pady].sort((a, b) => a - b));
		// Rozbalené (Fáze 7, OQ 70): vzestupně a jen z karet v okně.
		for (const p of klavesy.karty.rozbalene) expect(klavesy.karty.pady).toContain(p);
		expect(klavesy.karty.rozbalene).toEqual([...klavesy.karty.rozbalene].sort((a, b) => a - b));
		jeNastaveni(klavesy.nastaveni);
		jeKlavesa(klavesy.zkratka, true);
		expect(typeof klavesy.zpet).toBe('boolean');
		expect(['ok', 'obnovena', 'novejsi', 'necitelna', 'neulozena']).toContain(klavesy.konfigurace);
		expect(klavesy.zaloha === null || typeof klavesy.zaloha === 'string').toBe(true);
		expect(Array.isArray(klavesy.chyby)).toBe(true);
		for (const c of klavesy.chyby) expect(typeof c).toBe('string');
		expect(klavesy.vazby.length).toBeLessThan(257);
		for (const v of klavesy.vazby) {
			expect(klice(v)).toEqual(['klavesa', 'pad', 'vstup']);
			jeCil({ pad: v.pad, vstup: v.vstup });
			jeKlavesa(v.klavesa, true);
		}
	});

	test('klavesy: vazby v pořadí Mapping::bindings(), sdílená klávesa víckrát (nejvýš 4×)', () => {
		const index = (v: Vazba) => (v.klavesa.e0 ? 0x80 : 0) + v.klavesa.scan;
		// Klávesa, pak ovladač, pak pořadí vstupů (bity živého stavu) — tatáž
		// dvojice nikdy dvakrát (Fáze 7: sdílená klávesa = víc vstupů).
		const klic = (v: Vazba) => index(v) * 1000 + v.pad * 100 + klavesy.vstupy.indexOf(v.vstup);
		const klice2 = klavesy.vazby.map(klic);
		expect(klice2).toEqual([...klice2].sort((a, b) => a - b));
		expect(new Set(klice2).size).toBe(klice2.length);
		const poradi = klavesy.vazby.map(index);
		const kolikrat = new Map<number, number>();
		for (const i of poradi) kolikrat.set(i, (kolikrat.get(i) ?? 0) + 1);
		expect(Math.max(...kolikrat.values())).toBeLessThan(5);
		// Zlatý soubor má aspoň jednu sdílenou klávesu (F: ovladač 1 · X, ovladač 2 · B).
		expect([...kolikrat.values()].some((n) => n > 1)).toBe(true);
		// Zkratka ani Win nikdy nejsou vazbou (jádro je nedovolí).
		expect(poradi).not.toContain(index({ ...klavesy.vazby[0]!, klavesa: klavesy.zkratka }));
		expect(poradi).not.toContain(0x80 + 0x5b);
		expect(poradi).not.toContain(0x80 + 0x5c);
	});

	test('rezim: tvar', () => {
		expect(klice(rezimJson)).toEqual(['cil', 'hook_chyba', 'rezim', 'seq']);
		expect(['disabled', 'paused', 'capturing', 'binding', 'no_hook']).toContain(rezimJson.rezim);
		jeCislo(rezimJson.seq);
		jeCil(rezimJson.cil);
		expect(typeof rezimJson.hook_chyba).toBe('boolean');
	});

	test('rezim: přiřazuje se zkratka pozastavení (Fáze 7, Z6)', () => {
		expect(klice(rezimZkratkaJson)).toEqual(['cil', 'hook_chyba', 'rezim', 'seq']);
		expect(rezimZkratkaJson.rezim).toBe('binding');
		expect(rezimZkratkaJson.cil).toEqual({ zkratka: true });
	});

	test('zive: tvar a 4 ovladače', () => {
		expect(klice(ziveJson)).toEqual(['pady', 'seq']);
		jeCislo(ziveJson.seq);
		expect(ziveJson.pady).toHaveLength(4);
		for (const p of ziveJson.pady) {
			expect(klice(p)).toEqual(['drzi', 'hra', 'l', 'p']);
			jeCislo(p.drzi);
			jeCislo(p.hra);
			for (const osa of [p.l, p.p]) {
				expect(osa).toHaveLength(2);
				for (const x of osa) expect([-1, 0, 1]).toContain(x);
			}
			// Jen 24 bitů vstupů.
			expect(p.drzi >>> 24).toBe(0);
			expect(p.hra >>> 24).toBe(0);
		}
	});

	test('oznameni: všechny varianty a nic navíc', () => {
		const typy = new Set<string>();
		const duvody = new Set<string>();
		for (const o of oznameniJson as Record<string, unknown>[]) {
			jeCislo(o.seq);
			expect(o.seq as number).toBeLessThan(65536);
			expect(typeof o.mezera).toBe('boolean');
			typy.add(o.typ as string);
			switch (o.typ) {
				case 'ulozeno':
					// Nová zkratka pozastavení (Z6): bez ovladače, vstupu a přesunu.
					if (o.zkratka !== undefined) {
						expect(klice(o)).toEqual(['klavesa', 'mezera', 'seq', 'typ', 'zkratka']);
						expect(o.zkratka).toBe(true);
						jeKlavesa(o.klavesa, false);
						typy.add('ulozeno-zkratka');
						break;
					}
					expect(klice(o)).toEqual([
						'klavesa',
						'mezera',
						'odkud',
						'odkud_dalsi',
						'pad',
						'sdileno',
						'seq',
						'typ',
						'vstup'
					]);
					jeCil({ pad: o.pad, vstup: o.vstup });
					jeKlavesa(o.klavesa, false);
					if (o.odkud !== null) jeCil(o.odkud);
					// Fáze 7: kolik dalších vstupů o klávesu přišlo (jen s `odkud`)
					// a kolika dalším klávesa patří — obojí 0–3.
					for (const n of [o.odkud_dalsi, o.sdileno]) {
						jeCislo(n);
						expect([0, 1, 2, 3]).toContain(n as number);
					}
					if (o.odkud === null) expect(o.odkud_dalsi).toBe(0);
					break;
				case 'odmitnuto':
					expect(klice(o)).toEqual(['duvod', 'klavesa', 'mezera', 'seq', 'typ']);
					expect(['zkratka', 'win', 'nejde', 'plno', 'namapovana', 'nevhodna']).toContain(o.duvod);
					duvody.add(o.duvod as string);
					jeKlavesa(o.klavesa, false);
					break;
				case 'zruseno':
					expect(klice(o)).toEqual(['duvod', 'mezera', 'seq', 'typ']);
					expect(['esc', 'cas', 'okno', 'vynuceno']).toContain(o.duvod);
					break;
				case 'zapni_ovladac':
					expect(klice(o)).toEqual(['mezera', 'seq', 'typ']);
					break;
				default:
					throw new Error(`neznámý typ oznámení ${String(o.typ)}`);
			}
		}
		expect([...typy].sort()).toEqual(['odmitnuto', 'ulozeno', 'ulozeno-zkratka', 'zapni_ovladac', 'zruseno']);
		// Zlatý soubor má každý důvod odmítnutí, i přiřazování zkratky.
		expect([...duvody].sort()).toEqual(['namapovana', 'nejde', 'nevhodna', 'plno', 'win', 'zkratka']);
	});

	test('nastaveni: tvar (Fáze 7)', () => {
		jeNastaveni(nastaveniJson);
	});
});

describe('schéma', () => {
	test('každý vstup má polohu, název i krátký název', () => {
		expect(Object.keys(POLOHY).sort()).toEqual([...VSTUPY].sort());
		expect(Object.keys(NAZVY).sort()).toEqual([...VSTUPY].sort());
		expect(Object.keys(KRATKE).sort()).toEqual([...VSTUPY].sort());
	});

	test('polohy jsou v mřížce a nepřekrývají se', () => {
		const obsazeno = new Set<string>();
		for (const v of VSTUPY) {
			const { r, s } = POLOHY[v];
			expect(r >= 1 && r <= RADKU && s >= 1 && s <= SLOUPCU).toBe(true);
			const kde = `${r}:${s}`;
			expect(obsazeno.has(kde)).toBe(false);
			obsazeno.add(kde);
		}
		for (const p of PODKLADY) {
			const [r1, s1, r2, s2] = p.oblast;
			expect(r1 >= 1 && s1 >= 1 && r2 <= RADKU + 1 && s2 <= SLOUPCU + 1).toBe(true);
		}
	});

	test('popisky mají jen tlačítka; hlavička je L3 a R3', () => {
		const sPopiskem = VSTUPY.filter((v) => POLOHY[v].popisek).sort();
		expect(sPopiskem).toEqual(['a', 'b', 'back', 'lb', 'lt', 'rb', 'rt', 'start', 'x', 'y']);
		expect(VSTUPY.filter((v) => POLOHY[v].hlavicka)).toEqual(['l3', 'r3']);
	});

	test('hlavička páčky je uprostřed kříže svých směrů', () => {
		for (const [h, nahoru, dolu, vlevo, vpravo] of [
			['l3', 'ls_up', 'ls_down', 'ls_left', 'ls_right'],
			['r3', 'rs_up', 'rs_down', 'rs_left', 'rs_right']
		] as const) {
			const s = POLOHY[h];
			expect(POLOHY[nahoru]).toEqual({ r: s.r - 1, s: s.s });
			expect(POLOHY[dolu]).toEqual({ r: s.r + 1, s: s.s });
			expect(POLOHY[vlevo]).toEqual({ r: s.r, s: s.s - 1 });
			expect(POLOHY[vpravo]).toEqual({ r: s.r, s: s.s + 1 });
		}
	});
});

describe('svícení', () => {
	test('hra má přednost před náhledem', () => {
		expect(sviti(0b11, 0b01, 0)).toBe('hra');
		expect(sviti(0b11, 0b01, 1)).toBe('nahled');
		expect(sviti(0b11, 0b01, 2)).toBeNull();
	});

	test('hra bez držené klávesy svítí (Vyzkoušet)', () => {
		expect(sviti(0, 0b100, 2)).toBe('hra');
	});

	test('horní bity a neplatný index', () => {
		const rt = 1 << 23;
		expect(sviti(rt, 0, 23)).toBe('nahled');
		expect(sviti(rt, rt, 23)).toBe('hra');
		expect(sviti(-1, 0, 31)).toBe('nahled');
		expect(sviti(0xffffff, 0xffffff, undefined)).toBeNull();
		expect(sviti(0xffffff, 0xffffff, -1)).toBeNull();
		expect(sviti(0xffffff, 0xffffff, 32)).toBeNull();
	});

	test('A+D ze zlatého souboru: vítěz plně, poražený obrysem', () => {
		const i = indexy(klavesy.vstupy);
		const p = ziveJson.pady[0]!;
		expect(sviti(p.drzi, p.hra, i.ls_right)).toBe('hra');
		expect(sviti(p.drzi, p.hra, i.ls_left)).toBe('nahled');
		expect(sviti(p.drzi, p.hra, i.ls_up)).toBeNull();
		expect(sviti(p.drzi, p.hra, i.rt)).toBe('hra');
	});
});

describe('dekódování bitů', () => {
	test('zlatý živý stav', () => {
		const [p0, p1, p2] = ziveJson.pady;
		expect(dekoduj(p0!.drzi, klavesy.vstupy)).toEqual(['ls_left', 'ls_right', 'rs_up', 'a', 'rt']);
		expect(dekoduj(p0!.hra, klavesy.vstupy)).toEqual(['ls_right', 'rs_up', 'a', 'rt']);
		expect(dekoduj(p1!.drzi, klavesy.vstupy)).toEqual(['ls_up']);
		expect(dekoduj(p2!.drzi, klavesy.vstupy)).toEqual([]);
	});

	test('podle pořadí od backendu, ne podle vlastní kopie', () => {
		const obracene = [...VSTUPY].reverse();
		expect(dekoduj(1, obracene)).toEqual(['rt']);
		expect(indexy(obracene).rt).toBe(0);
		expect(indexy(obracene).ls_up).toBe(23);
	});

	test('neznámý kód se přeskočí', () => {
		expect(dekoduj(0b111, ['ls_up', 'guide', 'a'])).toEqual(['ls_up', 'a']);
		expect(indexy(['ls_up', 'guide', 'a'])).toEqual({ ls_up: 0, a: 2 });
	});

	test('posun hlavičky: rovně 6 px, šikmo stejně daleko', () => {
		expect(posunHlavicky(0, 0)).toEqual([0, 0]);
		expect(posunHlavicky(1, 0)).toEqual([6, 0]);
		expect(posunHlavicky(-1, 0)).toEqual([-6, 0]);
		// +y = nahoru (XInput), na obrazovce záporné y.
		expect(posunHlavicky(0, 1)).toEqual([0, -6]);
		expect(posunHlavicky(0, -1)).toEqual([0, 6]);
		const [x, y] = posunHlavicky(1, 1);
		expect(x).toBeCloseTo(4.243, 3);
		expect(y).toBeCloseTo(-4.243, 3);
		expect(Math.hypot(x, y)).toBeCloseTo(6, 6);
		expect(Object.is(posunHlavicky(0, 0)[0], -0)).toBe(false);
	});
});

describe('karty', () => {
	const vypnute = ['off', 'off', 'off', 'off'];

	test('ovladač 1 vždy', () => {
		expect(viditelneKarty([], vypnute, [])).toEqual([0]);
	});

	test('s klávesami, zapnutý, uložená karta', () => {
		expect(viditelneKarty([{ pad: 2 }], vypnute, [])).toEqual([0, 2]);
		expect(viditelneKarty([], ['off', 'off', 'off', 'on'], [])).toEqual([0, 3]);
		expect(viditelneKarty([], ['off', 'connecting', 'off', 'off'], [])).toEqual([0, 1]);
		expect(viditelneKarty([], ['off', 'off', 'error', 'off'], [])).toEqual([0, 2]);
		expect(viditelneKarty([], vypnute, [1])).toEqual([0, 1]);
		expect(viditelneKarty([{ pad: 3 }, { pad: 3 }], vypnute, [3, 1])).toEqual([0, 1, 3]);
	});

	test('stavy bez běžícího ovladače kartu neukážou', () => {
		expect(viditelneKarty([], ['bus_missing', 'bus_missing', 'bus_not_running', 'off'], [])).toEqual([0]);
	});

	test('mimo rozsah se ignoruje', () => {
		expect(viditelneKarty([{ pad: 7 }], vypnute, [-1, 4])).toEqual([0]);
	});

	test('zlatý soubor: ovladače 1 a 2 s klávesami, 4 jen s uloženou kartou (OQ 52)', () => {
		expect(viditelneKarty(klavesy.vazby, vypnute, [])).toEqual([0, 1]);
		expect(viditelneKarty(klavesy.vazby, vypnute, klavesy.karty.pady)).toEqual([0, 1, 3]);
	});

	test('další karta', () => {
		expect(dalsiKarta([0])).toBe(1);
		expect(dalsiKarta([0, 2])).toBe(1);
		expect(dalsiKarta([0, 1, 2])).toBe(3);
		expect(dalsiKarta([0, 1, 2, 3])).toBeNull();
	});
});

describe('klávesy ovladače', () => {
	test('seskupení podle vstupu, pořadí od backendu', () => {
		const s = seskup(klavesy.vazby, 0);
		expect(s.lb?.map((k) => k.nazev)).toEqual(['Q', 'Alt']);
		expect(s.ls_up?.map((k) => k.kratky)).toEqual(['W']);
		expect(seskup(klavesy.vazby, 1).ls_up?.map((k) => k.nazev)).toEqual(['Num 8']);
		expect(seskup(klavesy.vazby, 2)).toEqual({});
	});

	test('Alt', () => {
		expect(jeAlt({ scan: 0x38 })).toBe(true);
		expect(jeAlt({ scan: 0x39 })).toBe(false);
	});

	test('poslední vstup celého mapování', () => {
		const k = klavesy.vazby[0]!.klavesa;
		const jen: Vazba[] = [
			{ pad: 1, vstup: 'a', klavesa: k },
			{ pad: 1, vstup: 'a', klavesa: { ...k, scan: 0x10 } }
		];
		expect(jedinyVstup(jen)).toEqual({ pad: 1, vstup: 'a' });
		expect(jedinyVstup([...jen, { pad: 1, vstup: 'b', klavesa: k }])).toBeNull();
		expect(jedinyVstup([...jen, { pad: 0, vstup: 'a', klavesa: k }])).toBeNull();
		expect(jedinyVstup([])).toBeNull();
		expect(jedinyVstup(klavesy.vazby)).toBeNull();
	});

	test('počty a „má všechny klávesy"', () => {
		expect(pocetKlaves(klavesy.vazby, 0)).toBe(25);
		// Numerická klávesnice a F sdílená s ovladačem 1 (Fáze 7).
		expect(pocetKlaves(klavesy.vazby, 1)).toBe(6);
		expect(maVsechnyKlavesy(klavesy.vazby, 0)).toBe(false);
		expect(maVsechnyKlavesy(klavesy.vazby.filter((v) => v.pad === 1), 1)).toBe(true);
		expect(maVsechnyKlavesy([], 1)).toBe(false);
	});

	// Backend odebere jen `off` (spec B4) — 🗑 nesmí být aktivní v žádném
	// jiném stavu, ani bez ViGEmBus, kde vypnout nejde (revize B5).
	test('🗑 jen u vypnutého ovladače, s radou podle stavu', () => {
		const stavy: Record<PadStav, string> = {
			off: '',
			connecting: 'Nejdřív ho vypni',
			on: 'Nejdřív ho vypni',
			error: 'Nejdřív Zkusit znovu',
			bus_missing: 'Nejdřív musí běžet ViGEmBus',
			bus_not_running: 'Nejdřív musí běžet ViGEmBus'
		};
		for (const [stav, rada] of Object.entries(stavy) as [PadStav, string][]) {
			expect(procNeodebrat(stav, false)).toBe(rada);
			// Všechny klávesy mapování: u vypnutého brání ony, jinak stav.
			expect(procNeodebrat(stav, true)).toBe(stav === 'off' ? 'Nejdřív dej klávesy jinému ovladači' : rada);
		}
	});
});

describe('texty', () => {
	test('bublina čepičky', () => {
		const s = seskup(klavesy.vazby, 0);
		expect(bublinaVstupu('ls_up', s.ls_up ?? [])).toBe('Levá páčka ↑ · W');
		expect(bublinaVstupu('lb', s.lb ?? [])).toBe('LB · Q · také Alt\nPři hraní nepůjde Alt+Tab');
		expect(bublinaVstupu('x', [])).toBe('X · bez klávesy');
	});

	test('odmítnutí', () => {
		expect(textOdmitnuti('zkratka', 'Scroll Lock')).toBe('Scroll Lock je pauza');
		expect(textOdmitnuti('zkratka')).toBe('Zkratka je pauza');
		expect(textOdmitnuti('win')).toBe('Win patří Windows');
		expect(textOdmitnuti('nejde')).toBe('Tuhle klávesu nejde použít');
		expect(textOdmitnuti('plno')).toBe('Klávesa už ovládá 4 vstupy');
		expect(textOdmitnuti('plno', { klavesa: 'F' })).toBe('F už ovládá 4 vstupy');
		// Přiřazování zkratky pozastavení (Z6).
		expect(textOdmitnuti('namapovana', { klavesa: 'F5', kam: { pad: 0, vstup: 'a' } })).toBe('F5 patří ovladači 1 · A');
		expect(textOdmitnuti('namapovana', { klavesa: 'F5' })).toBe('F5 už ovládá vstup');
		expect(textOdmitnuti('nevhodna')).toBe('Pauza jde jen na F1–F24 (ne F4), Scroll Lock nebo Pause');
	});

	test('sdílená klávesa: kam ještě patří, bublina a oznámení (Fáze 7, Z4)', () => {
		// Zlatý soubor: F je X ovladače 1 i B ovladače 2.
		const cile = cileKlaves(klavesy.vazby);
		const f = { scan: 0x21, e0: false };
		expect(cile.get(klicKlavesy(f))).toEqual([
			{ pad: 0, vstup: 'x' },
			{ pad: 1, vstup: 'b' }
		]);
		expect(dalsiCile(cile, f, 0, 'x')).toEqual([{ pad: 1, vstup: 'b' }]);
		expect(dalsiCile(cile, f, 1, 'b')).toEqual([{ pad: 0, vstup: 'x' }]);
		expect(dalsiCile(cile, { scan: 0x11, e0: false }, 0, 'ls_up')).toEqual([]);
		const s = seskup(klavesy.vazby, 0);
		expect(bublinaVstupu('x', s.x ?? [], (k) => dalsiCile(cile, k, 0, 'x'))).toBe(
			'X · F\nF — také: Ovladač 2 · B'
		);
		// Nesdílená čepička bez řádku navíc.
		expect(bublinaVstupu('ls_up', s.ls_up ?? [], (k) => dalsiCile(cile, k, 0, 'ls_up'))).toBe('Levá páčka ↑ · W');
		expect(textCile({ pad: 0, vstup: 'ls_up' })).toBe('Ovladač 1 · Levá páčka ↑');
		expect(textSdileni([{ pad: 1, vstup: 'a' }], 0)).toBe('Sdíleno s ovladačem 2 · A');
		expect(textSdileni([{ pad: 0, vstup: 'lb' }], 0)).toBe('Sdíleno s LB');
		expect(textSdileni([{ pad: 1, vstup: 'a' }, { pad: 0, vstup: 'lb' }], 0)).toBe('Sdíleno se 2 vstupy');
	});

	test('varování u zkratky pozastavení (Z6)', () => {
		expect(varovaniZkratky({ scan: 0x46, e0: false }, false)).toBe('');
		expect(varovaniZkratky({ scan: 0x58, e0: false }, false)).toContain('F12 ve Steamu');
		expect(varovaniZkratky({ scan: 0x0f, e0: false }, true)).toBe('Tuhle klávesu Windows nedostanou, dokud je zapnutý ovladač');
		// F4 ručně v config.json (z okna nejde, OQ 69): konkrétně, co nepůjde.
		expect(varovaniZkratky({ scan: 0x3e, e0: false }, true)).toBe('Alt+F4 nezavře okno, dokud je zapnutý ovladač');
		expect(varovaniZkratky(null, true)).toBe('');
	});

	test('přesun ze stejného a z jiného ovladače', () => {
		expect(textPresunu({ pad: 0, vstup: 'lb' }, 0)).toBe('Přesunuto z LB');
		expect(textPresunu({ pad: 1, vstup: 'lb' }, 0)).toBe('Přesunuto z ovladače 2 · LB');
		expect(textPresunu({ pad: 0, vstup: 'ls_up' }, 1)).toBe('Přesunuto z ovladače 1 · levá ↑');
		// Ze sdílené klávesy bez volby (Fáze 7): o klávesu přišlo víc vstupů.
		expect(textPresunu({ pad: 1, vstup: 'lb' }, 0, 1)).toBe('Přesunuto z ovladače 2 · LB a 1 dalšího');
		expect(textPresunu({ pad: 0, vstup: 'lb' }, 0, 3)).toBe('Přesunuto z LB a 3 dalších');
	});

	test('chyby konfigurace v bublině pruhu (spec 1.7)', () => {
		expect(bublinaChyb([])).toBe('Podrobnosti jsou v logu.');
		expect(bublinaChyb(['řádek 1: expected value', 'vazba č. 2: ovladač 5 neexistuje (jen 1–4)'])).toBe(
			[
				'Co je v souboru špatně:',
				'· řádek 1: expected value',
				'· vazba č. 2: ovladač 5 neexistuje (jen 1–4)',
				'Podrobnosti jsou v logu.'
			].join('\n')
		);
	});
});
