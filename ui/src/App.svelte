<script lang="ts">
	import { onMount } from 'svelte';
	import Footer from './lib/Footer.svelte';
	import PadCard from './lib/PadCard.svelte';
	import Titlebar from './lib/Titlebar.svelte';
	import UpdateBanner from './lib/UpdateBanner.svelte';
	import { startPad } from './lib/pad.svelte';
	import { startUpdateChecks, updater } from './lib/updater.svelte';

	// Jedno okno, žádné routování: KeyPad má jednu práci (přepnout
	// klávesnici na gamepad a zpátky) a všechno k ní má být vidět naráz.
	//
	// Stav z backendu teče událostmi Tauri (náhrada za egui
	// request_repaint z ROADMAP): zatím stav virtuálního padu, ve Fázi 4
	// přibude režim Klávesnice / Gamepad.

	onMount(() => {
		startPad();
		startUpdateChecks();
	});
</script>

<div class="app">
	<Titlebar />

	<main class="panel">
		<PadCard />

		<!-- Tichý placeholder (WinSent DESIGN.md kap. 8): co tu bude
		     a kdy — žádný předstíraný obsah. -->
		<div class="ph">
			<span class="label-tech">// přepínání</span>
			<!-- Pevné mezery drží „klávesnice ↔ gamepad" pohromadě — šipka
			     na začátku řádku by vypadala jako odrážka. -->
			<p>Přepínání klávesnice&nbsp;↔&nbsp;gamepad přijde v&nbsp;další fázi.</p>
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
		display: flex;
		flex-direction: column;
		gap: 1rem;
		margin: 0 10px;
		padding: 1rem;
		background: var(--panel);
		border: 1px solid var(--border);
		border-radius: var(--radius-lg);
		overflow-y: auto;
	}

	.ph {
		flex: 1;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 0.5rem;
		min-height: 6rem;
		text-align: center;
	}
	.ph p {
		margin: 0;
		max-width: 32ch;
		color: var(--text-faint);
		font-size: var(--fs-md);
		text-wrap: balance;
	}
</style>
