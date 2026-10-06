// Smlouva okna s backendem (Fáze 6, spec B4 „Smlouva s oknem").
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
	/** Zkratka pauzy (Scroll Lock). */
	zkratka: Klavesa;
	/**
	 * Všechny vazby v pořadí `Mapping::bindings()`: běžné klávesy podle
	 * scan kódu, pak rozšířené (E0). Backend je tak nemusí řadit a okno
	 * z pořadí bere jen to, která klávesa vstupu je na čepičce první.
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
	/** Ovladače s kartou v okně (uložené v config.json, OQ 52). */
	karty: KartyInfo;
}

/**
 * Karty ovladačů v okně — ukládají se (OQ 52, rozhodl vlastník 6. 10.):
 * karta zůstane i bez kláves a po restartu, zmizí jen 🗑. Část `klavesy`
 * a odpověď příkazů `pridej_kartu` a `odeber_ovladac`.
 */
export interface KartyInfo {
	/** Pořadí změny — starší seznam (odpověď po novější) se zahodí. */
	rev: number;
	/** Ovladače od 0, vzestupně; 0 vždy a ovladač s klávesami taky. */
	pady: number[];
}

export type Rezim = 'disabled' | 'paused' | 'capturing' | 'binding' | 'no_hook';

/** Vstup konkrétního ovladače. */
export interface Cil {
	pad: number;
	vstup: Vstup;
}

/** Odpověď `rezim` i obsah události `rezim`. */
export interface RezimInfo {
	rezim: Rezim;
	/** Pořadí změny — starší (odpověď po novější události) se zahodí. */
	seq: number;
	/** Co se právě přiřazuje (jen v `binding`). */
	cil: Cil | null;
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
			pad: number;
			vstup: Vstup;
			klavesa: KlavesaOznameni;
			/** Odkud se klávesa přesunula (i z jiného ovladače). */
			odkud: Cil | null;
	  }
	| { typ: 'odmitnuto'; duvod: 'zkratka' | 'win' | 'nejde'; klavesa: KlavesaOznameni }
	| { typ: 'zruseno'; duvod: 'esc' | 'cas' | 'okno' | 'vynuceno' }
	| { typ: 'zapni_ovladac' }
);

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
