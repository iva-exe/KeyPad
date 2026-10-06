<script lang="ts">
	import { onMount } from 'svelte';
	import { nactiAplikaci } from './lib/aplikace.svelte';
	import { hlidejKlavesnici } from './lib/klavesnice';
	import {
		cilPrirazeni,
		klavesy,
		nactiVse,
		posledniOznameni,
		pridejKartu,
		prihlasKlavesy,
		rezim,
		zrusPrirazeni
	} from './lib/klavesy.svelte';
	import PadCard from './lib/PadCard.svelte';
	import { nactiPady, pady, prihlasPady } from './lib/pady.svelte';
	import PridatOvladac from './lib/PridatOvladac.svelte';
	import Sbernice from './lib/Sbernice.svelte';
	import Titlebar from './lib/Titlebar.svelte';
	import UpdateBanner from './lib/UpdateBanner.svelte';
	import { startUpdateChecks, updater } from './lib/updater.svelte';
	import { maBackend } from './lib/tauri';
	import { dalsiKarta, viditelneKarty } from './lib/vstupy';
	import { prihlasZive } from './lib/zive.svelte';

	// Jedno okno, žádné routování: KeyPad má jednu práci (klávesy →
	// virtuální ovladače) a všechno k ní má být vidět naráz. Co nejméně
	// textu — stav je v barvě a ikonách, podrobnosti v bublinách a v logu.
	//
	// Karty ovladačů jsou akordeon: rozbalená je vždy jedna (schéma
	// s klávesami), ostatní jen hlavička se stavem a přepínačem. Seznam
	// karet ukládá backend (OQ 52); vidět je i ovladač s klávesami
	// a zapnutý ovladač, ať nikdy nezmizí karta, která něco dělá.

	const karty = $derived(
		viditelneKarty(
			klavesy.vazby,
			pady.map((p) => p.state),
			klavesy.karty
		)
	);
	const dalsi = $derived(dalsiKarta(karty));

	let zvolena = $state(0);
	// Rozbalená karta zmizela (odebraný ovladač) → první.
	const rozbalena = $derived(karty.includes(zvolena) ? zvolena : (karty[0] ?? 0));

	// Běžící přiřazování zruší už stisk myši na hlavičce (`stiskMysi`) —
	// čepička by jinak pulzovala ve sbalené kartě, kde ji nikdo nevidí.
	function rozbal(pad: number): void {
		zvolena = pad;
	}

	function pridejDalsi(): void {
		// Zapamatovat předem: `dalsi` se po přidání hned přepočítá na
		// následující volný ovladač.
		const pad = dalsi;
		if (pad === null) return;
		void pridejKartu(pad);
		rozbal(pad);
	}

	/** Všechno, co se mohlo změnit, když okno nebylo vidět. */
	function nactiStav(): Promise<unknown> {
		return Promise.all([nactiPady(), nactiVse()]);
	}

	/**
	 * Klik mimo přiřazovanou čepičku přiřazování zruší („klik jinam",
	 * spec 1.4). Klik na jinou čepičku ne: ta začne přiřazovat sama a zrušení
	 * by jen navíc přepnulo režim tam a zpátky.
	 */
	function stiskMysi(e: PointerEvent): void {
		if (cilPrirazeni() === null) return;
		const cil = e.target instanceof Element ? e.target : null;
		if (cil?.closest('[data-vstup]')) return;
		zrusPrirazeni();
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

<svelte:window onpointerdowncapture={stiskMysi} />

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
			<PadCard {pad} rozbalena={pad === rozbalena} onrozbal={() => rozbal(pad)} />
		{/each}

		{#if dalsi !== null}
			<PridatOvladac pad={dalsi} onpridej={pridejDalsi} />
		{/if}
	</main>

	{#if updater.available}
		<UpdateBanner />
	{/if}
</div>

<style>
	.app {
		display: flex;
		flex-direction: column;
		height: 100%;
		/* Spodní okraj místo patičky — panel nesmí sahat k hraně okna. */
		padding-bottom: 10px;
	}

	/* Jeden obsahový panel (WinSent „Frame 5" bez sidebaru — v úzkém
	   okně by postranní navigace sebrala místo obsahu). Při čtyřech
	   kartách se posouvá panel, okno ne. */
	.panel {
		flex: 1;
		min-height: 0;
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
		margin: 0 10px;
		/* Vpravo místo pro posuvník (10 px, app.css) napořád: objeví se,
		   až se rozbalená karta nevejde, a schéma by jinak při rozbalení
		   poskočilo o šířku posuvníku. Vlevo i vpravo pak zůstává 12 px. */
		padding: 0.75rem 2px 0.75rem 0.75rem;
		scrollbar-gutter: stable;
		background: var(--panel);
		border: 1px solid var(--border);
		border-radius: var(--radius-lg);
		overflow-y: auto;
	}
</style>
