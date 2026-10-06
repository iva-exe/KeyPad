<script lang="ts">
	import { onMount } from 'svelte';
	import { nactiAplikaci } from './lib/aplikace.svelte';
	import { escRusiPrirazeni, hlidejKlavesnici } from './lib/klavesnice';
	import {
		klavesy,
		nactiVse,
		posledniOznameni,
		pridejKartu,
		prihlasKlavesy,
		prirazuje,
		rezim,
		zrusPrirazeni
	} from './lib/klavesy.svelte';
	import PadCard from './lib/PadCard.svelte';
	import { nactiPady, pady, prihlasPady } from './lib/pady.svelte';
	import Potvrzeni from './lib/Potvrzeni.svelte';
	import { potvrzeni, zavriPotvrzeni } from './lib/dialog.svelte';
	import PridatOvladac from './lib/PridatOvladac.svelte';
	import Sbernice from './lib/Sbernice.svelte';
	import Titlebar from './lib/Titlebar.svelte';
	import UpdateBanner from './lib/UpdateBanner.svelte';
	import { startUpdateChecks, updater } from './lib/updater.svelte';
	import { maBackend, poslouchej } from './lib/tauri';
	import { dalsiKarta, viditelneKarty } from './lib/vstupy';
	import { prihlasZive } from './lib/zive.svelte';

	// Jedno okno, žádné routování: KeyPad má jednu práci (klávesy →
	// virtuální ovladače) a všechno k ní má být vidět naráz. Co nejméně
	// textu — stav je v barvě a ikonách, podrobnosti v bublinách a v logu.
	//
	// Karty ovladačů se rozbalují každá zvlášť — rozbalená může být víc
	// i všechny (Fáze 7, Z2; dřív akordeon). Seznam karet i jejich
	// rozbalení ukládá backend (OQ 52, 70); vidět je i ovladač s klávesami
	// a zapnutý ovladač, ať nikdy nezmizí karta, která něco dělá.

	const karty = $derived(
		viditelneKarty(
			klavesy.vazby,
			pady.map((p) => p.state),
			klavesy.karty
		)
	);
	const dalsi = $derived(dalsiKarta(karty));

	function pridejDalsi(): void {
		// Zapamatovat předem: `dalsi` se po přidání hned přepočítá na
		// následující volný ovladač. Nová karta přijde rozbalená.
		const pad = dalsi;
		if (pad === null) return;
		void pridejKartu(pad);
	}

	/** Všechno, co se mohlo změnit, když okno nebylo vidět. */
	function nactiStav(): Promise<unknown> {
		return Promise.all([nactiPady(), nactiVse()]);
	}

	/**
	 * Klik mimo přiřazovanou čepičku přiřazování zruší („klik jinam",
	 * spec 1.4). Klik na jinou čepičku ne: ta začne přiřazovat sama a zrušení
	 * by jen navíc přepnulo režim tam a zpátky — stejně čepička zkratky
	 * pozastavení v ⓘ (Z6). Zrušit sbalením karty z klávesnice umí karta.
	 */
	function stiskMysi(e: PointerEvent): void {
		if (!prirazuje()) return;
		const cil = e.target instanceof Element ? e.target : null;
		if (cil?.closest('[data-vstup], [data-zkratka]')) return;
		zrusPrirazeni();
	}

	/**
	 * Esc, který došel až do okna, přiřazování zruší — hook ho buď
	 * nedostal (OQ 60), nebo propustil jako vstříknutý (SendInput);
	 * skutečný Esc živý hook spolkne a do okna nedojde. Autorepeat ne:
	 * zrušení už letí do backendu. Panel ⓘ se přitom nezavře (Z6).
	 */
	function klavesa(e: KeyboardEvent): void {
		if (!e.repeat && escRusiPrirazeni(e, prirazuje())) zrusPrirazeni();
	}

	onMount(() => {
		const konecStraze = hlidejKlavesnici();
		void nactiAplikaci();
		startUpdateChecks();
		if (maBackend) {
			// Nejdřív poslouchat, pak se zeptat: změna mezi dotazem
			// a přihlášením k události by se jinak ztratila. Starší
			// odpovědi zahodí `seq` a `rev`.
			void Promise.all([prihlasPady(), prihlasKlavesy(), prihlasZive()]).then(nactiStav);
			// Schované i minimalizované okno potvrzovací dialog zruší (Z3) —
			// minimalizace WebView2 neuspí, `visibilitychange` by nepřišlo.
			void poslouchej<boolean>('okno-videt', (videt) => {
				if (!videt) zavriPotvrzeni(false);
			});
		}
		// Schované okno má uspaný WebView a backend mu živý stav
		// neposílá; po návratu z oznamovací oblasti všechno znovu
		// (i ikona a instalace v „O aplikaci").
		const zpet = () => {
			if (document.hidden) return;
			void nactiAplikaci();
			if (maBackend) void nactiStav();
		};
		document.addEventListener('visibilitychange', zpet);
		return () => {
			konecStraze();
			document.removeEventListener('visibilitychange', zpet);
		};
	});
</script>

<svelte:window onpointerdowncapture={stiskMysi} onkeydown={klavesa} />

<!-- data-*: stav okna pro test na skryté ploše (B6) — režim a poslední
     oznámení přímo, bez čtení textů. -->
<div
	class="app"
	data-rezim={rezim.rezim}
	data-rev={klavesy.rev}
	data-oznameni={posledniOznameni.typ || undefined}
	data-oznameni-seq={posledniOznameni.seq}
>
	<Titlebar />

	<main class="panel">
		<Sbernice />

		{#each karty as pad (pad)}
			<PadCard {pad} rozbalena={klavesy.rozbalene.includes(pad)} />
		{/each}

		{#if dalsi !== null}
			<PridatOvladac pad={dalsi} onpridej={pridejDalsi} />
		{/if}
	</main>

	{#if updater.available}
		<UpdateBanner />
	{/if}
</div>

<!-- Bez {#key}: lokální přechod dialogu se přehraje, jen když blok, který
     ho vytvořil, už jednou běžel — `{#key}` vznikal zároveň s dialogem
     a přechod se nepřehrál nikdy (revize). Dialog vytváří tenhle {#if}:
     mezi dvěma otevřeními projde `druh` vždy přes null. -->
{#if potvrzeni.druh !== null}
	<Potvrzeni />
{/if}

<style>
	.app {
		display: flex;
		flex-direction: column;
		height: 100%;
		/* Spodní okraj místo patičky — panel nesmí sahat k hraně okna. */
		padding-bottom: 10px;
	}

	/* Jeden obsahový panel (WinSent „Frame 5" bez sidebaru — v úzkém
	   okně by postranní navigace sebrala místo obsahu). Při víc rozbalených
	   kartách se posouvá panel, okno ne — nic se neořízne (Z2). */
	.panel {
		flex: 1;
		min-height: 0;
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
		margin: 0 10px;
		/* Vpravo místo pro posuvník (10 px, app.css) napořád: objeví se,
		   až se rozbalené karty nevejdou, a schéma by jinak při rozbalení
		   poskočilo o šířku posuvníku. Panel ořezává na hraně svého vnitřku
		   (padding box) a mezera posuvníku leží vně — vpravo tedy 8 px, ať se
		   záře karty (dosah 8 px, Z5) neusekne; vlevo 12 px. */
		padding: 0.75rem 8px 0.75rem 0.75rem;
		scrollbar-gutter: stable;
		background: var(--panel);
		border: 1px solid var(--border);
		border-radius: var(--radius-lg);
		overflow-x: hidden;
		overflow-y: auto;
	}
</style>
