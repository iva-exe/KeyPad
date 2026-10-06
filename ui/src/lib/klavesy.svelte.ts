// Klávesy ovladačů, režim zachytávání a přiřazování (Fáze 6).
//
// Zdrojem pravdy jsou režim a mapování v backendu. Okno je načítá
// příkazy `rezim` a `klavesy` a změny dostává událostmi `rezim`
// a `klavesy-zmena` (ta nese jen revizi — názvy kláves se pak čtou
// znovu, aby seděly s rozložením klávesnice). Událost `oznameni` jen
// říká, co se stalo a proč (uloženo, odmítnuto, zrušeno): podle ní okno
// blikne, zatřese a napíše jednu větu. Platí jen poslední oznámení;
// když se nějaké ztratí (`mezera`), okno si načte všechno znovu.
import type {
	Cil,
	Klavesa,
	KlavesyInfo,
	Oznameni,
	Rezim,
	RezimInfo,
	StavKonfigurace,
	Vazba,
	Vstup,
	ZmenaKlaves
} from './smlouva';
import { pridej } from './pady.svelte';
import { poslouchej, textChyby, zavolej } from './tauri';
import { KRATKE, MAX_PADU, textOdmitnuti, textPresunu, VSTUPY } from './vstupy';
import { nactiZive } from './zive.svelte';

export const klavesy = $state({
	/** Revize mapování; −1 = zatím nenačteno. */
	rev: -1,
	/** Pořadí bitů živého stavu (od backendu). */
	vstupy: [...VSTUPY] as string[],
	zkratka: null as Klavesa | null,
	vazby: [] as Vazba[],
	/** Poslední změnu jde vrátit. */
	zpet: false,
	konfigurace: 'ok' as StavKonfigurace,
	zaloha: null as string | null,
	chyby: [] as string[]
});

export const rezim = $state({
	rezim: 'disabled' as Rezim,
	/** Pořadí poslední převzaté změny; −1 = zatím nic. */
	seq: -1,
	cil: null as Cil | null,
	hookChyba: false
});

export interface CilPrirazeni extends Cil {
	/** `+`: klávesa se přidá k ostatním (jinak je nahradí). */
	pridat: boolean;
}

export const prirazovani = $state({
	/**
	 * Kliknutý vstup, dokud ho backend nepotvrdí změnou režimu — čepička
	 * pulzuje hned po kliknutí, ne až po cestě přes hook vlákno.
	 */
	pozadavek: null as CilPrirazeni | null,
	/** Poslední vyžádané přiřazování — podle něj se pozná „přidat". */
	posledni: null as CilPrirazeni | null,
	/** Roste s každým novým přiřazováním — restartuje odpočet. */
	id: 0,
	/** Kdy přiřazování začalo (performance.now()). */
	od: 0,
	/**
	 * Revize mapování při kliknutí. „Zpět" po uložení se od ní odvíjí,
	 * ne od revize v chvíli oznámení: nové klávesy se můžou načíst dřív,
	 * než oznámení dorazí, a „Zpět" by pak čekalo na revizi, která už přišla.
	 */
	revPred: -1
});

/**
 * Limit přiřazování v jádře (`BINDING_TIMEOUT_MS`) — jen pro odpočet
 * v okně. Přiřazování ukončí backend (oznámení `zruseno` / `cas`), ne
 * tenhle údaj.
 */
export const PRIRAZENI_MS = 10_000;

/** Jediný řádek nápovědy v rozbalené kartě. */
export const zprava = $state({
	text: '',
	/** Kterému ovladači patří; −1 = té kartě, která je rozbalená. */
	pad: -1,
	/** Nabídnout „Zpět" (jen když backend má co vrátit). */
	zpet: false,
	/** Revize před změnou — „Zpět" až po načtení novější. */
	revPred: -1,
	chyba: false,
	/** Platí jen během přiřazování (odmítnutá klávesa — čeká se dál). */
	behem: false,
	/**
	 * Odmítnutá klávesa („Win patří Windows") — jantarově, i když
	 * přiřazování mezitím skončilo a zpráva dožívá (`dozijOdmitnuti`).
	 */
	odmitnuti: false,
	/** Podrobnost do bubliny. */
	titulek: '',
	/** „Nejdřív zapni ovladač" — zapnutím ovladače přestává platit. */
	zapni: false,
	id: 0
});

export type Efekt = 'ulozeno' | 'presunuto' | 'odmitnuto';

/** Jednorázové animace čepiček podle klíče „pad:vstup". */
export const efekty: Record<string, { druh: Efekt; id: number }> = $state({});
/** Záblesk odznaku karty (klávesa se přesunula z jiného ovladače). */
export const odznaky: number[] = $state(Array.from({ length: MAX_PADU }, () => 0));
/** Pulz přepínačů (zkratka bez zapnutého ovladače). */
export const pulzPrepinacu = $state({ id: 0 });
/** Poslední oznámení — pro testy okna na skryté ploše (`data-oznameni`). */
export const posledniOznameni = $state({ typ: '', seq: -1 });

/** Jak dlouho čeká kliknutí na potvrzení režimem (okno nemusí mít popředí). */
const POTVRZENI_MS = 1000;
/** „Zpět" po přesunu (spec 1.4). */
const ZPET_MS = 5000;
const ZPRAVA_MS = 4000;
/** Odmítnutá klávesa — přiřazování běží dál, nápověda se pak vrátí. */
const ODMITNUTI_MS = 2500;
/** Přiřazování začaté jinde (načtení po mezeře) se počítá od teď. */
const CERSTVE_KLIKNUTI_MS = 1500;

let citac = 0;
let casovacZpravy: ReturnType<typeof setTimeout> | undefined;
let pojistka: ReturnType<typeof setTimeout> | undefined;
/** Režim, který okno znalo při kliknutí — novější už o kliknutí rozhodl. */
let seqPriKliku = -1;

/** Co se právě přiřazuje (kliknutí čekající na backend, nebo potvrzený cíl). */
export function cilPrirazeni(): CilPrirazeni | null {
	const p = prirazovani.pozadavek;
	if (p) return p;
	const c = rezim.cil;
	if (rezim.rezim !== 'binding' || !c) return null;
	const l = prirazovani.posledni;
	return { pad: c.pad, vstup: c.vstup, pridat: !!l && l.pad === c.pad && l.vstup === c.vstup && l.pridat };
}

interface VolbyZpravy {
	zpet?: boolean;
	chyba?: boolean;
	/** Odmítnutá klávesa během přiřazování (`behem` i `odmitnuti`). */
	odmitnuti?: boolean;
	titulek?: string;
	ms?: number;
	/** Revize před změnou (pro „Zpět"); bez ní ta, kterou okno zná teď. */
	revPred?: number;
	/** Rada „Nejdřív zapni ovladač" — zmizí se zapnutím ovladače. */
	zapni?: boolean;
}

/** Zpráva `id` zmizí za `ms` — pokud ji mezitím nenahradila jiná. */
function naplanujKonec(id: number, ms: number): void {
	clearTimeout(casovacZpravy);
	casovacZpravy = setTimeout(() => {
		if (zprava.id === id) zprava.text = '';
	}, ms);
}

function ukazZpravu(pad: number, text: string, v: VolbyZpravy = {}): void {
	const id = ++zprava.id;
	zprava.text = text;
	zprava.pad = pad;
	zprava.zpet = !!v.zpet;
	zprava.revPred = v.revPred ?? klavesy.rev;
	zprava.chyba = !!v.chyba;
	zprava.behem = !!v.odmitnuti;
	zprava.odmitnuti = !!v.odmitnuti;
	zprava.titulek = v.titulek ?? '';
	zprava.zapni = !!v.zapni;
	naplanujKonec(id, v.ms ?? ZPRAVA_MS);
}

export function skryjZpravu(): void {
	zprava.id++;
	zprava.text = '';
	zprava.zapni = false;
	clearTimeout(casovacZpravy);
}

/**
 * Přiřazování skončilo — odmítnutí („Win patří Windows") dožije jako
 * obyčejná zpráva, místo aby zmizelo s přiřazováním. Win totiž otevře
 * Start, okno tím ztratí popředí a backend přiřazování zruší (spec 2.4):
 * vysvětlení by jinak zmizelo právě tam, kvůli čemu existuje (kontrolní
 * bod vlastníka 8). Stejně dopadne klávesa, která spustí aplikaci
 * (Kalkulačka, Pošta). Volá se už se změnou režimu, ne až s oznámením
 * `zruseno`, které chodí po ní — řádek by jinak na okamžik zhasl.
 * Esc a klik jinam (uživatel chce klid) zprávu schovají samy.
 */
function dozijOdmitnuti(): void {
	if (!zprava.behem || zprava.text === '') return;
	zprava.behem = false;
	// Celá doba obyčejné zprávy od konce přiřazování: uživatel se
	// k oknu vrací až po zavření Startu.
	naplanujKonec(zprava.id, ZPRAVA_MS);
}

function zablikni(pad: number, vstup: Vstup, druh: Efekt): void {
	efekty[`${pad}:${vstup}`] = { druh, id: ++citac };
}

function prevezmiRezim(r: RezimInfo): void {
	if (r.seq < rezim.seq) return;
	const prirazoval = cilPrirazeni() !== null;
	rezim.rezim = r.rezim;
	rezim.seq = r.seq;
	rezim.cil = r.cil ?? null;
	rezim.hookChyba = !!r.hook_chyba;
	// Novější režim už o kliknutí rozhodl (přijal, nebo ne) — dál platí
	// jen to, co hlásí backend.
	if (prirazovani.pozadavek && r.seq > seqPriKliku) prirazovani.pozadavek = null;
	const ted = cilPrirazeni() !== null;
	if (ted && !prirazoval && performance.now() - prirazovani.od > CERSTVE_KLIKNUTI_MS) {
		prirazovani.id++;
		prirazovani.od = performance.now();
	}
	// Uživatel radu poslechl a přepínačem ovladač zapnul (hra, nebo hned
	// pauza zkratkou): věta „Nejdřív zapni ovladač" by vedle zelené tečky
	// tvrdila opak — nalezeno testem okna na skryté ploše (B6).
	if (zprava.zapni && (r.rezim === 'capturing' || r.rezim === 'paused')) skryjZpravu();
	if (prirazoval && !ted) dozijOdmitnuti();
	if (prirazoval && !ted && r.hook_chyba) {
		ukazZpravu(-1, 'Klávesy teď nejde sledovat', {
			chyba: true,
			titulek: 'Windows nedovolily sledovat klávesnici — podrobnosti jsou v logu.'
		});
	}
}

function prevezmiKlavesy(k: KlavesyInfo): void {
	if (k.rev < klavesy.rev) return;
	klavesy.rev = k.rev;
	klavesy.vstupy = k.vstupy;
	klavesy.zkratka = k.zkratka;
	klavesy.vazby = k.vazby;
	klavesy.zpet = !!k.zpet;
	klavesy.konfigurace = k.konfigurace;
	klavesy.zaloha = k.zaloha ?? null;
	klavesy.chyby = k.chyby ?? [];
}

function prevezmiOznameni(o: Oznameni): void {
	posledniOznameni.typ = o.typ;
	posledniOznameni.seq = o.seq;
	if (o.mezera) void nactiVse();
	const cil = cilPrirazeni();
	switch (o.typ) {
		case 'ulozeno':
			prirazovani.pozadavek = null;
			zablikni(o.pad, o.vstup, 'ulozeno');
			if (o.odkud) {
				zablikni(o.odkud.pad, o.odkud.vstup, 'presunuto');
				if (o.odkud.pad !== o.pad && o.odkud.pad >= 0 && o.odkud.pad < MAX_PADU) {
					odznaky[o.odkud.pad] = ++citac;
				}
				ukazZpravu(o.pad, textPresunu(o.odkud, o.pad), {
					zpet: true,
					ms: ZPET_MS,
					revPred: prirazovani.revPred
				});
			} else {
				skryjZpravu();
			}
			break;
		case 'odmitnuto':
			if (cil) zablikni(cil.pad, cil.vstup, 'odmitnuto');
			ukazZpravu(cil?.pad ?? -1, textOdmitnuti(o.duvod, klavesy.zkratka?.nazev), {
				odmitnuti: true,
				ms: ODMITNUTI_MS
			});
			break;
		case 'zruseno':
			prirazovani.pozadavek = null;
			// Esc = „nechci" — návrat do klidu i bez dožívajícího odmítnutí.
			// Ztráta popředí, 10 s a vynucení ho nechají dožít.
			if (o.duvod === 'esc' && zprava.odmitnuti) skryjZpravu();
			break;
		case 'zapni_ovladac':
			pulzPrepinacu.id = ++citac;
			ukazZpravu(-1, 'Nejdřív zapni ovladač', { ms: ODMITNUTI_MS, zapni: true });
			break;
	}
}

/** Přihlásí se k událostem — vždy dřív, než se stav načte. */
export async function prihlasKlavesy(): Promise<void> {
	await Promise.all([
		poslouchej<RezimInfo>('rezim', prevezmiRezim),
		poslouchej<Oznameni>('oznameni', prevezmiOznameni),
		poslouchej<{ rev: number }>('klavesy-zmena', (z) => {
			if (z.rev > klavesy.rev) void nactiKlavesy();
		}),
		poslouchej<{ stav: StavKonfigurace }>('konfigurace', (k) => {
			klavesy.konfigurace = k.stav;
		})
	]);
}

export async function nactiKlavesy(): Promise<void> {
	try {
		prevezmiKlavesy(await zavolej<KlavesyInfo>('klavesy'));
	} catch {
		// Bez backendu zůstane prázdné schéma.
	}
}

export async function nactiRezim(): Promise<void> {
	try {
		prevezmiRezim(await zavolej<RezimInfo>('rezim'));
	} catch {
		// Mimo aplikaci backend není — zůstane „vypnuto".
	}
}

/** Po ztraceném oznámení: všechno, co z oznámení plyne, načíst znovu. */
export async function nactiVse(): Promise<void> {
	await Promise.all([nactiKlavesy(), nactiRezim(), nactiZive()]);
}

/** Klik na čepičku (nahradit) nebo na `+` (přidat). */
export async function prirad(pad: number, vstup: Vstup, pridat: boolean): Promise<void> {
	const id = ++prirazovani.id;
	prirazovani.pozadavek = { pad, vstup, pridat };
	prirazovani.posledni = { pad, vstup, pridat };
	prirazovani.od = performance.now();
	prirazovani.revPred = klavesy.rev;
	seqPriKliku = rezim.seq;
	skryjZpravu();
	// Backend kliknutí bez okna v popředí potichu nepřijme — čepička by
	// pak pulzovala navždy.
	clearTimeout(pojistka);
	pojistka = setTimeout(() => {
		if (prirazovani.id === id) prirazovani.pozadavek = null;
	}, POTVRZENI_MS);
	try {
		await zavolej('prirad', { pad, vstup, pridat });
	} catch (e) {
		if (prirazovani.id === id) prirazovani.pozadavek = null;
		ukazZpravu(pad, textChyby(e), { chyba: true });
	}
}

/** Esc, klik jinam: přiřazování skončí, nic se nezmění. */
export function zrusPrirazeni(): void {
	if (cilPrirazeni() === null) return;
	prirazovani.pozadavek = null;
	// Backend zrušení ohlásí jako ztrátu popředí (`zruseno okno`), po
	// které odmítnutí dožívá — tady ale uživatel chce klid sám.
	if (zprava.odmitnuti) skryjZpravu();
	zavolej('zrus_prirazeni').catch(() => undefined);
}

async function uprav(zmena: ZmenaKlaves, pad: number): Promise<boolean> {
	try {
		await zavolej('uprav_klavesy', { zmena });
		return true;
	} catch (e) {
		ukazZpravu(pad, textChyby(e), { chyba: true });
		return false;
	}
}

/** `×` nebo pravý klik: vstup bez kláves. */
export async function vyprazdni(pad: number, vstup: Vstup): Promise<void> {
	const pred = klavesy.rev;
	// Karta, se kterou uživatel pracuje, zůstane až do 🗑, i když jí
	// vyprázdní poslední klávesu. Ovladač s klávesami z config.json
	// mezi přidanými v sezení není — jeho karta by po poslední klávese
	// zmizela i se zprávou a „Zpět" (kap. 0: karta přidaná v sezení).
	// Předem, ne po odpovědi: nové klávesy se můžou načíst dřív.
	pridej(pad);
	if (await uprav({ typ: 'vyprazdnit', pad, vstup }, pad)) {
		ukazZpravu(pad, `Vyprázdněno: ${KRATKE[vstup]}`, { zpet: true, ms: ZPET_MS, revPred: pred });
	}
}

/** Výchozí klávesy ovladače 1 (klávesy ostatních ovladačů nebere, OQ 49). */
export async function vychozi(): Promise<void> {
	const pred = klavesy.rev;
	if (await uprav({ typ: 'vychozi' }, 0)) {
		ukazZpravu(0, 'Výchozí klávesy', { zpet: true, ms: ZPET_MS, revPred: pred });
	}
}

/** „Zpět" — vrátí poslední změnu, jen když od ní nic dalšího nepřišlo (revize). */
export async function zpet(): Promise<void> {
	const pad = zprava.pad;
	skryjZpravu();
	await uprav({ typ: 'zpet', rev: klavesy.rev }, pad);
}

/** Smí nápověda nabídnout „Zpět"? Až backend ohlásí novější revizi s předchůdcem. */
export function lzeVratit(): boolean {
	return zprava.zpet && klavesy.zpet && klavesy.rev > zprava.revPred;
}

export type DruhNapovedy = 'prirazovani' | 'odmitnuto' | 'zprava' | 'chyba' | 'ticha';

export interface Napoveda {
	text: string;
	druh: DruhNapovedy;
	titulek?: string;
	zpet?: boolean;
}

/**
 * Jediný řádek nápovědy rozbalené karty ovladače `pad` (`pocet` = kolik
 * má klávesy). Tady, ne v kartě: o tom, co je vidět, rozhoduje pořadí
 * událostí, a to zkouší `bun test` (klavesy.test.ts).
 */
export function napovedaKarty(pad: number, pocet: number): Napoveda | null {
	const cil = cilPrirazeni();
	const mojeZprava = zprava.text !== '' && (zprava.pad === pad || zprava.pad === -1);
	if (cil && cil.pad === pad) {
		if (mojeZprava && zprava.behem) return { text: zprava.text, druh: 'odmitnuto' };
		return {
			text: cil.pridat ? 'Přidej klávesu · Esc zruší' : 'Stiskni klávesu · Esc zruší',
			druh: 'prirazovani'
		};
	}
	if (mojeZprava && !zprava.behem) {
		return {
			text: zprava.text,
			druh: zprava.chyba ? 'chyba' : zprava.odmitnuti ? 'odmitnuto' : 'zprava',
			titulek: zprava.titulek || undefined,
			zpet: lzeVratit()
		};
	}
	// Se zapnutým ovladačem to už říká slovo v hlavičce.
	if (rezim.hookChyba && rezim.rezim !== 'no_hook') {
		return {
			text: 'Klávesy teď nejde ukázat',
			druh: 'chyba',
			titulek: 'Windows nedovolily sledovat klávesnici — podrobnosti jsou v logu.'
		};
	}
	if (pocet === 0) return { text: 'Klikni na vstup a stiskni klávesu', druh: 'ticha' };
	return null;
}

/** Odebere vypnutý ovladač 2–4 i s jeho klávesami. */
export async function odeberOvladac(pad: number): Promise<boolean> {
	try {
		await zavolej('odeber_ovladac', { pad });
		return true;
	} catch (e) {
		ukazZpravu(pad, textChyby(e), { chyba: true });
		return false;
	}
}
