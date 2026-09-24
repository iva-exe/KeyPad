<script lang="ts">
	import { onMount } from 'svelte';
	import Footer from './lib/Footer.svelte';
	import Titlebar from './lib/Titlebar.svelte';
	import UpdateBanner from './lib/UpdateBanner.svelte';
	import { startUpdateChecks, updater } from './lib/updater.svelte';

	// Jedno okno, žádné routování: KeyPad má jednu práci (přepnout
	// klávesnici na gamepad a zpátky) a všechno k ní má být vidět naráz.
	//
	// Stav z backendu (režim, pad, chyby) sem časem poteče událostmi
	// Tauri — náhrada za egui request_repaint z ROADMAP. Zatím žádný není.

	onMount(() => {
		startUpdateChecks();
	});
</script>

<div class="app">
	<Titlebar />

	<main class="panel">
		<!-- Tichý placeholder (WinSent DESIGN.md kap. 8): co tu bude
		     a kdy — žádný předstíraný obsah. -->
		<div class="ph">
			<span class="label-tech">// KeyPad</span>
			<!-- Pevné mezery drží „klávesnice ↔ gamepad" pohromadě — šipka
			     na začátku řádku by vypadala jako odrážka. -->
			<p>Přepínání klávesnice&nbsp;↔&nbsp;gamepad přijde v&nbsp;dalších fázích.</p>
		</div>
	</main>

	{#if updater.available}
		<UpdateBanner />
	{/if}

	<Footer />
</div>

<style>
	.app {
		display: flex;
		flex-direction: column;
		height: 100%;
	}

	/* Jeden obsahový panel (WinSent „Frame 5" bez sidebaru — v úzkém
	   okně by postranní navigace sebrala místo obsahu). */
	.panel {
		flex: 1;
		min-height: 0;
		margin: 0 10px;
		padding: 1.25rem 1.4rem;
		background: var(--panel);
		border: 1px solid var(--border);
		border-radius: var(--radius-lg);
		overflow-y: auto;
	}

	.ph {
		height: 100%;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 0.6rem;
		text-align: center;
	}
	.ph p {
		margin: 0;
		max-width: 32ch;
		color: var(--text-faint);
		font-size: var(--fs-xl);
		text-wrap: balance;
	}
</style>
