// Smlouva okna s backendem (Fáze 6, spec B4 „Smlouva s oknem"; Fáze 7).
//
// Jen typy, žádný kód. Tvar dat pevně drží zlaté soubory v testdata/:
// Rust je serializuje ze vzorových hodnot a porovná, okno je ověřuje
// v `vstupy.test.ts` — rozjet se tak nemůžou ani jedna strana potichu.
//
// Čísla ovladačů jsou v příkazech i událostech od 0 (0–3), stejně jako
// u `pad-stav`; „Ovladač 1" je až text v okně. Od 1 počítá jen
// config.json, kam okno nesahá.

/** Vstup ovladače — stabilní kód z jádra (`Action::code`). */
export type Vstup =
	| 'ls_up'
	| 'ls_down'
	| 'ls_left'
	| 'ls_right'
	| 'rs_up'
	| 'rs_down'
	| 'rs_left'
	| 'rs_right'
	| 'a'
	| 'b'
	| 'x'
	| 'y'
	| 'lb'
	| 'rb'
	| 'l3'
	| 'r3'
	| 'start'
	| 'back'
	| 'dpad_up'
	| 'dpad_down'
	| 'dpad_left'
	| 'dpad_right'
	| 'lt'
	| 'rt';

/** Klávesa s názvy podle rozložení klávesnice (scan kód = pozice). */
export interface Klavesa {
	scan: number;
	e0: boolean;
	/** Celý název (`GetKeyNameTextW`) — do bubliny. */
	nazev: string;
	/** Krátký název na čepičku (šipky, ␣, ↵ …; jinak nejvýš 5 znaků). */
	kratky: string;
}

export interface Vazba {
	pad: number;
	vstup: Vstup;
	klavesa: Klavesa;
}

export type StavKonfigurace = 'ok' | 'obnovena' | 'novejsi' | 'necitelna' | 'neulozena';

/** Odpověď `klavesy`. */
export interface KlavesyInfo {
	/** Revize mapování — roste s každou skutečnou změnou. */
	rev: number;
	/** Pořadí bitů v `ZivaInfo` (`Action::ALL`). */
	vstupy: Vstup[];
	/** Zkratka pauzy (Scroll Lock; Fáze 7 Z6: mění ji ⓘ → Pauza). */
	zkratka: Klavesa;
	/**
	 * Zkratka není F1–F24 (bez F4), Scroll Lock ani Pause (ručně v config.json,
	 * OQ 69) — platí, ale Windows ji nedostanou, dokud je zapnutý ovladač.
	 * Okno ji označí jantarovou tečkou.
	 */
	zkratka_mimo: boolean;
	/**
	 * Všechny vazby v pořadí `Mapping::bindings()`: běžné klávesy podle
	 * scan kódu, pak rozšířené (E0). Backend je tak nemusí řadit a okno
	 * z pořadí bere jen to, která klávesa vstupu je na čepičce první.
	 *
	 * Fáze 7: sdílená klávesa (volba „Jedna klávesa pro víc vstupů") je
	 * tu víckrát — každý její vstup jednou, podle ovladače a pořadí
	 * vstupů (`vstupy`). Sdílenost (oranžová čepička, „také: …") si okno
	 * spočítá samo; víc než 4 vstupy jedné klávesy backend nikdy nepošle.
	 */
	vazby: Vazba[];
	/** Poslední změnu jde vrátit (`uprav_klavesy` `zpet`). */
	zpet: boolean;
	konfigurace: StavKonfigurace;
	/**
	 * Kam se odložil nevalidní config.json — u `obnovena` vždy, jinak null.
	 * Nevalidní soubor, který nešel odložit, je `necitelna` (neukládá se).
	 */
	zaloha: string | null;
	/**
	 * Co je v nevalidním config.json špatně — nejvýš pár vět pro bublinu
	 * pruhu (spec 1.7), celý výčet je v logu. Jen u `obnovena`
	 * a `necitelna`, jinak prázdné.
	 */
	chyby: string[];
	/** Ovladače s kartou v okně (uložené v config.json, OQ 52) a rozbalené. */
	karty: KartyInfo;
	/** Volby z panelu ⓘ (Fáze 7). */
	nastaveni: NastaveniInfo;
}

/**
 * Karty ovladačů v okně — ukládají se (OQ 52, rozhodl vlastník 6. 10.):
 * karta zůstane i bez kláves a po restartu, zmizí jen 🗑. Fáze 7: i to,
 * které jsou rozbalené (OQ 70). Část `klavesy` a odpověď příkazů
 * `pridej_kartu` (nová karta přijde rozbalená), `odeber_ovladac`
 * a `rozbal_kartu`.
 */
export interface KartyInfo {
	/** Pořadí změny — starší seznam (odpověď po novější) se zahodí. */
	rev: number;
	/** Ovladače od 0, vzestupně; 0 vždy a ovladač s klávesami taky. */
	pady: number[];
	/**
	 * Rozbalené karty — ovladače od 0, vzestupně, jen z `pady` (rozbalit
	 * jde víc i všechny, sbalit i ovladač 1). Zapnutý ovladač bez uložené
	 * karty se ukáže sbalený.
	 */
	rozbalene: number[];
}

/**
 * Volby z panelu ⓘ (Fáze 7 Z6). Odpověď příkazů `nastaveni`
 * a `nastav`, obsah události `nastaveni` (po změně z okna i z nabídky
 * ikony) a část `klavesy`. Zdrojem pravdy je backend — „✓ Zvuk" v nabídce
 * ikony a v okně ukazuje vždy totéž.
 */
export interface NastaveniInfo {
	/** Pořadí změny — starší (odpověď po novější události) se zahodí. */
	rev: number;
	/** Zvuk pozastavení a pokračování. */
	zvuk: boolean;
	/**
	 * Jedna klávesa pro víc vstupů: přiřazení klávesy, která už patří
	 * jinam, ji sdílí místo přesunu (nejvýš 4 vstupy). Backend ji přidá ke
	 * každému `prirad` sám — okno ji neposílá.
	 */
	sdilene_klavesy: boolean;
}

/** Argument `volba` příkazu `nastav` (`{ volba, zapnuto }` → `NastaveniInfo`). */
export type Volba = 'zvuk' | 'sdilene_klavesy';

// Příkazy bez vlastního typu (Fáze 7): `rozbal_kartu { pad, rozbalena }` →
// `KartyInfo`; `prirad_zkratku` → null (přiřazování zkratky, výsledek
// událostmi `rezim` a `oznameni`, ruší ho `zrus_prirazeni`); `ukazka_zvuku`
// → null, nebo chyba s důvodem (simulace, vypnutý zvuk) pro bublinu.
// Událost `okno-videt` (payload `boolean`): hlavní okno přestalo, nebo začalo
// být vidět (schované i minimalizované) — okno pak zruší potvrzovací dialog.

export type Rezim = 'disabled' | 'paused' | 'capturing' | 'binding' | 'no_hook';

/** Vstup konkrétního ovladače. */
export interface Cil {
	pad: number;
	vstup: Vstup;
}

/** Přiřazuje se zkratka pozastavení (ⓘ → Pauza, Fáze 7 Z6). */
export interface CilZkratky {
	zkratka: true;
}

/** Odpověď `rezim` i obsah události `rezim`. */
export interface RezimInfo {
	rezim: Rezim;
	/** Pořadí změny — starší (odpověď po novější události) se zahodí. */
	seq: number;
	/** Co se právě přiřazuje (jen v `binding`): vstup, nebo zkratka pozastavení. */
	cil: Cil | CilZkratky | null;
	/** Windows nedovolily hook klávesnice. */
	hook_chyba: boolean;
}

/** Živý stav jednoho ovladače. */
export interface ZivyPad {
	/** Fyzicky držené vstupy (bity podle `KlavesyInfo.vstupy`). */
	drzi: number;
	/** Vstupy, které hra opravdu dostává (stav ve slotu padu). */
	hra: number;
	/** Výchylka levé páčky po SOCD: x, y ∈ {−1, 0, 1}, +y = nahoru. */
	l: [number, number];
	/** Totéž pro pravou páčku. */
	p: [number, number];
}

/** Odpověď `zive` i obsah události `zive`. */
export interface ZivaInfo {
	seq: number;
	pady: ZivyPad[];
}

/** Klávesa v oznámení — jen pozice; názvy dodá `klavesy`. */
export interface KlavesaOznameni {
	scan: number;
	e0: boolean;
}

/** Událost `oznameni` — co se stalo při přiřazování (a proč). */
export type Oznameni = {
	/** Pořadí oznámení (16 bitů, přetéká). */
	seq: number;
	/** Mezi tímhle a předchozím oznámením se nějaké ztratilo → načíst vše. */
	mezera: boolean;
} & (
	| {
			typ: 'ulozeno';
			/** Jen u zkratky (varianta níž); tady kvůli zúžení typu. */
			zkratka?: undefined;
			pad: number;
			vstup: Vstup;
			klavesa: KlavesaOznameni;
			/**
			 * Odkud se klávesa přesunula (i z jiného ovladače). U klávesy, která
			 * patřila víc vstupům, první z nich (podle ovladače a pořadí vstupů).
			 */
			odkud: Cil | null;
			/** Kolik dalších vstupů o klávesu přišlo (0–3, jen s `odkud`). */
			odkud_dalsi: number;
			/**
			 * Kolika dalším vstupům klávesa dál patří (0–3): se zapnutou volbou ty,
			 * se kterými se teď sdílí, a i bez ní ty, kterým patřila spolu s cílem
			 * už dřív (sdílená klávesa se nikomu nebere).
			 */
			sdileno: number;
	  }
	| {
			typ: 'ulozeno';
			/** Nová zkratka pozastavení (Fáze 7, Z6) — bez ovladače a vstupu. */
			zkratka: true;
			klavesa: KlavesaOznameni;
	  }
	| {
			typ: 'odmitnuto';
			duvod: DuvodOdmitnuti;
			klavesa: KlavesaOznameni;
	  }
	| { typ: 'zruseno'; duvod: 'esc' | 'cas' | 'okno' | 'vynuceno' }
	| { typ: 'zapni_ovladac' }
);

/**
 * Proč stisknutou klávesu nejde přiřadit (přiřazování čeká dál):
 * - `zkratka` — je to zkratka pauzy (u vstupu),
 * - `win`, `nejde` — Win patří Windows, klávesa bez scan kódu (AltGr…),
 * - `plno` (Fáze 7) — klávesa už ovládá 4 vstupy a sdílet ji nejde; kam
 *   patří, najde okno ve `vazby`,
 * - `namapovana` (Z6, zkratka) — klávesa ovládá vstup (najde ve `vazby`),
 * - `nevhodna` (Z6, zkratka) — není F1–F24 (bez F4), Scroll Lock ani Pause.
 */
export type DuvodOdmitnuti = 'zkratka' | 'win' | 'nejde' | 'plno' | 'namapovana' | 'nevhodna';

/** Argument `zmena` příkazu `uprav_klavesy`. */
export type ZmenaKlaves =
	| { typ: 'vyprazdnit'; pad: number; vstup: Vstup }
	| { typ: 'vychozi' }
	| { typ: 'zpet'; rev: number };

export type PadStav =
	/** ViGEmBus je, ovladač vypnutý — zapne ho přepínač. */
	| 'off'
	| 'connecting'
	| 'on'
	/** ViGEmBus v systému vůbec není — jen tady se nabízí instalace. */
	| 'bus_missing'
	/** ViGEmBus je nainstalovaný, ale neběží — `detail` je rada, co s tím. */
	| 'bus_not_running'
	| 'error';

/** Odpověď `pad_status`, položka `pady` i obsah události `pad-stav`. */
export interface PadInfo {
	/** Který ovladač (0–3). */
	pad: number;
	state: PadStav;
	/** Číslo hráče podle ViGEmBus — okno ho neukazuje (s víc pady lže). */
	player: number | null;
	detail: string;
	needs_update: boolean;
	/** Běží instalátor ViGEmBus — ovladač vypnutý, přepínač zablokovaný. */
	installer: boolean;
	seq: number;
}

/** Stránky, které smí okno otevřít (backend drží adresy). */
export type Odkaz = 'vigembus' | 'vigembus_releases';
