// Klávesy ovladačů, režim zachytávání a přiřazování (Fáze 6).
//
// Zdrojem pravdy jsou režim a mapování v backendu. Okno je načítá
// příkazy `rezim` a `klavesy` a změny dostává událostmi `rezim`
// a `klavesy-zmena` (ta nese jen revizi — názvy kláves se pak čtou
// znovu, aby seděly s rozložením klávesnice). Událost `oznameni` jen
// říká, co se stalo a proč (uloženo, odmítnuto, zrušeno): podle ní okno
// blikne, zatřese a napíše jednu větu. Platí jen poslední oznámení;
// když se nějaké ztratí (`mezera`), okno si načte všechno znovu.
//
// Fáze 7: karty se rozbalují každá zvlášť (rozbalení pamatuje backend
// v config.json), volby z ⓘ (zvuk, jedna klávesa pro víc vstupů) mají
// zdroj pravdy v backendu (`nastaveni`), sdílená klávesa se ohlásí
// „Sdíleno s …" a zkratka pozastavení se přiřazuje stejně jako klávesa
// vstupu (cíl `{ zkratka: true }`).
import type {
	Cil,
	KartyInfo,
	Klavesa,
	KlavesaOznameni,
	KlavesyInfo,
	NastaveniInfo,
	Oznameni,
	Rezim,
	RezimInfo,
	StavKonfigurace,
	Vazba,
	Volba,
	Vstup,
	ZmenaKlaves
} from './smlouva';
import { poslouchej, textChyby, zavolej } from './tauri';
import {
	cileKlaves,
	dalsiCile,
	KRATKE,
	klicKlavesy,
	MAX_PADU,
	textOdmitnuti,
	textPresunu,
	textSdileni,
	VSTUPY
} from './vstupy';
import { nactiZive } from './zive.svelte';

export const klavesy = $state({
	/** Revize mapování; −1 = zatím nenačteno. */
	rev: -1,
	/** Pořadí bitů živého stavu (od backendu). */
	vstupy: [...VSTUPY] as string[],
	zkratka: null as Klavesa | null,
	/** Zkratka mimo F1–F24 bez F4, Scroll Lock a Pause (ručně v config.json, OQ 69). */
	zkratkaMimo: false,
	vazby: [] as Vazba[],
	/** Poslední změnu jde vrátit. */
	zpet: false,
	konfigurace: 'ok' as StavKonfigurace,
	zaloha: null as string | null,
	chyby: [] as string[],
	/**
	 * Ovladače s kartou (uložené v backendu, OQ 52) — karta zůstane i bez
	 * kláves a po restartu, zmizí jen 🗑.
	 */
	karty: [0] as number[],
	/**
	 * Rozbalené karty (Fáze 7, Z2): každá zvlášť, víc i všechny. Pamatuje je
	 * backend v config.json (OQ 70); bez uloženého stavu rozbalený ovladač 1.
	 */
	rozbalene: [0] as number[],
	/** Pořadí seznamu karet; −1 = zatím nenačteno. */
	kartyRev: -1
});

/** Volby z ⓘ (Fáze 7, Z6) — zdroj pravdy je backend (i „✓ Zvuk" v nabídce ikony). */
export const nastaveni = $state({
	/** Pořadí změny; −1 = zatím nenačteno. */
	rev: -1,
	zvuk: true,
	/** Jedna klávesa pro víc vstupů. */
	sdilene_klavesy: false
});

export const rezim = $state({
	rezim: 'disabled' as Rezim,
	/** Pořadí poslední převzaté změny; −1 = zatím nic. */
	seq: -1,
	/** Přiřazovaný vstup (jen v `binding`). */
	cil: null as Cil | null,
	/** Přiřazuje se zkratka pozastavení (`cil: { zkratka: true }`, Z6). */
	zkratka: false,
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
	/** Kliknutá čepička „Pauza" v ⓘ, dokud ji backend nepotvrdí (Z6). */
	zkratka: false,
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

/** Řádek pod čepičkou „Pauza" v ⓘ (přiřazování zkratky, Z6). */
export const zpravaZkratky = $state({
	text: '',
	druh: '' as '' | 'odmitnuto' | 'zprava' | 'chyba',
	id: 0
});

export type Efekt = 'ulozeno' | 'presunuto' | 'odmitnuto' | 'sdileno';

/** Jednorázové animace čepiček podle klíče „pad:vstup". */
export const efekty: Record<string, { druh: Efekt; id: number }> = $state({});
/** Jednorázová animace čepičky „Pauza" v ⓘ. */
export const efektZkratky = $state({ druh: 'ulozeno' as Efekt, id: 0 });
/**
 * Klávesy sdílené čepičky pod myší (klíče `klicKlavesy`) — ostatní čepičky
 * téže klávesy a odznak sbalené karty se zvýrazní (Z4). Jen stav najetí,
 * žádný časovač.
 */
export const zvyraznene = $state({ klice: [] as string[] });
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

/** Který vstup se právě přiřazuje (kliknutí čekající na backend, nebo potvrzený cíl). */
export function cilPrirazeni(): CilPrirazeni | null {
	if (prirazovani.zkratka) return null;
	const p = prirazovani.pozadavek;
	if (p) return p;
	const c = rezim.cil;
	if (rezim.rezim !== 'binding' || !c) return null;
	const l = prirazovani.posledni;
	return { pad: c.pad, vstup: c.vstup, pridat: !!l && l.pad === c.pad && l.vstup === c.vstup && l.pridat };
}

/** Přiřazuje se zkratka pozastavení (kliknutí čekající na backend, nebo potvrzené)? */
export function prirazujeZkratku(): boolean {
	if (prirazovani.zkratka) return true;
	return !prirazovani.pozadavek && rezim.rezim === 'binding' && rezim.zkratka;
}

/** Běží jakékoli přiřazování (vstupu i zkratky)? Pak Esc a klik jinam ruší. */
export function prirazuje(): boolean {
	return cilPrirazeni() !== null || prirazujeZkratku();
}

/** Název klávesy podle rozložení (z načtených kláves), nebo ''. */
export function nazevKlavesy(k: KlavesaOznameni): string {
	const z = klavesy.zkratka;
	if (z && z.scan === k.scan && z.e0 === k.e0) return z.nazev;
	return klavesy.vazby.find((v) => v.klavesa.scan === k.scan && v.klavesa.e0 === k.e0)?.klavesa.nazev ?? '';
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

let casovacZkratky: ReturnType<typeof setTimeout> | undefined;

/** Řádek pod čepičkou „Pauza" (odmítnutá klávesa, uloženo, chyba). */
function ukazZpravuZkratky(text: string, druh: 'odmitnuto' | 'zprava' | 'chyba'): void {
	const id = ++zpravaZkratky.id;
	zpravaZkratky.text = text;
	zpravaZkratky.druh = druh;
	clearTimeout(casovacZkratky);
	casovacZkratky = setTimeout(() => {
		if (zpravaZkratky.id === id) skryjZpravuZkratky();
	}, druh === 'odmitnuto' ? ODMITNUTI_MS : ZPRAVA_MS);
}

export function skryjZpravuZkratky(): void {
	zpravaZkratky.id++;
	zpravaZkratky.text = '';
	zpravaZkratky.druh = '';
	clearTimeout(casovacZkratky);
}

/**
 * Řádek pod čepičkou „Pauza": během přiřazování výzva (nebo odmítnutí,
 * které přiřazování nekončí), jinak poslední zpráva, nebo nic.
 */
export function napovedaZkratky(): { text: string; druh: string } | null {
	if (zpravaZkratky.text) return { text: zpravaZkratky.text, druh: zpravaZkratky.druh };
	if (prirazujeZkratku()) return { text: 'Stiskni F1–F24 (ne F4), Scroll Lock nebo Pause · Esc zruší', druh: 'prirazovani' };
	return null;
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
	const prirazoval = prirazuje();
	rezim.rezim = r.rezim;
	rezim.seq = r.seq;
	const c = r.cil ?? null;
	rezim.zkratka = !!c && 'zkratka' in c && c.zkratka === true;
	rezim.cil = c && 'pad' in c ? c : null;
	rezim.hookChyba = !!r.hook_chyba;
	// Novější režim už o kliknutí rozhodl (přijal, nebo ne) — dál platí
	// jen to, co hlásí backend.
	if ((prirazovani.pozadavek || prirazovani.zkratka) && r.seq > seqPriKliku) {
		prirazovani.pozadavek = null;
		prirazovani.zkratka = false;
	}
	const ted = prirazuje();
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
	klavesy.zkratkaMimo = !!k.zkratka_mimo;
	klavesy.vazby = k.vazby;
	klavesy.zpet = !!k.zpet;
	klavesy.konfigurace = k.konfigurace;
	klavesy.zaloha = k.zaloha ?? null;
	klavesy.chyby = k.chyby ?? [];
	prevezmiKarty(k.karty);
	if (k.nastaveni) prevezmiNastaveni(k.nastaveni);
}

/**
 * Změny karet poslané backendu, na které ještě nepřišla odpověď. Do té doby
 * okno ukazuje svou představu (rozbalení hned po kliku) a seznam karet se
 * stejným pořadím (načtení kláves, které změnu předběhlo) ji nepřepíše.
 */
let kartyCekaji = 0;

/**
 * Seznam karet od backendu. Odpověď příkazu a načtení kláves se můžou
 * předběhnout — starší seznam se zahodí (`rev`).
 */
function prevezmiKarty(k: KartyInfo | null | undefined): void {
	if (!k || !Array.isArray(k.pady) || k.rev < klavesy.kartyRev) return;
	if (kartyCekaji > 0 && k.rev === klavesy.kartyRev) return;
	klavesy.kartyRev = k.rev;
	klavesy.karty = [...k.pady];
	// Starší backend `rozbalene` nepošle — pak zůstane, co okno má.
	if (Array.isArray(k.rozbalene)) klavesy.rozbalene = [...k.rozbalene];
}

/** Příkaz, který mění karty: okno ukáže změnu hned, backend ji potvrdí. */
async function zmenKarty(prikaz: string, args: Record<string, unknown>, pad: number): Promise<boolean> {
	kartyCekaji++;
	try {
		const k = await zavolej<KartyInfo>(prikaz, args);
		kartyCekaji--;
		prevezmiKarty(k);
		return true;
	} catch (e) {
		kartyCekaji--;
		ukazZpravu(pad, textChyby(e), { chyba: true });
		// Okno ukáže, co backend opravdu má.
		void nactiKlavesy();
		return false;
	}
}

/** Karta hned v okně, dřív než odpoví backend (ten ji uloží a potvrdí). */
function ukazKartu(pad: number): void {
	if (!klavesy.karty.includes(pad)) klavesy.karty = [...klavesy.karty, pad];
}

function rozbalLokalne(pad: number, rozbalena: boolean): void {
	const bez = klavesy.rozbalene.filter((p) => p !== pad);
	klavesy.rozbalene = rozbalena ? [...bez, pad].sort((a, b) => a - b) : bez;
}

/**
 * Karta právě přidaná „+ Ovladač" (−1 = žádná): karta se po vykreslení
 * posune do zorného pole a příznak shodí. Vzniká rovnou rozbalená, takže
 * se nepřehraje přechod rozbalení, po kterém se jinak posouvá — nová karta
 * by zůstala pod okrajem panelu (nalezeno revizí).
 */
export const novaKarta = $state({ pad: -1 });

/**
 * „+ Ovladač": karta ovladače i bez kláves, rozbalená. Backend ji uloží
 * (OQ 52) — zůstane i po restartu, zmizí jen 🗑. Nic nepřipojí (princip 11).
 */
export async function pridejKartu(pad: number): Promise<void> {
	novaKarta.pad = pad;
	ukazKartu(pad);
	rozbalLokalne(pad, true);
	await zmenKarty('pridej_kartu', { pad }, pad);
}

/**
 * Klik na hlavičku karty (Fáze 7, Z2): rozbalit, nebo sbalit — každou
 * zvlášť. Okno přepne hned, backend stav uloží do config.json (OQ 70).
 * Sbalení karty s přiřazovanou čepičkou přiřazování zruší — čepička by
 * jinak pulzovala tam, kde ji nikdo nevidí.
 */
export async function rozbalKartu(pad: number, rozbalena: boolean): Promise<void> {
	if (!rozbalena && cilPrirazeni()?.pad === pad) zrusPrirazeni();
	rozbalLokalne(pad, rozbalena);
	await zmenKarty('rozbal_kartu', { pad, rozbalena }, pad);
}

/** Volby od backendu — starší (odpověď po novější události) se zahodí. */
function prevezmiNastaveni(n: NastaveniInfo): void {
	if (n.rev < nastaveni.rev) return;
	nastaveni.rev = n.rev;
	nastaveni.zvuk = !!n.zvuk;
	nastaveni.sdilene_klavesy = !!n.sdilene_klavesy;
}

export async function nactiNastaveni(): Promise<void> {
	try {
		prevezmiNastaveni(await zavolej<NastaveniInfo>('nastaveni'));
	} catch {
		// Bez backendu zůstanou výchozí hodnoty.
	}
}

/**
 * Přepínač volby v ⓘ: přepne hned, backend uloží a srovná nabídku ikony.
 * Chyba vrátí, co backend opravdu má. Vrací text chyby, nebo ''.
 */
export async function nastav(volba: Volba, zapnuto: boolean): Promise<string> {
	const pred = nastaveni[volba];
	nastaveni[volba] = zapnuto;
	try {
		prevezmiNastaveni(await zavolej<NastaveniInfo>('nastav', { volba, zapnuto }));
		return '';
	} catch (e) {
		nastaveni[volba] = pred;
		void nactiNastaveni();
		return textChyby(e);
	}
}

/** ▷ ukázka zvuku (Z1): '' = hraje, jinak důvod do bubliny (simulace, vypnutý zvuk). */
export async function ukazkaZvuku(): Promise<string> {
	try {
		await zavolej('ukazka_zvuku');
		return '';
	} catch (e) {
		return textChyby(e);
	}
}

function prevezmiOznameni(o: Oznameni): void {
	posledniOznameni.typ = o.typ;
	posledniOznameni.seq = o.seq;
	if (o.mezera) void nactiVse();
	const cil = cilPrirazeni();
	const zkratka = prirazujeZkratku();
	switch (o.typ) {
		case 'ulozeno':
			prirazovani.pozadavek = null;
			prirazovani.zkratka = false;
			if (o.zkratka) {
				// Nová zkratka pozastavení (Z6) — název ukáže čepička „Pauza"
				// po načtení kláves.
				efektZkratky.druh = 'ulozeno';
				efektZkratky.id = ++citac;
				ukazZpravuZkratky('Uloženo', 'zprava');
				// Zkratka zvedla revizi — „Zpět" zprávy karty už neplatí.
				if (zprava.zpet) skryjZpravu();
				break;
			}
			zablikni(o.pad, o.vstup, 'ulozeno');
			if (o.odkud) {
				zablikni(o.odkud.pad, o.odkud.vstup, 'presunuto');
				if (o.odkud.pad !== o.pad && o.odkud.pad >= 0 && o.odkud.pad < MAX_PADU) {
					odznaky[o.odkud.pad] = ++citac;
				}
				ukazZpravu(o.pad, textPresunu(o.odkud, o.pad, o.odkud_dalsi ?? 0), {
					zpet: true,
					ms: ZPET_MS,
					revPred: prirazovani.revPred
				});
			} else if ((o.sdileno ?? 0) > 0) {
				// Sdílená klávesa (Z4): s kým se dělí, vědí vazby — staré
				// i nové dávají tytéž ostatní vstupy (cíl se jen přidal).
				const dalsi = dalsiCile(cileKlaves(klavesy.vazby), o.klavesa, o.pad, o.vstup);
				for (const d of dalsi) zablikni(d.pad, d.vstup, 'sdileno');
				ukazZpravu(o.pad, dalsi.length > 0 ? textSdileni(dalsi, o.pad) : `Sdíleno se ${o.sdileno + 1} vstupy`, {
					zpet: true,
					ms: ZPET_MS,
					revPred: prirazovani.revPred
				});
			} else {
				skryjZpravu();
			}
			break;
		case 'odmitnuto': {
			const kontext = {
				zkratka: klavesy.zkratka?.nazev,
				klavesa: nazevKlavesy(o.klavesa),
				kam: cileKlaves(klavesy.vazby).get(klicKlavesy(o.klavesa))?.[0] ?? null
			};
			if (zkratka) {
				efektZkratky.druh = 'odmitnuto';
				efektZkratky.id = ++citac;
				ukazZpravuZkratky(textOdmitnuti(o.duvod, kontext), 'odmitnuto');
				break;
			}
			if (cil) zablikni(cil.pad, cil.vstup, 'odmitnuto');
			ukazZpravu(cil?.pad ?? -1, textOdmitnuti(o.duvod, kontext), {
				odmitnuti: true,
				ms: ODMITNUTI_MS
			});
			break;
		}
		case 'zruseno':
			prirazovani.pozadavek = null;
			prirazovani.zkratka = false;
			if (zpravaZkratky.druh === 'odmitnuto' && o.duvod === 'esc') skryjZpravuZkratky();
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
		}),
		// Volby z ⓘ — i po změně „✓ Zvuk" v nabídce ikony (Z6).
		poslouchej<NastaveniInfo>('nastaveni', prevezmiNastaveni)
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
	prirazovani.zkratka = false;
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

/**
 * Klik na čepičku „Pauza" v ⓘ (Z6): přiřazovat zkratku pozastavení — stejně
 * jako klávesu vstupu (pulz, 10 s, Esc, klik jinam, jen s oknem v popředí).
 * Které klávesy smí být zkratkou, hlídá engine (odmítnutí přijde oznámením).
 */
export async function priradZkratku(): Promise<void> {
	const id = ++prirazovani.id;
	prirazovani.pozadavek = null;
	prirazovani.zkratka = true;
	prirazovani.od = performance.now();
	prirazovani.revPred = klavesy.rev;
	seqPriKliku = rezim.seq;
	skryjZpravuZkratky();
	// Nová zkratka zvedne revizi mapování a „Zpět" zprávy karty by už
	// nebylo k čemu — nová akce = nová zpráva (jako klik na čepičku).
	skryjZpravu();
	clearTimeout(pojistka);
	pojistka = setTimeout(() => {
		if (prirazovani.id === id) prirazovani.zkratka = false;
	}, POTVRZENI_MS);
	try {
		await zavolej('prirad_zkratku');
	} catch (e) {
		if (prirazovani.id === id) prirazovani.zkratka = false;
		ukazZpravuZkratky(textChyby(e), 'chyba');
	}
}

/** Esc, klik jinam: přiřazování skončí, nic se nezmění. */
export function zrusPrirazeni(): void {
	if (!prirazuje()) return;
	prirazovani.pozadavek = null;
	prirazovani.zkratka = false;
	// Backend zrušení ohlásí jako ztrátu popředí (`zruseno okno`), po
	// které odmítnutí dožívá — tady ale uživatel chce klid sám.
	if (zprava.odmitnuti) skryjZpravu();
	if (zpravaZkratky.druh === 'odmitnuto') skryjZpravuZkratky();
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
	// vyprázdní poslední klávesu — backend ji při vyprázdnění uloží mezi
	// karty (OQ 52). Tady jen předem, ne po odpovědi: nové klávesy se
	// můžou načíst dřív a karta by na okamžik zmizela i se zprávou.
	ukazKartu(pad);
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

/**
 * „Zpět" — vrátí změnu, o které mluví zpráva: revizi hned po ní. Ne tu,
 * kterou okno zná teď — mezitím mohla přijít jiná změna (zkratka, 🗑)
 * a backend vrací vždy předchůdce revize, kterou dostane: s aktuální by
 * „Zpět" po přesunu vrátil tu jinou změnu a přesun nechal (nalezeno revizí).
 */
export async function zpet(): Promise<void> {
	const pad = zprava.pad;
	const rev = zprava.revPred + 1;
	skryjZpravu();
	await uprav({ typ: 'zpet', rev }, pad);
}

/**
 * Smí nápověda nabídnout „Zpět"? Jen když backend ohlásil právě revizi po
 * změně ze zprávy (každá změna mapování ji zvedne přesně o 1) a má k ní
 * předchůdce. Jakákoli pozdější změna „Zpět" té zprávy zruší.
 */
export function lzeVratit(): boolean {
	return zprava.zpet && klavesy.zpet && klavesy.rev === zprava.revPred + 1;
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
	// Zpráva bez ovladače (−1) patří první rozbalené kartě — s víc
	// rozbalenými (Z2) by se jinak opakovala v každé.
	const prvniRozbalena = klavesy.rozbalene.length > 0 ? Math.min(...klavesy.rozbalene) : -1;
	const mojeZprava = zprava.text !== '' && (zprava.pad === pad || (zprava.pad === -1 && pad === prvniRozbalena));
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

/** Odebere vypnutý ovladač 2–4 i s jeho klávesami a kartou. */
export async function odeberOvladac(pad: number): Promise<boolean> {
	// Smazané klávesy zvednou revizi — „Zpět" starší zprávy jiné karty by
	// pak mířilo jinam. Nová akce = nová zpráva (jako klik na čepičku).
	skryjZpravu();
	try {
		prevezmiKarty(await zavolej<KartyInfo>('odeber_ovladac', { pad }));
		return true;
	} catch (e) {
		ukazZpravu(pad, textChyby(e), { chyba: true });
		return false;
	}
}
