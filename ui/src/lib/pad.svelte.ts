// Stav virtuálního gamepadu pro celé okno.
//
// Zdroj pravdy je backend (pad vlákno, src-tauri/src/platform/windows/
// pad.rs). Sem teče událostí `pad-stav` při každé změně a příkazem
// `pad_status` při startu a po návratu okna z oznamovací oblasti.
// Odpovědi a události se můžou předběhnout — pořadí drží `seq`.
//
// Ovladač se připojuje JEN přepínačem (`pad_on`) — po startu, po
// probuzení i po aktualizaci je vypnutý (Fáze 2b). Po připojení backend
// hned zachytává jeho klávesy; zkratka (Scroll Lock) zachytávání
// pozastaví — to okno pozná z režimu (událost `rezim`, Fáze 4).
//
// Backend umí až 4 ovladače; okno zatím ukazuje první (další přinese
// editor kláves, Fáze 6) — události ostatních proto ignoruje.
import { poslouchej, textChyby, vAplikaci, zavolej } from './tauri';

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

/** Režim zachytávání kláves (událost `rezim`, příkaz `rezim`). */
export type Rezim =
	/** Žádný ovladač není zapnutý. */
	| 'disabled'
	/** Pozastaveno zkratkou — klávesy jdou do Windows. */
	| 'paused'
	/** Klávesy ovládají zapnuté ovladače. */
	| 'capturing'
	| 'binding'
	/** Ovladač je zapnutý, ale Windows nedovolily sledovat klávesnici. */
	| 'no_hook';

/** Odpověď `rezim` i obsah události `rezim`. */
interface RezimInfo {
	rezim: Rezim;
	/** Pořadí změny — starší (odpověď po novější události) se zahodí. */
	seq: number;
}

/** Odpověď `pad_status` i obsah události `pad-stav`. */
interface PadInfo {
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

export const pad = $state({
	state: 'off' as PadStav,
	/** Podrobnost od backendu (proč chyba, rada) — do bubliny. */
	detail: '',
	/** ViGEmBus je starší než z posledního vydání. */
	stary: false,
	/** Pořadí poslední převzaté změny; −1 = zatím nic. */
	seq: -1,
	/** Přepínač čeká na odpověď backendu — ukazuje, co uživatel chce. */
	prepina: false,
	/** Kam uživatel přepínač přepnul (platí jen s `prepina`). */
	chce: false,
	/** Páčka zrovna opisuje kruh — tlačítko je chvíli zablokované. */
	testuje: false,
	/** Spouští se instalátor ViGEmBus (příkaz ještě neodpověděl). */
	spoustiInstalator: false,
	/** Instalátor běží — hlásí backend (pad vlákno), dokud instalátor
	    neskončí; po jeho konci backend ovladač sám ověří. */
	instalatorBezi: false,
	/** Chyba posledního kliknutí — krátká věta u tlačítek. */
	chybaAkce: '',
	/** Režim zachytávání (zdroj pravdy je engine v backendu). */
	rezim: 'disabled' as Rezim,
	/** Pořadí poslední převzaté změny režimu; −1 = zatím nic. */
	rezimSeq: -1
});

function prevezmiRezim(r: RezimInfo): void {
	if (r.seq < pad.rezimSeq) return;
	pad.rezim = r.rezim;
	pad.rezimSeq = r.seq;
}

/** Ovladač, který okno zatím ukazuje. */
const PRVNI = 0;

/** Je ovladač zapnutý (nebo se právě zapíná)? */
export function zapnuto(): boolean {
	return pad.state === 'on' || pad.state === 'connecting';
}

let casovacPrepnuti: ReturnType<typeof setTimeout> | undefined;

function prevezmi(i: PadInfo): void {
	// Každý ovladač má vlastní `seq` — cizí události do karty nepatří.
	if ((i.pad ?? PRVNI) !== PRVNI) return;
	// Starší změnu zahodit. Stejný `seq` projde: backend zapisuje
	// a čte celý stav najednou pod zámkem, takže stejný `seq` = stejný
	// obsah (odpověď `pad_status` po události se stejným číslem nic
	// nerozbije).
	if (i.seq < pad.seq) return;
	const nova = i.seq > pad.seq;
	pad.state = i.state;
	pad.detail = i.detail ?? '';
	pad.stary = !!i.needs_update;
	// Z backendu, ne odhadem okna: vypnutí padu před instalátorem se
	// může ohlásit dřív i později než odpověď `install_vigembus`.
	pad.instalatorBezi = !!i.installer;
	pad.seq = i.seq;
	if (nova) {
		// Backend odpověděl — přepínač zase ukazuje skutečnost.
		pad.prepina = false;
		clearTimeout(casovacPrepnuti);
	}
	if (i.state !== 'on') pad.testuje = false;
}

async function nacti(): Promise<void> {
	try {
		prevezmi(await zavolej<PadInfo>('pad_status', { pad: PRVNI }));
		prevezmiRezim(await zavolej<RezimInfo>('rezim'));
	} catch {
		// Mimo aplikaci backend není — zůstane „vypnuto".
	}
}

let spusteno = false;

/** Začne sledovat stav padu (idempotentní). */
export function startPad(): void {
	if (spusteno || !vAplikaci) return;
	spusteno = true;
	// Nejdřív poslouchat, pak se zeptat: změna mezi dotazem a přihlášením
	// k události by se jinak ztratila. Starší odpověď zahodí `seq`.
	void Promise.all([
		poslouchej<PadInfo>('pad-stav', prevezmi),
		poslouchej<RezimInfo>('rezim', prevezmiRezim)
	]).then(nacti);
	// Schované okno má uspaný webview; po návratu stav pro jistotu znovu.
	document.addEventListener('visibilitychange', () => {
		if (!document.hidden) void nacti();
	});
}

/**
 * Jak dlouho přepínač ukazuje přání uživatele, když backend mlčí.
 * Normálně odpoví do milisekund („zapínám…"); kdyby příkaz nic
 * nezměnil, přepínač se po chvíli vrátí ke skutečnosti.
 */
const PREPNUTI_MS = 2000;

/** Přepínač: zapnout / vypnout virtuální ovladač. */
export async function prepni(): Promise<void> {
	// Podle toho, co přepínač UKAZUJE (dvojklik před odpovědí backendu
	// je „zapnout a zase vypnout", ne dvakrát „zapnout").
	const chce = !(pad.prepina ? pad.chce : zapnuto());
	pad.chybaAkce = '';
	pad.prepina = true;
	pad.chce = chce;
	clearTimeout(casovacPrepnuti);
	casovacPrepnuti = setTimeout(() => (pad.prepina = false), PREPNUTI_MS);
	try {
		await zavolej(chce ? 'pad_on' : 'pad_off', { pad: PRVNI });
	} catch (e) {
		pad.chybaAkce = textChyby(e);
		pad.prepina = false;
	}
}

/** Jak dlouho blokovat „Vyzkoušet" — kruh trvá 1,2 s. */
const TEST_MS = 1300;

export async function vyzkousej(): Promise<void> {
	if (pad.testuje) return;
	pad.chybaAkce = '';
	pad.testuje = true;
	try {
		await zavolej('pad_test', { pad: PRVNI });
		setTimeout(() => (pad.testuje = false), TEST_MS);
	} catch (e) {
		pad.chybaAkce = textChyby(e);
		pad.testuje = false;
	}
}

export async function zkusZnovu(): Promise<void> {
	pad.chybaAkce = '';
	try {
		await zavolej('pad_retry');
	} catch (e) {
		pad.chybaAkce = textChyby(e);
	}
}

/**
 * Instalace chybějícího nebo aktualizace staršího ViGEmBus. Zapnutý
 * ovladač backend před spuštěním vypne; „instalátor běží" pak hlásí
 * událostí `pad-stav` (`installer`), až do konce instalátoru.
 */
export async function instalujOvladac(): Promise<void> {
	if (pad.spoustiInstalator || pad.instalatorBezi) return;
	pad.chybaAkce = '';
	pad.spoustiInstalator = true;
	try {
		await zavolej('install_vigembus');
	} catch (e) {
		pad.chybaAkce = textChyby(e);
	}
	pad.spoustiInstalator = false;
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
