// Údaje o aplikaci (příkaz `app_info`): verze, log, credit ViGEmBus,
// jestli je kam okno schovat a jestli je po ruce instalátor.
//
// Jeden stav pro celé okno — titulek (křížek), „O aplikaci" i karta
// ovladače by jinak každý zvlášť ukazovaly jiná čísla.
import { vAplikaci, zavolej } from './tauri';

/** Odpověď příkazu `app_info` (src-tauri/src/main.rs). */
interface AppInfo {
	version: string;
	log_path: string;
	dev: boolean;
	tray: boolean;
	setup: boolean;
	vigembus_credit: string;
	vigembus_license: string;
	vigembus_url: string;
}

export const aplikace = $state({
	/** Neznámá hodnota = „—", nikdy vymyšlené číslo (WinSent DESIGN.md). */
	verze: '—',
	cestaLogu: '',
	/** Ikona v oznamovací oblasti existuje; `null` = zatím nevíme. */
	tray: null as boolean | null,
	/** Instalátor KeyPadu je v instalační složce — umí nainstalovat
	    i aktualizovat ViGEmBus. Bez něj jen ruční stažení. */
	setup: true,
	/** Credit, licence a adresa ViGEmBus — drží je `updater::vigembus`
	    (tytéž texty jako v instalátoru), okno žádnou vlastní kopii nemá. */
	vigembusCredit: '',
	vigembusLicence: '',
	vigembusAdresa: ''
});

/** Načte údaje (při startu a po návratu okna — ikona i instalátor se
    můžou mezitím změnit). */
export async function nactiAplikaci(): Promise<void> {
	if (!vAplikaci) return;
	try {
		const i = await zavolej<AppInfo>('app_info');
		aplikace.verze = i.version;
		aplikace.cestaLogu = i.log_path;
		aplikace.tray = i.tray;
		aplikace.setup = i.setup;
		aplikace.vigembusCredit = i.vigembus_credit;
		aplikace.vigembusLicence = i.vigembus_license;
		aplikace.vigembusAdresa = i.vigembus_url;
	} catch {
		// Backend neodpověděl — zůstanou výchozí hodnoty.
	}
}
