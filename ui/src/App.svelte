<script lang="ts">
	import { onMount } from 'svelte';
	import PadCard from './lib/PadCard.svelte';
	import Titlebar from './lib/Titlebar.svelte';
	import UpdateBanner from './lib/UpdateBanner.svelte';
	import { nactiAplikaci } from './lib/aplikace.svelte';
	import { startPad } from './lib/pad.svelte';
	import { startUpdateChecks, updater } from './lib/updater.svelte';

	// Jedno okno, žádné routování: KeyPad má jednu práci (přepnout
	// klávesnici na gamepad a zpátky) a všechno k ní má být vidět naráz.
	// Co nejméně textu — podrobnosti jsou v bublinách a v logu.
	//
	// Stav z backendu teče událostmi Tauri (náhrada za egui
	// request_repaint z ROADMAP): zatím stav virtuálního padu, ve Fázi 4
	// přibude režim Klávesnice / Gamepad.

	onMount(() => {
		void nactiAplikaci();
		startPad();
		startUpdateChecks();
		// Po návratu okna z oznamovací oblasti: ikona i instalace se
		// mohly mezitím změnit.
		const zpet = () => {
			if (!document.hidden) void nactiAplikaci();
		};
		document.addEventListener('visibilitychange', zpet);
		return () => document.removeEventListener('visibilitychange', zpet);
	});
</script>

<div class="app">
	<Titlebar />

	<main class="panel">
		<PadCard />

		<!-- Tichý placeholder (WinSent DESIGN.md kap. 8): co tu bude —
		     žádný předstíraný obsah. -->
		<div class="ph">
			<span class="label-tech">// klávesy</span>
			<p>Přijde v další fázi.</p>
		</div>
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
		gap: 0.4rem;
		min-height: 6rem;
		text-align: center;
	}
	.ph p {
		margin: 0;
		color: var(--text-faint);
		font-size: var(--fs-sm);
	}
</style>
